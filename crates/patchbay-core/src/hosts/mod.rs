//! The host list and resolution of a host into a [`ConnectTarget`].
//!
//! Hosts come from three places:
//!
//! - `~/.ssh/config` and its includes: read-only, identified as `cfg:<alias>`.
//! - `~/.ssh/patchbay.conf` (see [`managed`]): the hosts created in Patchbay.
//!   They are ssh_config hosts too (`cfg:<alias>`), but editable. The file is
//!   meant to be included from `~/.ssh/config` so `ssh <alias>` works as well
//!   ([`HostStore::set_shared_with_ssh`]); until it is, Patchbay reads it as
//!   if it were included at the top.
//! - Devices on the user's tailnet ([`HostStore::set_tailnet`]), identified
//!   as `ts:<stable node id>`, unless a config host already points at them.
//!
//! Groups and tags of Patchbay hosts, the user last logged in as per
//! Tailscale device, and the ids of renamed hosts live in a schema-versioned
//! JSON file (`<config_dir>/patchbay/hosts.json`). Version 1 of that file
//! held the hosts themselves; they are moved to patchbay.conf on load.
//!
//! CONTRACT (used by the Tauri layer and `session`): keep these signatures stable.

mod managed;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::forward::config_rules;
use crate::model::{
    AgentSource, AuthMethods, ConnectTarget, Host, HostDraft, HostId, HostSource, PortForward,
    TailscaleInfo,
};
use crate::persist;
use crate::ssh_config::{LocalEnv, ResolveOverrides, ResolvedHost, SshConfig};
use crate::tailscale::{TailnetStatus, TailscaleDevice};
use crate::{Error, Result};

use managed::{Entry, ManagedConfig};

const SCHEMA_VERSION: u32 = 2;

/// OpenSSH's default identities, used when no `IdentityFile` applies.
const DEFAULT_IDENTITIES: &[&str] = &[
    "id_ed25519",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519_sk",
    "id_rsa",
];

/// A host as stored by schema version 1 (read for migration only).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredHost {
    id: HostId,
    name: String,
    hostname: String,
    port: Option<u16>,
    user: Option<String>,
    #[serde(default)]
    identity_files: Vec<PathBuf>,
    proxy_jump: Option<String>,
    group: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// How a Patchbay host is organized in the sidebar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

impl HostMeta {
    fn is_empty(&self) -> bool {
        self.group.is_none() && self.tags.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetaFile {
    version: u32,
    /// Schema 1: the hosts themselves.
    #[serde(default, skip_serializing)]
    hosts: Vec<StoredHost>,
    /// Group and tags of Patchbay hosts, by lower-cased alias.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    host_meta: BTreeMap<String, HostMeta>,
    /// Old id -> current id of renamed hosts, so references kept elsewhere
    /// (open tabs) can follow.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    renamed: BTreeMap<HostId, HostId>,
    /// User last logged in as, by Tailscale stable node id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tailscale_users: BTreeMap<String, String>,
}

/// Owner of the host list. Not internally synchronized; wrap in a lock when
/// shared.
pub struct HostStore {
    ssh_config_path: PathBuf,
    managed_path: PathBuf,
    meta_path: PathBuf,
    env: LocalEnv,
    config: SshConfig,
    managed: ManagedConfig,
    meta: MetaFile,
    /// `~/.ssh/config` includes patchbay.conf.
    shared_with_ssh: bool,
    tailnet: Option<TailnetStatus>,
    hosts: Vec<Host>,
    cfg_forwards: Vec<PortForward>,
    warnings: Vec<String>,
}

impl HostStore {
    /// Default locations: `~/.ssh/config`, `~/.ssh/patchbay.conf` and
    /// `<config_dir>/patchbay/hosts.json`.
    pub fn load_default() -> Result<Self> {
        let env = LocalEnv::detect();
        let config_dir = dirs::config_dir()
            .ok_or_else(|| Error::Other("cannot determine the configuration directory".into()))?;
        let ssh_config = env.ssh_dir().join("config");
        Self::load_with_env(
            ssh_config,
            config_dir.join("patchbay").join("hosts.json"),
            env,
        )
    }

    /// Explicit locations (tests). Missing files mean "no hosts".
    pub fn load(ssh_config: PathBuf, meta: PathBuf) -> Result<Self> {
        Self::load_with_env(ssh_config, meta, LocalEnv::detect())
    }

    /// Explicit locations and local environment (home directory, user name).
    /// patchbay.conf lives next to `ssh_config`.
    pub fn load_with_env(ssh_config: PathBuf, meta: PathBuf, env: LocalEnv) -> Result<Self> {
        let managed_path = ssh_config
            .parent()
            .unwrap_or(Path::new("."))
            .join(managed::FILE_NAME);
        let mut store = Self {
            config: SshConfig::empty(env.clone()),
            ssh_config_path: ssh_config,
            managed_path,
            meta_path: meta,
            env,
            managed: ManagedConfig::default(),
            meta: MetaFile::default(),
            shared_with_ssh: false,
            tailnet: None,
            hosts: Vec::new(),
            cfg_forwards: Vec::new(),
            warnings: Vec::new(),
        };
        store.reload()?;
        store.migrate()?;
        Ok(store)
    }

    /// Re-read everything from disk.
    pub fn reload(&mut self) -> Result<()> {
        let meta = read_meta(&self.meta_path)?;
        let managed = ManagedConfig::load(&self.managed_path)?;
        let plain = SshConfig::load_with_env(&self.ssh_config_path, self.env.clone())?;
        let shared = plain.includes(&self.managed_path);
        let config = if shared || !self.managed_path.exists() {
            plain
        } else {
            SshConfig::load_with_prelude(
                &self.ssh_config_path,
                self.env.clone(),
                &self.managed_path,
            )?
        };
        self.meta = meta;
        self.managed = managed;
        self.config = config;
        self.shared_with_ssh = shared;
        self.refresh();
        Ok(())
    }

    /// Non-fatal problems found while parsing ssh_config (unsupported
    /// keywords, `Match exec`, bad Include, ...), for display in the UI.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Config hosts in file order, then Patchbay hosts by group and name,
    /// then Tailscale devices by name.
    pub fn list(&self) -> Vec<Host> {
        self.hosts.clone()
    }

    /// Read-only port forward rules from the `LocalForward` /
    /// `RemoteForward` / `DynamicForward` options of ssh_config hosts (see
    /// [`crate::forward::config_rules`]), in config order.
    pub fn config_forwards(&self) -> &[PortForward] {
        &self.cfg_forwards
    }

    pub fn get(&self, id: &str) -> Option<Host> {
        self.hosts.iter().find(|h| h.id == id).cloned()
    }

    /// Old id -> current id of hosts renamed (or migrated) since.
    pub fn renamed(&self) -> &BTreeMap<HostId, HostId> {
        &self.meta.renamed
    }

    /// Where Patchbay hosts are written.
    pub fn managed_path(&self) -> &Path {
        &self.managed_path
    }

    /// Whether `~/.ssh/config` includes patchbay.conf, so `ssh` sees the
    /// hosts saved in Patchbay.
    pub fn shared_with_ssh(&self) -> bool {
        self.shared_with_ssh
    }

    /// Adds (or removes) the `Include patchbay.conf` line at the top of
    /// `~/.ssh/config`.
    pub fn set_shared_with_ssh(&mut self, share: bool) -> Result<()> {
        if share == self.shared_with_ssh {
            return Ok(());
        }
        if share {
            if !self.managed_path.exists() {
                self.managed.save(&self.managed_path)?;
            }
            managed::add_include(&self.ssh_config_path)?;
        } else {
            managed::remove_include(&self.ssh_config_path, &self.managed_path)?;
        }
        self.reload()
    }

    /// The tailnet as last seen (`None`: Tailscale not installed or not
    /// running). Its devices are listed unless a config host points at them.
    pub fn set_tailnet(&mut self, tailnet: Option<TailnetStatus>) {
        if self.tailnet != tailnet {
            self.tailnet = tailnet;
            self.refresh();
        }
    }

    pub fn create(&mut self, draft: HostDraft) -> Result<Host> {
        let (entry, meta) = validate(draft, &self.env.home)?;
        self.check_alias_free(&entry.alias, None)?;
        let mut managed = self.managed.clone();
        managed.upsert(None, &entry);
        managed.save(&self.managed_path)?;
        let id = format!("cfg:{}", entry.alias);
        self.meta.renamed.remove(&id);
        set_meta(&mut self.meta, &entry.alias, meta);
        write_meta(&self.meta_path, &self.meta)?;
        self.reload()?;
        self.get(&id).ok_or(Error::UnknownHost(id))
    }

    /// Saves a Patchbay host. Renaming it changes its id (`cfg:<alias>`);
    /// the old id is remembered in [`renamed`](Self::renamed) and ProxyJump
    /// references from other Patchbay hosts are updated. Errors with
    /// `ReadOnlyHost` for hosts that aren't Patchbay's.
    pub fn update(&mut self, id: &str, draft: HostDraft) -> Result<Host> {
        let old = self.editable(id)?.name;
        let (entry, meta) = validate(draft, &self.env.home)?;
        self.check_alias_free(&entry.alias, Some(&old))?;
        let mut managed = self.managed.clone();
        managed.upsert(Some(&old), &entry);
        let new_id = format!("cfg:{}", entry.alias);
        if new_id != id {
            for mut other in managed.entries() {
                if let Some(spec) = &other.proxy_jump
                    && let Some(renamed) = rename_hop(spec, &old, &entry.alias)
                {
                    other.proxy_jump = Some(renamed);
                    let alias = other.alias.clone();
                    managed.upsert(Some(&alias), &other);
                }
            }
        }
        managed.save(&self.managed_path)?;
        self.meta.host_meta.remove(&old.to_lowercase());
        set_meta(&mut self.meta, &entry.alias, meta);
        if new_id != id {
            for target in self.meta.renamed.values_mut() {
                if target == id {
                    target.clone_from(&new_id);
                }
            }
            self.meta.renamed.insert(id.to_owned(), new_id.clone());
            self.meta.renamed.remove(&new_id);
        }
        write_meta(&self.meta_path, &self.meta)?;
        self.reload()?;
        self.get(&new_id).ok_or(Error::UnknownHost(new_id))
    }

    /// Errors with `ReadOnlyHost` for hosts that aren't Patchbay's.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let alias = self.editable(id)?.name;
        let mut managed = self.managed.clone();
        managed.remove(&alias);
        managed.save(&self.managed_path)?;
        if self.meta.host_meta.remove(&alias.to_lowercase()).is_some() {
            write_meta(&self.meta_path, &self.meta)?;
        }
        self.reload()
    }

    /// Remembers `user` as the user to log in as on Tailscale device `id`
    /// (after a successful login), unless ssh_config already decides it.
    /// Returns whether anything changed.
    pub fn remember_tailscale_user(&mut self, id: &str, user: &str) -> Result<bool> {
        let Some(device) = self.device(id) else {
            return Ok(false);
        };
        let node = device.id.clone();
        if self.config_user(device).is_some()
            || self.meta.tailscale_users.get(&node).map(String::as_str) == Some(user)
        {
            return Ok(false);
        }
        self.meta.tailscale_users.insert(node, user.to_owned());
        write_meta(&self.meta_path, &self.meta)?;
        self.refresh();
        Ok(true)
    }

    /// Fully resolve a host for connecting, including its ProxyJump chain.
    pub fn resolve_target(&self, id: &HostId) -> Result<ConnectTarget> {
        self.resolve_target_as(id, None)
    }

    /// [`resolve_target`](Self::resolve_target), logging in as `user` (like
    /// `ssh -l`) when given. Tailscale devices without a configured or
    /// remembered user fail with [`Error::UserRequired`] unless `user` is
    /// given.
    pub fn resolve_target_as(&self, id: &HostId, user: Option<&str>) -> Result<ConnectTarget> {
        let overrides = ResolveOverrides {
            user: user.map(str::to_owned),
            port: None,
        };
        if id.starts_with("ts:") {
            return self.resolve_tailscale(id, &overrides);
        }
        let host = self
            .hosts
            .iter()
            .find(|h| &h.id == id)
            .ok_or_else(|| Error::UnknownHost(id.clone()))?;
        let resolved = self
            .config
            .resolve_with(&host.name, &self.env.user, &overrides);
        let mut visiting = vec![host.name.to_lowercase()];
        let jumps = self.expand_jumps(resolved.proxy_jump.as_deref(), &mut visiting)?;
        let mut target = self.target(&resolved, host.name.clone(), Some(id.clone()));
        target.jumps = jumps;
        Ok(target)
    }

    fn resolve_tailscale(
        &self,
        id: &HostId,
        overrides: &ResolveOverrides,
    ) -> Result<ConnectTarget> {
        let device = self
            .device(id)
            .ok_or_else(|| Error::UnknownHost(id.clone()))?;
        let name = device_alias(device);
        let mut resolved = self.config.resolve_with(&name, &self.env.user, overrides);
        if resolved.user.is_none() {
            resolved.user = self.meta.tailscale_users.get(&device.id).cloned();
        }
        if resolved.user.is_none() {
            return Err(Error::UserRequired(device.name.clone()));
        }
        // Connect to the Tailscale IP, so MagicDNS needn't work for us, but
        // check the host key under the name `ssh <name>` would use.
        if resolved.hostname.eq_ignore_ascii_case(&name)
            && let Some(ip) = device.ips.first()
        {
            resolved.hostname = ip.to_string();
            resolved.host_key_alias.get_or_insert(name.clone());
        }
        let mut visiting = vec![name.to_lowercase()];
        let jumps = self.expand_jumps(resolved.proxy_jump.as_deref(), &mut visiting)?;
        let mut target = self.target(&resolved, device.name.clone(), Some(id.clone()));
        target.jumps = jumps;
        target.trusted_host_keys = device.ssh_host_keys.clone();
        Ok(target)
    }

    fn device(&self, id: &str) -> Option<&TailscaleDevice> {
        let node = id.strip_prefix("ts:")?;
        self.tailnet.as_ref()?.devices.iter().find(|d| d.id == node)
    }

    /// The user ssh_config sets for a Tailscale device, if any.
    fn config_user(&self, device: &TailscaleDevice) -> Option<String> {
        self.config
            .resolve(&device_alias(device), &self.env.user)
            .user
    }

    /// The Patchbay host `id`, or why it can't be edited.
    fn editable(&self, id: &str) -> Result<Host> {
        match self.get(id) {
            Some(h) if h.source == HostSource::App => Ok(h),
            Some(_) => Err(Error::ReadOnlyHost(id.to_owned())),
            None => Err(Error::UnknownHost(id.to_owned())),
        }
    }

    /// Errors if a config host other than `except` is called `alias`.
    fn check_alias_free(&self, alias: &str, except: Option<&str>) -> Result<()> {
        let clash = self.config.host_aliases().iter().find(|a| {
            a.eq_ignore_ascii_case(alias) && !except.is_some_and(|e| a.eq_ignore_ascii_case(e))
        });
        match clash {
            Some(a) => Err(Error::Other(format!(
                "Another host is already called {a:?} in your SSH config"
            ))),
            None => Ok(()),
        }
    }

    /// Moves the hosts of a schema 1 file into patchbay.conf.
    fn migrate(&mut self) -> Result<()> {
        if self.meta.version >= SCHEMA_VERSION {
            return Ok(());
        }
        let old = std::mem::take(&mut self.meta.hosts);
        if !old.is_empty() {
            let mut managed = self.managed.clone();
            let mut taken: HashSet<String> = self
                .config
                .host_aliases()
                .iter()
                .map(|a| a.to_lowercase())
                .collect();
            for h in old {
                let base = sanitize_alias(&h.name);
                let alias = match managed.get(&base) {
                    // Left behind by an interrupted migration.
                    Some(e) if e.hostname.as_deref() == Some(h.hostname.as_str()) => e.alias,
                    _ => {
                        let alias = unique_alias(&base, &taken);
                        managed.upsert(
                            None,
                            &Entry {
                                alias: alias.clone(),
                                hostname: Some(h.hostname),
                                port: h.port,
                                user: h.user,
                                identity_files: h
                                    .identity_files
                                    .iter()
                                    .map(|p| portable_path(p, &self.env.home))
                                    .collect(),
                                proxy_jump: h.proxy_jump,
                            },
                        );
                        alias
                    }
                };
                taken.insert(alias.to_lowercase());
                set_meta(
                    &mut self.meta,
                    &alias,
                    HostMeta {
                        group: h.group,
                        tags: h.tags,
                    },
                );
                self.meta.renamed.insert(h.id, format!("cfg:{alias}"));
            }
            managed.save(&self.managed_path)?;
            let backup = self.meta_path.with_extension("v1.json");
            if let Err(e) = fs::copy(&self.meta_path, &backup) {
                tracing::warn!("could not back up {}: {e}", self.meta_path.display());
            }
            tracing::info!("moved Patchbay hosts to {}", self.managed_path.display());
        }
        if self.meta_path.exists() {
            self.meta.version = SCHEMA_VERSION;
            write_meta(&self.meta_path, &self.meta)?;
        }
        self.reload()
    }

    /// Recompute the host list and the warning list.
    fn refresh(&mut self) {
        let mut warnings: Vec<String> = self.config.warnings().to_vec();
        let mut push = |ws: Vec<String>| {
            for w in ws {
                if !warnings.contains(&w) {
                    warnings.push(w);
                }
            }
        };
        let entries = self.managed.entries();
        let mut cfg_hosts = Vec::new();
        let mut app_hosts = Vec::new();
        let mut cfg_forwards = Vec::new();
        // Names and addresses config hosts point at (to hide duplicate
        // Tailscale devices).
        let mut targets: HashSet<String> = HashSet::new();
        for alias in self.config.host_aliases() {
            let r = self.config.resolve(alias, &self.env.user);
            let (forwards, forward_warnings) = config_rules(alias, &r.forwards);
            cfg_forwards.extend(forwards);
            push(forward_warnings);
            targets.insert(alias.to_lowercase());
            targets.insert(r.hostname.trim_end_matches('.').to_lowercase());
            let entry = entries
                .iter()
                .find(|e| e.alias.eq_ignore_ascii_case(alias))
                .filter(|_| self.config.alias_file(alias) == Some(self.managed_path.as_path()));
            match entry {
                Some(e) => {
                    let meta = self
                        .meta
                        .host_meta
                        .get(&alias.to_lowercase())
                        .cloned()
                        .unwrap_or_default();
                    // Options as written (that's what the editor shows),
                    // except the port, which shows the effective one.
                    app_hosts.push(Host {
                        id: format!("cfg:{alias}"),
                        name: alias.clone(),
                        source: HostSource::App,
                        hostname: e.hostname.clone().unwrap_or_else(|| alias.clone()),
                        port: e.port.or(r.port).unwrap_or(22),
                        user: e.user.clone(),
                        identity_files: e.identity_files.iter().map(PathBuf::from).collect(),
                        proxy_jump: e.proxy_jump.clone(),
                        group: meta.group,
                        tags: meta.tags,
                        tailscale: None,
                    });
                }
                None => cfg_hosts.push(Host {
                    id: format!("cfg:{alias}"),
                    name: alias.clone(),
                    source: HostSource::SshConfig,
                    hostname: r.hostname,
                    port: r.port.unwrap_or(22),
                    user: r.user,
                    identity_files: r.identity_files,
                    proxy_jump: r.proxy_jump,
                    group: None,
                    tags: Vec::new(),
                    tailscale: None,
                }),
            }
            push(r.warnings);
        }
        app_hosts.sort_by_cached_key(|h| {
            (
                h.group.as_ref().map(|g| g.to_lowercase()),
                h.name.to_lowercase(),
            )
        });

        let mut ts_hosts = Vec::new();
        if let Some(tailnet) = &self.tailnet {
            for d in &tailnet.devices {
                let known = d
                    .dns_name
                    .iter()
                    .map(|n| n.to_lowercase())
                    .chain(d.ips.iter().map(|ip| ip.to_string()))
                    .any(|k| targets.contains(&k));
                if !known {
                    ts_hosts.push(self.tailscale_host(d));
                }
            }
        }

        self.hosts = cfg_hosts
            .into_iter()
            .chain(app_hosts)
            .chain(ts_hosts)
            .collect();
        self.cfg_forwards = cfg_forwards;
        self.warnings = warnings;
    }

    fn tailscale_host(&self, d: &TailscaleDevice) -> Host {
        let name = device_alias(d);
        let r = self.config.resolve(&name, &self.env.user);
        let remembered = self.meta.tailscale_users.get(&d.id);
        // Best guesses first: what worked here, what worked elsewhere, the
        // local user (what plain ssh would use), root on Unix-likes.
        let mut suggestions: Vec<String> = Vec::new();
        let mut suggest = |u: &str| {
            if !u.is_empty() && !suggestions.iter().any(|s| s == u) {
                suggestions.push(u.to_owned());
            }
        };
        if let Some(u) = remembered {
            suggest(u);
        }
        for u in self.meta.tailscale_users.values() {
            suggest(u);
        }
        suggest(&self.env.user);
        if matches!(
            d.os.to_lowercase().as_str(),
            "linux" | "freebsd" | "openbsd"
        ) {
            suggest("root");
        }
        Host {
            id: format!("ts:{}", d.id),
            name: d.name.clone(),
            source: HostSource::Tailscale,
            hostname: name,
            port: r.port.unwrap_or(22),
            user: r.user.or_else(|| remembered.cloned()),
            identity_files: r.identity_files,
            proxy_jump: r.proxy_jump,
            group: None,
            tags: Vec::new(),
            tailscale: Some(TailscaleInfo {
                online: d.online,
                os: d.os.clone(),
                dns_name: d.dns_name.clone(),
                ips: d.ips.iter().map(ToString::to_string).collect(),
                ssh: !d.ssh_host_keys.is_empty(),
                owner: d.owner.clone(),
                user_suggestions: suggestions,
            }),
        }
    }

    /// Fill in defaults for a resolved host. `jumps` is left empty.
    fn target(&self, r: &ResolvedHost, label: String, host_id: Option<HostId>) -> ConnectTarget {
        let ssh_dir = self.env.ssh_dir();
        let identity_files = if r.identity_files.is_empty() {
            DEFAULT_IDENTITIES
                .iter()
                .map(|n| ssh_dir.join(n))
                .filter(|p| p.is_file())
                .collect()
        } else {
            r.identity_files.clone()
        };
        ConnectTarget {
            host_id,
            label,
            hostname: r.hostname.clone(),
            port: r.port.unwrap_or(22),
            user: r.user.clone().unwrap_or_else(|| self.env.user.clone()),
            identity_files,
            identities_only: r.identities_only.unwrap_or(false),
            agent: r.identity_agent.clone().unwrap_or(AgentSource::Default),
            host_key_alias: r.host_key_alias.clone(),
            known_hosts_files: r
                .user_known_hosts_files
                .clone()
                .unwrap_or_else(|| vec![ssh_dir.join("known_hosts"), ssh_dir.join("known_hosts2")]),
            trusted_host_keys: Vec::new(),
            connect_timeout: r.connect_timeout,
            server_alive_interval: r.server_alive_interval,
            jumps: Vec::new(),
            auth_methods: auth_methods(r),
        }
    }

    /// Expand a ProxyJump spec into a flat hop list, first hop first.
    ///
    /// Mirrors OpenSSH, where `-J a,b` runs `ssh -J a b`: only the first hop's
    /// own ProxyJump is honored (recursively prepended); later hops are
    /// reached through the earlier ones. `visiting` holds the hosts whose
    /// jump chain is being expanded, to detect cycles.
    fn expand_jumps(
        &self,
        spec: Option<&str>,
        visiting: &mut Vec<String>,
    ) -> Result<Vec<ConnectTarget>> {
        let Some(spec) = spec.map(str::trim) else {
            return Ok(Vec::new());
        };
        if spec.is_empty() || spec.eq_ignore_ascii_case("none") {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (i, raw) in spec.split(',').enumerate() {
            let hop = parse_hop(raw)?;
            let key = hop.host.to_lowercase();
            if visiting.contains(&key) {
                return Err(Error::Other(format!(
                    "ProxyJump loop: {} -> {}",
                    visiting.join(" -> "),
                    hop.host
                )));
            }
            let overrides = ResolveOverrides {
                user: hop.user.clone(),
                port: hop.port,
            };
            let resolved = self
                .config
                .resolve_with(&hop.host, &self.env.user, &overrides);
            if i == 0 {
                visiting.push(key);
                let nested = self.expand_jumps(resolved.proxy_jump.as_deref(), visiting);
                visiting.pop();
                out.extend(nested?);
            }
            let host_id = self
                .config
                .host_aliases()
                .iter()
                .find(|a| a.eq_ignore_ascii_case(&hop.host))
                .map(|a| format!("cfg:{a}"));
            out.push(self.target(&resolved, raw.trim().to_owned(), host_id));
        }
        Ok(out)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Hop {
    user: Option<String>,
    host: String,
    port: Option<u16>,
}

/// Parse `[user@]host[:port]`, `[user@][v6addr]:port` or
/// `ssh://[user@]host[:port]`.
fn parse_hop(raw: &str) -> Result<Hop> {
    let bad = |why: &str| Error::Other(format!("invalid ProxyJump hop {:?}: {why}", raw.trim()));
    let mut rest = raw.trim();
    if rest.len() >= 6 && rest[..6].eq_ignore_ascii_case("ssh://") {
        rest = rest[6..].trim_end_matches('/');
    }
    let (user, hostport) = match rest.rsplit_once('@') {
        Some(("", _)) => return Err(bad("empty user")),
        Some((u, h)) => (Some(u.to_owned()), h),
        None => (None, rest),
    };
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let (host, after) = inner
            .split_once(']')
            .ok_or_else(|| bad("unterminated '['"))?;
        match after {
            "" => (host, None),
            _ => (
                host,
                Some(
                    after
                        .strip_prefix(':')
                        .ok_or_else(|| bad("expected ':' after ']'"))?,
                ),
            ),
        }
    } else if hostport.matches(':').count() == 1 {
        let (h, p) = hostport.split_once(':').unwrap_or((hostport, ""));
        (h, Some(p))
    } else {
        (hostport, None)
    };
    if host.is_empty() {
        return Err(bad("empty host"));
    }
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| bad("bad port"))
        })
        .transpose()?;
    Ok(Hop {
        user,
        host: host.to_owned(),
        port,
    })
}

/// The name ssh_config resolution uses for a Tailscale device: its MagicDNS
/// name (as in `ssh nas.tail1234.ts.net`), else its first IP.
fn device_alias(d: &TailscaleDevice) -> String {
    d.dns_name
        .clone()
        .or_else(|| d.ips.first().map(ToString::to_string))
        .unwrap_or_else(|| d.hostname.clone())
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn validate(draft: HostDraft, home: &Path) -> Result<(Entry, HostMeta)> {
    let alias = draft.name.trim().to_owned();
    managed::valid_alias(&alias).map_err(|e| Error::Other(e.into()))?;
    let hostname = draft.hostname.trim().to_owned();
    if hostname.is_empty() {
        return Err(Error::Other("Hostname must not be empty".into()));
    }
    if hostname.contains(char::is_whitespace) {
        return Err(Error::Other("Hostname must not contain spaces".into()));
    }
    if draft.port == Some(0) {
        return Err(Error::Other("Port must be between 1 and 65535".into()));
    }
    let user = non_empty(draft.user);
    if user
        .as_deref()
        .is_some_and(|u| u.contains(char::is_whitespace))
    {
        return Err(Error::Other("User must not contain spaces".into()));
    }
    let proxy_jump = non_empty(draft.proxy_jump);
    if let Some(spec) = &proxy_jump
        && !spec.eq_ignore_ascii_case("none")
    {
        for hop in spec.split(',') {
            parse_hop(hop)?;
        }
    }
    let mut tags: Vec<String> = Vec::new();
    for t in draft.tags {
        let t = t.trim().to_owned();
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    }
    let entry = Entry {
        alias,
        hostname: Some(hostname),
        port: draft.port,
        user,
        identity_files: draft
            .identity_files
            .iter()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| portable_path(p, home))
            .collect(),
        proxy_jump,
    };
    let meta = HostMeta {
        group: non_empty(draft.group),
        tags,
    };
    Ok((entry, meta))
}

/// `~/...` for paths in the home directory (so the file works on other
/// machines too), else the path as given.
fn portable_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if path.is_absolute() => {
            let parts: Vec<_> = rest
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            format!("~/{}", parts.join("/"))
        }
        _ => path.to_string_lossy().into_owned(),
    }
}

/// A valid alias made from a schema 1 host name (which allowed a few
/// characters ssh_config doesn't).
fn sanitize_alias(name: &str) -> String {
    let mut alias: String = name
        .trim()
        .chars()
        .map(|c| match c {
            c if c.is_whitespace() || c.is_control() => '-',
            '*' | '?' | '!' | ',' | '"' | '\'' | '#' | '=' | '\\' | '@' => '-',
            c => c,
        })
        .collect();
    alias = alias.trim_start_matches('-').to_owned();
    if alias.is_empty() {
        alias = "host".into();
    }
    alias
}

/// `base`, or `base-2`, `base-3`, ... if taken (case-insensitively).
fn unique_alias(base: &str, taken: &HashSet<String>) -> String {
    let mut alias = base.to_owned();
    let mut n = 2;
    while taken.contains(&alias.to_lowercase()) {
        alias = format!("{base}-{n}");
        n += 1;
    }
    alias
}

/// `spec` with hops to `old` pointed at `new` instead (keeping user and
/// port), or `None` if no hop goes to `old`.
fn rename_hop(spec: &str, old: &str, new: &str) -> Option<String> {
    let mut changed = false;
    let hops: Vec<String> = spec
        .split(',')
        .map(|raw| match parse_hop(raw) {
            Ok(hop) if hop.host.eq_ignore_ascii_case(old) => {
                changed = true;
                let user = hop.user.map(|u| format!("{u}@")).unwrap_or_default();
                let port = hop.port.map(|p| format!(":{p}")).unwrap_or_default();
                format!("{user}{new}{port}")
            }
            _ => raw.trim().to_owned(),
        })
        .collect();
    changed.then(|| hops.join(","))
}

fn set_meta(file: &mut MetaFile, alias: &str, meta: HostMeta) {
    let key = alias.to_lowercase();
    if meta.is_empty() {
        file.host_meta.remove(&key);
    } else {
        file.host_meta.insert(key, meta);
    }
}

fn read_meta(path: &Path) -> Result<MetaFile> {
    let Some(text) = persist::read_optional(path)? else {
        return Ok(MetaFile {
            version: SCHEMA_VERSION,
            ..MetaFile::default()
        });
    };
    let file: MetaFile = serde_json::from_str(&text)?;
    if file.version > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "{} was written by a newer version of Patchbay (schema {}, supported {})",
            path.display(),
            file.version,
            SCHEMA_VERSION
        )));
    }
    Ok(file)
}

fn write_meta(path: &Path, meta: &MetaFile) -> Result<()> {
    let body = serde_json::to_vec_pretty(&MetaFile {
        version: SCHEMA_VERSION,
        ..meta.clone()
    })?;
    persist::write_atomic(path, &body)
}

/// Client-side authentication method switches from ssh_config.
fn auth_methods(r: &ResolvedHost) -> AuthMethods {
    let flag = |key: &str| -> Option<bool> {
        let value = r.options.get(key)?.first()?;
        match value.to_ascii_lowercase().as_str() {
            "yes" | "true" => Some(true),
            "no" | "false" => Some(false),
            _ => None,
        }
    };
    let mut methods = AuthMethods {
        publickey: flag("pubkeyauthentication").unwrap_or(true),
        keyboard_interactive: flag("kbdinteractiveauthentication")
            .or_else(|| flag("challengeresponseauthentication"))
            .unwrap_or(true),
        password: flag("passwordauthentication").unwrap_or(true),
    };
    // PreferredAuthentications is used as an allow-list; Patchbay keeps its own order.
    if let Some(preferred) = r
        .options
        .get("preferredauthentications")
        .and_then(|v| v.first())
    {
        let listed: Vec<String> = preferred
            .split(',')
            .map(|m| m.trim().to_ascii_lowercase())
            .collect();
        let has = |m: &str| listed.iter().any(|l| l == m);
        methods.publickey &= has("publickey");
        methods.keyboard_interactive &= has("keyboard-interactive");
        methods.password &= has("password");
    }
    methods
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    struct Fixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
        cfg: PathBuf,
        app: PathBuf,
        conf: PathBuf,
    }

    impl Fixture {
        fn new(config: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            fs::create_dir_all(home.join(".ssh")).unwrap();
            let cfg = home.join(".ssh/config");
            fs::write(&cfg, config).unwrap();
            let app = dir.path().join("conf/patchbay/hosts.json");
            let conf = home.join(".ssh/patchbay.conf");
            Self {
                _dir: dir,
                home,
                cfg,
                app,
                conf,
            }
        }

        fn env(&self) -> LocalEnv {
            LocalEnv {
                home: self.home.clone(),
                user: "local".into(),
                hostname: "laptop".into(),
                uid: None,
            }
        }

        fn store(&self) -> HostStore {
            HostStore::load_with_env(self.cfg.clone(), self.app.clone(), self.env()).unwrap()
        }
    }

    fn draft(name: &str, hostname: &str) -> HostDraft {
        HostDraft {
            name: name.into(),
            hostname: hostname.into(),
            ..HostDraft::default()
        }
    }

    #[test]
    fn lists_config_then_sorted_app_hosts() {
        let fx = Fixture::new(
            "Host web db\n  HostName %h.example.com\n  User ops\nHost *\n  Port 2222\n  IdentityFile ~/.ssh/k\n",
        );
        let mut store = fx.store();
        store
            .create(HostDraft {
                group: Some("prod".into()),
                ..draft("zeta", "z.example")
            })
            .unwrap();
        store.create(draft("Beta", "b.example")).unwrap();
        store.create(draft("alpha", "a.example")).unwrap();
        store
            .create(HostDraft {
                group: Some("prod".into()),
                port: Some(22),
                ..draft("eta", "e.example")
            })
            .unwrap();

        let list = store.list();
        let names: Vec<_> = list.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["web", "db", "alpha", "Beta", "eta", "zeta"]);
        let web = &list[0];
        assert_eq!(web.id, "cfg:web");
        assert_eq!(web.source, HostSource::SshConfig);
        assert_eq!(web.hostname, "web.example.com");
        assert_eq!(web.port, 2222);
        assert_eq!(web.user.as_deref(), Some("ops"));
        assert_eq!(web.identity_files, vec![fx.home.join(".ssh/k")]);
        assert_eq!(list[2].port, 2222);
        assert_eq!(list[4].port, 22);
        assert_eq!(list[2].id, "cfg:alpha");
        assert_eq!(list[2].source, HostSource::App);
        assert_eq!(store.get("cfg:db").unwrap().hostname, "db.example.com");
    }

    #[test]
    fn crud_persists_atomically() {
        let fx = Fixture::new("");
        let mut store = fx.store();
        let h = store
            .create(HostDraft {
                user: Some("  ".into()),
                tags: vec!["a".into(), " a ".into(), "b".into()],
                identity_files: vec![fx.home.join(".ssh/id_box")],
                ..draft(" box ", "box.lan")
            })
            .unwrap();
        assert_eq!(h.id, "cfg:box");
        assert_eq!(h.name, "box");
        assert_eq!(h.user, None);
        assert_eq!(h.tags, ["a", "b"]);
        assert_eq!(h.identity_files, [PathBuf::from("~/.ssh/id_box")]);

        let conf = fs::read_to_string(&fx.conf).unwrap();
        assert!(
            conf.ends_with("\nHost box\n    HostName box.lan\n    IdentityFile ~/.ssh/id_box\n"),
            "{conf}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fx.app).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["hostMeta"]["box"]["tags"][1], "b");
        let leftovers = |dir: &Path| fs::read_dir(dir).unwrap().count();
        assert_eq!(leftovers(fx.app.parent().unwrap()), 1);
        assert_eq!(leftovers(&fx.home.join(".ssh")), 2);

        let mut again = fx.store();
        assert_eq!(again.get(&h.id).unwrap(), h);
        let updated = again
            .update(
                &h.id,
                HostDraft {
                    port: Some(2200),
                    ..draft("box2", "box.lan")
                },
            )
            .unwrap();
        assert_eq!(updated.id, "cfg:box2");
        assert_eq!(updated.port, 2200);
        assert!(updated.tags.is_empty());
        assert_eq!(again.renamed().get("cfg:box").unwrap(), "cfg:box2");
        store.reload().unwrap();
        assert!(store.get(&h.id).is_none());
        assert_eq!(store.get("cfg:box2").unwrap().port, 2200);

        store.delete("cfg:box2").unwrap();
        assert!(fx.store().list().is_empty());
        assert!(matches!(
            store.delete("cfg:box2"),
            Err(Error::UnknownHost(_))
        ));
    }

    #[test]
    fn validation_and_read_only() {
        let fx = Fixture::new("Host cfg1\n");
        let mut store = fx.store();
        for bad in [
            draft("", "h"),
            draft("n", " "),
            draft("n", "a b"),
            HostDraft {
                port: Some(0),
                ..draft("n", "h")
            },
            HostDraft {
                proxy_jump: Some("a,@b".into()),
                ..draft("n", "h")
            },
            draft("a b", "h"),
            draft("web*", "h"),
            draft("-n", "h"),
            draft("CFG1", "h"),
        ] {
            assert!(matches!(store.create(bad), Err(Error::Other(_))));
        }
        assert!(!fx.app.exists());
        assert!(!fx.conf.exists());
        assert!(matches!(
            store.update("cfg:cfg1", draft("n", "h")),
            Err(Error::ReadOnlyHost(_))
        ));
        assert!(matches!(
            store.delete("cfg:cfg1"),
            Err(Error::ReadOnlyHost(_))
        ));
    }

    #[test]
    fn rejects_newer_schema() {
        let fx = Fixture::new("");
        fs::create_dir_all(fx.app.parent().unwrap()).unwrap();
        fs::write(&fx.app, r#"{"version": 99, "hosts": []}"#).unwrap();
        assert!(HostStore::load_with_env(fx.cfg.clone(), fx.app.clone(), fx.env()).is_err());
    }

    #[test]
    fn resolves_config_host_with_defaults() {
        let fx = Fixture::new("Host plain\n  HostName 10.0.0.5\n");
        fs::write(fx.home.join(".ssh/id_rsa"), "").unwrap();
        fs::write(fx.home.join(".ssh/id_ed25519"), "").unwrap();
        let store = fx.store();
        let t = store.resolve_target(&"cfg:plain".to_owned()).unwrap();
        assert_eq!(t.host_id.as_deref(), Some("cfg:plain"));
        assert_eq!(t.label, "plain");
        assert_eq!(t.hostname, "10.0.0.5");
        assert_eq!(t.port, 22);
        assert_eq!(t.user, "local");
        assert_eq!(
            t.identity_files,
            vec![fx.home.join(".ssh/id_ed25519"), fx.home.join(".ssh/id_rsa")]
        );
        assert!(!t.identities_only);
        assert_eq!(t.agent, AgentSource::Default);
        assert_eq!(
            t.known_hosts_files,
            vec![
                fx.home.join(".ssh/known_hosts"),
                fx.home.join(".ssh/known_hosts2")
            ]
        );
        assert!(t.jumps.is_empty());
        assert!(matches!(
            store.resolve_target(&"cfg:nope".to_owned()),
            Err(Error::UnknownHost(_))
        ));
    }

    #[test]
    fn resolves_config_host_options() {
        let fx = Fixture::new(
            "Host full\n  HostName full.example\n  Port 2022\n  User u\n  IdentityFile ~/.ssh/full\n\
             IdentitiesOnly yes\n  IdentityAgent none\n  HostKeyAlias fullkey\n\
             UserKnownHostsFile ~/.ssh/kh_%h\n  ConnectTimeout 7\n  ServerAliveInterval 15\n",
        );
        let t = fx.store().resolve_target(&"cfg:full".to_owned()).unwrap();
        assert_eq!(t.port, 2022);
        assert_eq!(t.user, "u");
        assert_eq!(t.identity_files, vec![fx.home.join(".ssh/full")]);
        assert!(t.identities_only);
        assert_eq!(t.agent, AgentSource::Disabled);
        assert_eq!(t.host_key_alias.as_deref(), Some("fullkey"));
        assert_eq!(
            t.known_hosts_files,
            vec![fx.home.join(".ssh/kh_full.example")]
        );
        assert_eq!(t.connect_timeout, Some(Duration::from_secs(7)));
        assert_eq!(t.server_alive_interval, Some(Duration::from_secs(15)));
    }

    #[test]
    fn app_host_falls_back_to_config() {
        // Like `ssh <alias>`: blocks match the alias, not the HostName.
        let fx = Fixture::new(
            "Host *.corp\n  User corpuser\nHost inh*\n  Port 2200\n\
             Host *\n  User fallback\n  IdentityFile ~/.ssh/shared\n  ServerAliveInterval 20\n",
        );
        let mut store = fx.store();
        let inherit = store.create(draft("inherit", "srv.corp")).unwrap();
        assert_eq!(inherit.port, 2200);
        let t = store.resolve_target(&inherit.id).unwrap();
        assert_eq!(t.label, "inherit");
        assert_eq!(t.host_id.as_deref(), Some("cfg:inherit"));
        assert_eq!(t.hostname, "srv.corp");
        assert_eq!(t.user, "fallback");
        assert_eq!(t.port, 2200);
        assert_eq!(t.identity_files, vec![fx.home.join(".ssh/shared")]);
        assert_eq!(t.server_alive_interval, Some(Duration::from_secs(20)));

        let explicit = store
            .create(HostDraft {
                user: Some("me".into()),
                port: Some(2),
                identity_files: vec!["~/.ssh/mine".into()],
                ..draft("inh2", "srv.corp")
            })
            .unwrap();
        let t = store.resolve_target(&explicit.id).unwrap();
        assert_eq!(t.user, "me");
        assert_eq!(t.port, 2);
        assert_eq!(
            t.identity_files,
            vec![fx.home.join(".ssh/mine"), fx.home.join(".ssh/shared")]
        );
        let t = store
            .resolve_target_as(&explicit.id, Some("other"))
            .unwrap();
        assert_eq!(t.user, "other");
    }

    #[test]
    fn migrates_schema_1_hosts_to_patchbay_conf() {
        let fx = Fixture::new("Host web\n  HostName web.example\n");
        fs::create_dir_all(fx.app.parent().unwrap()).unwrap();
        let v1 = serde_json::json!({
            "version": 1,
            "hosts": [
                {"id": "app:1", "name": "My box", "hostname": "box.lan", "port": 2200,
                 "user": "me", "identityFiles": [fx.home.join(".ssh/k")], "proxyJump": "web",
                 "group": "Home", "tags": ["nas"]},
                {"id": "app:2", "name": "web", "hostname": "other.example", "port": null,
                 "user": null, "proxyJump": null, "group": null},
            ]
        });
        fs::write(&fx.app, v1.to_string()).unwrap();

        let store = fx.store();
        let names: Vec<_> = store.list().iter().map(|h| h.id.clone()).collect();
        assert_eq!(names, ["cfg:web", "cfg:web-2", "cfg:My-box"]);
        let boxed = store.get("cfg:My-box").unwrap();
        assert_eq!(boxed.source, HostSource::App);
        assert_eq!((boxed.port, boxed.user.as_deref()), (2200, Some("me")));
        assert_eq!(boxed.identity_files, [PathBuf::from("~/.ssh/k")]);
        assert_eq!(boxed.group.as_deref(), Some("Home"));
        assert_eq!(boxed.tags, ["nas"]);
        let t = store.resolve_target(&boxed.id).unwrap();
        assert_eq!(t.jumps[0].hostname, "web.example");
        assert_eq!(store.renamed().get("app:1").unwrap(), "cfg:My-box");
        assert_eq!(store.renamed().get("app:2").unwrap(), "cfg:web-2");
        assert!(fx.app.with_extension("v1.json").exists());

        // Loading again doesn't migrate twice.
        let again = fx.store();
        assert_eq!(again.list(), store.list());
        let conf = fs::read_to_string(&fx.conf).unwrap();
        assert_eq!(conf.matches("Host ").count(), 2, "{conf}");
    }

    #[test]
    fn renames_follow_references() {
        let fx = Fixture::new("");
        let mut store = fx.store();
        store.create(draft("bastion", "b.example")).unwrap();
        let target = store
            .create(HostDraft {
                proxy_jump: Some("u@bastion:2,x".into()),
                ..draft("target", "t.example")
            })
            .unwrap();
        store
            .update("cfg:bastion", draft("gw", "b.example"))
            .unwrap();
        let jump = |store: &HostStore| store.get(&target.id).unwrap().proxy_jump.unwrap();
        assert_eq!(jump(&store), "u@gw:2,x");
        store.update("cfg:gw", draft("gw2", "b.example")).unwrap();
        assert_eq!(jump(&store), "u@gw2:2,x");
        let renamed: Vec<_> = store
            .renamed()
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(renamed, [("cfg:bastion", "cfg:gw2"), ("cfg:gw", "cfg:gw2")]);
        // A new host with an old name is not redirected.
        store.create(draft("bastion", "new.example")).unwrap();
        assert!(!store.renamed().contains_key("cfg:bastion"));
        // Changing only the case is a rename too.
        let h = store.update("cfg:gw2", draft("GW2", "b.example")).unwrap();
        assert_eq!(h.id, "cfg:GW2");
        assert_eq!(jump(&store), "u@GW2:2,x");
    }

    #[test]
    fn hand_edits_to_patchbay_conf_are_kept() {
        let fx = Fixture::new("");
        fs::write(
            &fx.conf,
            "# notes\nHost *.lan\n  User lan\n\nHost box\n  HostName box.lan\n  ForwardAgent yes\n",
        )
        .unwrap();
        let mut store = fx.store();
        let ids: Vec<_> = store.list().iter().map(|h| h.id.clone()).collect();
        assert_eq!(ids, ["cfg:box"]);
        store
            .update(
                "cfg:box",
                HostDraft {
                    user: Some("me".into()),
                    ..draft("box", "box.lan")
                },
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(&fx.conf).unwrap(),
            "# notes\nHost *.lan\n  User lan\n\nHost box\n    HostName box.lan\n    User me\n  \
             ForwardAgent yes\n"
        );
    }

    #[test]
    fn sharing_with_ssh_adds_the_include_line() {
        let fx = Fixture::new("Host web\n  User u\n");
        let mut store = fx.store();
        store.create(draft("box", "box.lan")).unwrap();
        assert!(!store.shared_with_ssh());
        let before = store.list();

        store.set_shared_with_ssh(true).unwrap();
        assert!(store.shared_with_ssh());
        let cfg = fs::read_to_string(&fx.cfg).unwrap();
        assert!(
            cfg.contains("\nInclude patchbay.conf\n\nHost web\n"),
            "{cfg}"
        );
        assert_eq!(store.list(), before);
        assert_eq!(store.get("cfg:box").unwrap().source, HostSource::App);
        assert!(fx.store().shared_with_ssh());

        store.set_shared_with_ssh(false).unwrap();
        assert!(!store.shared_with_ssh());
        assert_eq!(fs::read_to_string(&fx.cfg).unwrap(), "Host web\n  User u\n");
        assert_eq!(store.list(), before);
    }

    fn device(id: &str, name: &str) -> TailscaleDevice {
        TailscaleDevice {
            id: id.into(),
            name: name.into(),
            dns_name: Some(format!("{name}.tail1234.ts.net")),
            hostname: name.into(),
            os: "linux".into(),
            ips: vec!["100.64.0.1".parse().unwrap()],
            online: true,
            ssh_host_keys: vec!["ssh-ed25519 AAAA".into()],
            tags: Vec::new(),
            owner: None,
        }
    }

    fn tailnet(devices: Vec<TailscaleDevice>) -> Option<TailnetStatus> {
        Some(TailnetStatus {
            magic_dns_suffix: Some("tail1234.ts.net".into()),
            devices,
        })
    }

    #[test]
    fn tailscale_devices_need_a_user() {
        let fx = Fixture::new("");
        let mut store = fx.store();
        store.set_tailnet(tailnet(vec![device("n1", "nas")]));
        let nas = store.get("ts:n1").unwrap();
        assert_eq!(nas.source, HostSource::Tailscale);
        assert_eq!(nas.hostname, "nas.tail1234.ts.net");
        assert_eq!(nas.user, None);
        let info = nas.tailscale.unwrap();
        assert!(info.ssh && info.online);
        assert_eq!(info.user_suggestions, ["local", "root"]);

        assert!(matches!(
            store.resolve_target(&nas.id),
            Err(Error::UserRequired(name)) if name == "nas"
        ));
        let t = store.resolve_target_as(&nas.id, Some("pi")).unwrap();
        assert_eq!(t.user, "pi");
        assert_eq!(t.hostname, "100.64.0.1");
        assert_eq!(t.host_key_alias.as_deref(), Some("nas.tail1234.ts.net"));
        assert_eq!(t.trusted_host_keys, ["ssh-ed25519 AAAA"]);
        assert_eq!(t.label, "nas");

        assert!(store.remember_tailscale_user(&nas.id, "pi").unwrap());
        assert!(!store.remember_tailscale_user(&nas.id, "pi").unwrap());
        let mut again = fx.store();
        again.set_tailnet(tailnet(vec![device("n1", "nas"), device("n2", "pc")]));
        assert_eq!(again.get("ts:n1").unwrap().user.as_deref(), Some("pi"));
        assert_eq!(again.resolve_target(&nas.id).unwrap().user, "pi");
        let pc = again.get("ts:n2").unwrap().tailscale.unwrap();
        assert_eq!(pc.user_suggestions, ["pi", "local", "root"]);

        again.set_tailnet(None);
        assert!(again.get("ts:n1").is_none());
        assert!(matches!(
            again.resolve_target(&nas.id),
            Err(Error::UnknownHost(_))
        ));
    }

    #[test]
    fn ssh_config_decides_tailscale_users_and_duplicates() {
        let fx = Fixture::new(
            "Host *.ts.net\n  User admin\nHost mynas\n  HostName nas.tail1234.ts.net\n\
             Host pc.tail1234.ts.net\n  HostName pc.home\n",
        );
        let mut store = fx.store();
        store.set_tailnet(tailnet(vec![
            device("n1", "nas"),
            device("n2", "pc"),
            device("n3", "pi"),
        ]));
        let ids: Vec<_> = store
            .list()
            .into_iter()
            .filter(|h| h.source == HostSource::Tailscale)
            .map(|h| h.id)
            .collect();
        // nas: a config host points at it. pc: it is a config host itself.
        assert_eq!(ids, ["ts:n3"]);
        assert_eq!(store.get("ts:n3").unwrap().user.as_deref(), Some("admin"));
        assert!(!store.remember_tailscale_user("ts:n3", "other").unwrap());
        let t = store.resolve_target(&"ts:n3".to_owned()).unwrap();
        assert_eq!(t.user, "admin");
    }

    #[test]
    fn jump_chain_is_expanded_recursively() {
        let fx = Fixture::new(
            "Host target\n  HostName t.internal\n  ProxyJump bastion\n\
             Host bastion\n  HostName b.example\n  User ops\n  IdentityFile ~/.ssh/bastion\n  ProxyJump edge\n\
             Host edge\n  HostName e.example\n  Port 2222\n  ProxyJump none\n",
        );
        let store = fx.store();
        let t = store.resolve_target(&"cfg:target".to_owned()).unwrap();
        assert_eq!(t.hostname, "t.internal");
        let hops: Vec<_> = t
            .jumps
            .iter()
            .map(|j| (j.label.as_str(), j.hostname.as_str(), j.port))
            .collect();
        assert_eq!(
            hops,
            [("edge", "e.example", 2222), ("bastion", "b.example", 22)]
        );
        assert_eq!(t.jumps[1].user, "ops");
        assert_eq!(
            t.jumps[1].identity_files,
            vec![fx.home.join(".ssh/bastion")]
        );
        assert_eq!(t.jumps[1].host_id.as_deref(), Some("cfg:bastion"));
        assert!(t.jumps.iter().all(|j| j.jumps.is_empty()));
    }

    #[test]
    fn jump_list_semantics_and_overrides() {
        let fx = Fixture::new(
            "Host t\n  ProxyJump root@first:2200,ssh://u2@second:2201/,[fe80::1]:22\n\
             Host first\n  HostName f.example\n  User nobody\n  Port 1\n  ProxyJump zero\n\
             Host second\n  ProxyJump ignored\n",
        );
        let t = fx.store().resolve_target(&"cfg:t".to_owned()).unwrap();
        let hops: Vec<_> = t
            .jumps
            .iter()
            .map(|j| (j.hostname.as_str(), j.port, j.user.as_str()))
            .collect();
        assert_eq!(
            hops,
            [
                ("zero", 22, "local"),
                ("f.example", 2200, "root"),
                ("second", 2201, "u2"),
                ("fe80::1", 22, "local"),
            ]
        );
        assert_eq!(t.jumps[1].label, "root@first:2200");
        assert_eq!(t.jumps[3].host_id, None);
    }

    #[test]
    fn app_host_jump_explicit_and_none() {
        let fx = Fixture::new("Host gw\n  ProxyJump none\nHost v*\n  ProxyJump gw\n");
        let mut store = fx.store();
        let via = store.create(draft("via", "a.corp")).unwrap();
        let t = store.resolve_target(&via.id).unwrap();
        assert_eq!(t.jumps.len(), 1);
        assert_eq!(t.jumps[0].hostname, "gw");

        let direct = store
            .create(HostDraft {
                proxy_jump: Some("none".into()),
                ..draft("direct", "b.corp")
            })
            .unwrap();
        assert!(store.resolve_target(&direct.id).unwrap().jumps.is_empty());

        let other = store
            .create(HostDraft {
                proxy_jump: Some("x@elsewhere:2".into()),
                ..draft("other", "c.corp")
            })
            .unwrap();
        let t = store.resolve_target(&other.id).unwrap();
        assert_eq!(t.jumps.len(), 1);
        assert_eq!(
            (t.jumps[0].hostname.as_str(), t.jumps[0].port),
            ("elsewhere", 2)
        );
    }

    #[test]
    fn jump_cycles_are_errors() {
        let fx = Fixture::new(
            "Host a\n  ProxyJump b\nHost b\n  ProxyJump a\nHost self\n  ProxyJump self\n\
             Host star\nHost *\n  ProxyJump bastion\n",
        );
        let store = fx.store();
        for id in ["cfg:a", "cfg:self", "cfg:star"] {
            let err = store.resolve_target(&id.to_owned()).unwrap_err();
            assert!(err.to_string().contains("loop"), "{id}: {err}");
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(sanitize_alias(" a b,c "), "a-b-c");
        assert_eq!(sanitize_alias("--x"), "x");
        assert_eq!(sanitize_alias("*"), "host");
        let taken: HashSet<String> = ["a".into(), "a-2".into()].into();
        assert_eq!(unique_alias("A", &taken), "A-3");
        assert_eq!(rename_hop("a,u@A:1", "a", "b").as_deref(), Some("b,u@b:1"));
        assert_eq!(rename_hop("x,y", "a", "b"), None);
        let home = Path::new("/home/me");
        assert_eq!(
            portable_path(Path::new("/home/me/.ssh/k"), home),
            "~/.ssh/k"
        );
        assert_eq!(portable_path(Path::new("/etc/k"), home), "/etc/k");
        assert_eq!(portable_path(Path::new("~/k"), home), "~/k");
    }

    #[test]
    fn hop_parsing() {
        let hop = |s: &str| parse_hop(s).unwrap();
        assert_eq!(
            hop("u@h:1"),
            Hop {
                user: Some("u".into()),
                host: "h".into(),
                port: Some(1)
            }
        );
        assert_eq!(hop("a@b@h").user.as_deref(), Some("a@b"));
        assert_eq!(hop("::1").host, "::1");
        assert_eq!(hop("[::1]:2").port, Some(2));
        assert_eq!(hop("SSH://h/").host, "h");
        for bad in ["", "@h", "h:0", "h:x", "[::1", "u@"] {
            assert!(parse_hop(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn warnings_are_collected() {
        let fx =
            Fixture::new("Host p\n  ProxyCommand nc %h %p\nMatch exec true\n  Port 1\nFoo bar\n");
        let store = fx.store();
        let w = store.warnings();
        assert!(w.iter().any(|w| w.contains("ProxyCommand")));
        assert!(w.iter().any(|w| w.contains("Match exec")));
        assert!(w.iter().any(|w| w.contains("\"foo\"")));
    }

    #[test]
    fn config_forwards_are_derived() {
        let fx = Fixture::new(
            "Host web\n  LocalForward 8080 localhost:80\n  LocalForward 9 /run/sock\n\
             Host *\n  DynamicForward 1080\n",
        );
        let store = fx.store();
        let rules: Vec<_> = store
            .config_forwards()
            .iter()
            .map(|r| (r.id.as_str(), r.name.as_str(), r.host_id.as_str()))
            .collect();
        assert_eq!(
            rules,
            [
                ("cfg:web:1", "web: L 8080 → localhost:80", "cfg:web"),
                ("cfg:web:3", "web: D 1080", "cfg:web"),
            ]
        );
        assert!(
            store
                .warnings()
                .iter()
                .any(|w| w.contains("Host web: LocalForward #2 skipped"))
        );
    }

    #[test]
    fn auth_method_switches_from_ssh_config() {
        let f = Fixture::new(
            "Host nokeys\n  PubkeyAuthentication no\n\
             Host keysonly\n  PasswordAuthentication no\n  KbdInteractiveAuthentication no\n\
             Host preferred\n  PreferredAuthentications keyboard-interactive,password\n\
             Host plain\n",
        );
        let store = f.store();
        let methods = |alias: &str| {
            store
                .resolve_target(&format!("cfg:{alias}"))
                .unwrap()
                .auth_methods
        };
        assert_eq!(
            methods("nokeys"),
            AuthMethods {
                publickey: false,
                keyboard_interactive: true,
                password: true
            }
        );
        assert_eq!(
            methods("keysonly"),
            AuthMethods {
                publickey: true,
                keyboard_interactive: false,
                password: false
            }
        );
        assert_eq!(
            methods("preferred"),
            AuthMethods {
                publickey: false,
                keyboard_interactive: true,
                password: true
            }
        );
        assert_eq!(methods("plain"), AuthMethods::default());
    }
}
