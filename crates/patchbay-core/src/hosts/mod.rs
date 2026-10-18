//! The host list and resolution of a host into a [`ConnectTarget`].
//!
//! Hosts come from two places:
//!
//! - `~/.ssh/config` and its includes, identified as `cfg:<alias>`. Patchbay
//!   edits hosts in place when they have a block of their own (`Host
//!   <alias>`) in a file it can write (see [`config_file`]), and adds new
//!   ones to `~/.ssh/config`. If that file can't be written (say, a dotfile
//!   manager owns it), new hosts go to `~/.ssh/patchbay.conf` instead, which
//!   Patchbay reads as if it were included at the top.
//! - Devices on the user's tailnet ([`HostStore::set_tailnet`]), identified
//!   as `ts:<stable node id>`, unless a config host already points at them.
//!
//! Groups and tags of config hosts, the user last logged in as per
//! Tailscale device, and the ids of renamed hosts live in a schema-versioned
//! JSON file (`<config_dir>/patchbay/hosts.json`). Version 1 of that file
//! held the hosts themselves, and Patchbay 0.1 kept them in patchbay.conf
//! (included from `~/.ssh/config`); both are moved into `~/.ssh/config` on
//! load.
//!
//! CONTRACT (used by the Tauri layer and `session`): keep these signatures stable.

mod config_file;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::forward::config_rules;
use crate::model::{
    AgentSource, AuthMethods, ConfigInfo, ConnectTarget, Host, HostDraft, HostId, HostSource,
    PortForward, TailscaleInfo, WrittenOptions,
};
use crate::persist;
use crate::ssh_config::{LocalEnv, ResolveOverrides, ResolvedHost, SshConfig};
use crate::tailscale::{TailnetStatus, TailscaleDevice};
use crate::{Error, Result};

use config_file::{ConfigFile, Entry};

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

/// How a host is organized in the sidebar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

impl HostMeta {
    fn new(group: Option<String>, tags: Vec<String>) -> Self {
        let mut clean: Vec<String> = Vec::new();
        for t in tags {
            let t = t.trim().to_owned();
            if !t.is_empty() && !clean.contains(&t) {
                clean.push(t);
            }
        }
        Self {
            group: non_empty(group),
            tags: clean,
        }
    }

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
    /// Group and tags of config hosts, by lower-cased alias.
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

/// A config file with hosts in it, parsed for editing.
struct EditableFile {
    file: ConfigFile,
    /// Why the file can't be written, if it can't.
    problem: Option<String>,
}

/// Owner of the host list. Not internally synchronized; wrap in a lock when
/// shared.
pub struct HostStore {
    ssh_config_path: PathBuf,
    /// `~/.ssh/patchbay.conf`.
    fallback_path: PathBuf,
    meta_path: PathBuf,
    env: LocalEnv,
    config: SshConfig,
    /// Files that define hosts, by path as the parser saw it.
    files: HashMap<PathBuf, EditableFile>,
    /// `~/.ssh/config` includes patchbay.conf.
    fallback_included: bool,
    meta: MetaFile,
    tailnet: Option<TailnetStatus>,
    hosts: Vec<Host>,
    cfg_forwards: Vec<PortForward>,
    warnings: Vec<String>,
}

impl HostStore {
    /// Default locations: `~/.ssh/config` and
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
        let fallback_path = ssh_config
            .parent()
            .unwrap_or(Path::new("."))
            .join(config_file::LEGACY_FILE_NAME);
        let mut store = Self {
            config: SshConfig::empty(env.clone()),
            ssh_config_path: ssh_config,
            fallback_path,
            meta_path: meta,
            env,
            files: HashMap::new(),
            fallback_included: false,
            meta: MetaFile::default(),
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
        let plain = SshConfig::load_with_env(&self.ssh_config_path, self.env.clone())?;
        let included = plain.includes(&self.fallback_path);
        let config = if included || !self.fallback_path.exists() {
            plain
        } else {
            SshConfig::load_with_prelude(
                &self.ssh_config_path,
                self.env.clone(),
                &self.fallback_path,
            )?
        };
        let mut files = HashMap::new();
        for alias in config.host_aliases() {
            let Some(origin) = config.alias_origin(alias) else {
                continue;
            };
            if files.contains_key(&origin.file) {
                continue;
            }
            let path = origin.file.clone();
            let editable = match ConfigFile::load(&path) {
                Ok(file) => EditableFile {
                    file,
                    problem: config_file::write_problem(&path),
                },
                Err(e) => EditableFile {
                    file: ConfigFile::default(),
                    problem: Some(format!("the file can't be read ({e})")),
                },
            };
            files.insert(path, editable);
        }
        self.meta = meta;
        self.config = config;
        self.files = files;
        self.fallback_included = included;
        self.refresh();
        Ok(())
    }

    /// Non-fatal problems found while parsing ssh_config (unsupported
    /// keywords, `Match exec`, bad Include, ...), for display in the UI.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Config hosts in file order, then Tailscale devices.
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

    /// Where [`create`](Self::create) adds hosts: `~/.ssh/config`, or
    /// patchbay.conf if that can't be written.
    pub fn new_host_file(&self) -> PathBuf {
        if config_file::write_problem(&self.ssh_config_path).is_none() {
            self.ssh_config_path.clone()
        } else {
            self.fallback_path.clone()
        }
    }

    /// The tailnet as last seen (`None`: Tailscale not installed or not
    /// running). Its devices are listed unless a config host points at them.
    pub fn set_tailnet(&mut self, tailnet: Option<TailnetStatus>) {
        if self.tailnet != tailnet {
            self.tailnet = tailnet;
            self.refresh();
        }
    }

    /// Adds a host to the SSH config (see [`new_host_file`](Self::new_host_file)).
    pub fn create(&mut self, draft: HostDraft) -> Result<Host> {
        let (entry, meta) = validate(draft, &self.env.home)?;
        self.check_alias_free(&entry.alias, None)?;
        let path = self.new_host_file();
        let mut file = ConfigFile::load(&path)?;
        file.insert(&entry);
        file.save(&path)?;
        let id = format!("cfg:{}", entry.alias);
        self.meta.renamed.remove(&id);
        set_meta(&mut self.meta, &entry.alias, meta);
        write_meta(&self.meta_path, &self.meta)?;
        self.reload()?;
        self.get(&id).ok_or(Error::UnknownHost(id))
    }

    /// Saves a host, editing its block in place. Renaming it changes its id
    /// (`cfg:<alias>`); the old id is remembered in
    /// [`renamed`](Self::renamed) and ProxyJump references to it are
    /// updated. Errors with `ReadOnlyHost` for hosts Patchbay can't edit
    /// (see [`ConfigInfo::read_only`]).
    pub fn update(&mut self, id: &str, draft: HostDraft) -> Result<Host> {
        let (old, path) = self.editable(id)?;
        let (entry, meta) = validate(draft, &self.env.home)?;
        self.check_alias_free(&entry.alias, Some(&old))?;
        let new_id = format!("cfg:{}", entry.alias);
        let mut file = ConfigFile::load(&path)?;
        if !file.update(&old, &entry) {
            return Err(self.changed_on_disk(&path));
        }
        if new_id != id {
            file.rename_jumps(&old, &entry.alias);
        }
        file.save(&path)?;
        if new_id != id {
            self.rename_jumps_elsewhere(&path, &old, &entry.alias);
        }
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

    /// Sets the group and tags of any config host (read-only ones too: they
    /// are Patchbay's, not ssh's).
    pub fn organize(&mut self, id: &str, group: Option<String>, tags: Vec<String>) -> Result<Host> {
        let host = self
            .get(id)
            .filter(|h| h.source == HostSource::SshConfig)
            .ok_or_else(|| Error::UnknownHost(id.to_owned()))?;
        set_meta(&mut self.meta, &host.name, HostMeta::new(group, tags));
        write_meta(&self.meta_path, &self.meta)?;
        self.refresh();
        self.get(id)
            .ok_or_else(|| Error::UnknownHost(id.to_owned()))
    }

    /// Removes a host's block. Errors with `ReadOnlyHost` for hosts Patchbay
    /// can't edit.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let (alias, path) = self.editable(id)?;
        let mut file = ConfigFile::load(&path)?;
        if !file.remove(&alias) {
            return Err(self.changed_on_disk(&path));
        }
        file.save(&path)?;
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

    /// The alias of host `id` and the file its block is in, or why it can't
    /// be edited.
    fn editable(&self, id: &str) -> Result<(String, PathBuf)> {
        let host = self
            .get(id)
            .ok_or_else(|| Error::UnknownHost(id.to_owned()))?;
        match host.config {
            Some(c) if c.read_only.is_none() => Ok((host.name, c.file)),
            _ => Err(Error::ReadOnlyHost(id.to_owned())),
        }
    }

    fn changed_on_disk(&self, path: &Path) -> Error {
        Error::Other(format!(
            "{} changed on disk; reload the host list and try again",
            self.display(path)
        ))
    }

    /// Points ProxyJump hops to `old` at `new` in the other config files
    /// Patchbay can write. Best effort: the rename itself is done.
    fn rename_jumps_elsewhere(&self, done: &Path, old: &str, new: &str) {
        let mut seen: HashSet<PathBuf> = HashSet::from([config_file::follow(done)]);
        for path in self.config.files() {
            if !seen.insert(config_file::follow(path)) || config_file::write_problem(path).is_some()
            {
                continue;
            }
            let result = ConfigFile::load(path).and_then(|mut f| {
                if f.rename_jumps(old, new) {
                    f.save(path)?;
                }
                Ok(())
            });
            if let Err(e) = result {
                tracing::warn!("could not update ProxyJump in {}: {e}", path.display());
            }
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

    /// `~/...` for paths in the home directory.
    fn display(&self, path: &Path) -> String {
        portable_path(path, &self.env.home)
    }

    /// Moves hosts kept elsewhere by older versions into `~/.ssh/config`:
    /// patchbay.conf (Patchbay 0.1), then the hosts of a schema 1 file.
    fn migrate(&mut self) -> Result<()> {
        if self.fallback_path.exists()
            && config_file::write_problem(&self.ssh_config_path).is_none()
        {
            self.migrate_patchbay_conf()?;
            self.reload()?;
        }
        if self.meta.version >= SCHEMA_VERSION {
            return Ok(());
        }
        let old = std::mem::take(&mut self.meta.hosts);
        if !old.is_empty() {
            let path = self.new_host_file();
            let mut file = ConfigFile::load(&path)?;
            let mut taken: HashSet<String> = self
                .config
                .host_aliases()
                .iter()
                .map(|a| a.to_lowercase())
                .collect();
            for h in old {
                let base = sanitize_alias(&h.name);
                let alias = match file.get(&base) {
                    // Left behind by an interrupted migration.
                    Some(e) if e.hostname.as_deref() == Some(h.hostname.as_str()) => e.alias,
                    _ => {
                        let alias = unique_alias(&base, &taken);
                        file.insert(&Entry {
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
                        });
                        alias
                    }
                };
                taken.insert(alias.to_lowercase());
                set_meta(&mut self.meta, &alias, HostMeta::new(h.group, h.tags));
                self.meta.renamed.insert(h.id, format!("cfg:{alias}"));
            }
            file.save(&path)?;
            let backup = self.meta_path.with_extension("v1.json");
            if let Err(e) = fs::copy(&self.meta_path, &backup) {
                tracing::warn!("could not back up {}: {e}", self.meta_path.display());
            }
            tracing::info!("moved Patchbay hosts to {}", path.display());
        }
        if self.meta_path.exists() {
            self.meta.version = SCHEMA_VERSION;
            write_meta(&self.meta_path, &self.meta)?;
        }
        self.reload()
    }

    /// Moves the contents of patchbay.conf into `~/.ssh/config` (ahead of
    /// any `Host *`), drops the `Include` line Patchbay 0.1 added, and
    /// renames patchbay.conf to patchbay.conf.bak.
    fn migrate_patchbay_conf(&mut self) -> Result<()> {
        let conf = ConfigFile::load(&self.fallback_path)?;
        let mut main = ConfigFile::load(&self.ssh_config_path)?;
        let before = main.render();
        config_file::remove_legacy_include(&mut main, &self.fallback_path);
        // If a previous run stopped after writing ~/.ssh/config, the hosts
        // are there already.
        let plain = SshConfig::load_with_env(&self.ssh_config_path, self.env.clone())?;
        let moved = conf.entries().iter().any(|e| {
            plain
                .alias_origin(&e.alias)
                .is_some_and(|o| o.file != self.fallback_path)
        });
        if !moved {
            main.insert_lines(conf.without_legacy_header());
        }
        if main.render() != before {
            main.save(&self.ssh_config_path)?;
        }
        let backup = self.fallback_path.with_extension("conf.bak");
        fs::rename(&self.fallback_path, &backup)?;
        tracing::info!(
            "moved the hosts in {} to {}",
            self.fallback_path.display(),
            self.ssh_config_path.display()
        );
        Ok(())
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
        let mut cfg_hosts = Vec::new();
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
            let meta = self
                .meta
                .host_meta
                .get(&alias.to_lowercase())
                .cloned()
                .unwrap_or_default();
            cfg_hosts.push(Host {
                id: format!("cfg:{alias}"),
                name: alias.clone(),
                source: HostSource::SshConfig,
                hostname: r.hostname.clone(),
                port: r.port.unwrap_or(22),
                user: r.user.clone(),
                identity_files: r.identity_files.clone(),
                proxy_jump: r.proxy_jump.clone(),
                group: meta.group,
                tags: meta.tags,
                config: self.config_info(alias, &r),
                tailscale: None,
            });
            push(r.warnings);
        }

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

        self.hosts = cfg_hosts.into_iter().chain(ts_hosts).collect();
        self.cfg_forwards = cfg_forwards;
        self.warnings = warnings;
    }

    /// Where `alias` is defined, and whether (and what) Patchbay can edit.
    fn config_info(&self, alias: &str, r: &ResolvedHost) -> Option<ConfigInfo> {
        let origin = self.config.alias_origin(alias)?;
        let file = self.files.get(&origin.file);
        let entry = file.and_then(|f| f.file.get(alias));
        let read_only = if origin.patterns.len() > 1 {
            Some(format!(
                "It shares its Host line with other names (Host {}).",
                origin.patterns.join(" ")
            ))
        } else if origin.nested {
            Some("Its file is included from inside a Host or Match block.".to_owned())
        } else if let Some(problem) = file.and_then(|f| f.problem.as_ref()) {
            Some(format!(
                "{} can't be changed: {problem}.",
                self.display(&origin.file)
            ))
        } else if entry.is_none() {
            Some("Patchbay can't edit its Host line.".to_owned())
        } else {
            None
        };
        let written = match (&read_only, entry) {
            (None, Some(e)) => WrittenOptions {
                hostname: e.hostname,
                port: e.port,
                user: e.user,
                identity_files: e.identity_files,
                proxy_jump: e.proxy_jump,
            },
            _ => WrittenOptions::default(),
        };
        Some(ConfigInfo {
            overridden: overridden(&written, r),
            file: origin.file.clone(),
            line: origin.line,
            read_only,
            written,
            visible_to_ssh: origin.file != self.fallback_path || self.fallback_included,
        })
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
            config: None,
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
    config_file::valid_alias(&alias).map_err(|e| Error::Other(e.into()))?;
    let hostname = non_empty(Some(draft.hostname));
    if hostname
        .as_deref()
        .is_some_and(|h| h.contains(char::is_whitespace))
    {
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
    let entry = Entry {
        alias,
        hostname,
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
    Ok((entry, HostMeta::new(draft.group, draft.tags)))
}

/// Sentences about the `written` options ssh ignores because an earlier
/// block already set them (`r` is what ssh uses).
fn overridden(written: &WrittenOptions, r: &ResolvedHost) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |option: &str, mine: Option<&str>, used: Option<&str>| {
        // Values with %tokens or ${VARS} expand, so they can't be compared.
        if let Some(mine) = mine
            && !mine.contains(['%', '$'])
            && used != Some(mine)
        {
            out.push(match used {
                Some(used) => format!(
                    "ssh uses {option} {used} from an earlier block in your SSH config, not {mine}."
                ),
                None => format!(
                    "ssh ignores {option} here: an earlier block in your SSH config sets ProxyCommand."
                ),
            });
        }
    };
    check("HostName", written.hostname.as_deref(), Some(&r.hostname));
    check("User", written.user.as_deref(), r.user.as_deref());
    let port = r.port.unwrap_or(22).to_string();
    check(
        "Port",
        written.port.map(|p| p.to_string()).as_deref(),
        Some(&port),
    );
    check(
        "ProxyJump",
        written.proxy_jump.as_deref(),
        r.proxy_jump.as_deref(),
    );
    out
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

        fn config(&self) -> String {
            fs::read_to_string(&self.cfg).unwrap()
        }
    }

    fn draft(name: &str, hostname: &str) -> HostDraft {
        HostDraft {
            name: name.into(),
            hostname: hostname.into(),
            ..HostDraft::default()
        }
    }

    fn info(store: &HostStore, id: &str) -> ConfigInfo {
        store.get(id).unwrap().config.unwrap()
    }

    #[test]
    fn new_hosts_go_before_pattern_blocks() {
        let fx = Fixture::new(
            "Host web db\n  HostName %h.example.com\n  User ops\nHost *\n  Port 2222\n  IdentityFile ~/.ssh/k\n",
        );
        let mut store = fx.store();
        assert_eq!(store.new_host_file(), fx.cfg);
        store
            .create(HostDraft {
                group: Some("prod".into()),
                ..draft("zeta", "z.example")
            })
            .unwrap();
        store
            .create(HostDraft {
                port: Some(22),
                ..draft("eta", "e.example")
            })
            .unwrap();
        assert_eq!(
            fx.config(),
            "Host web db\n  HostName %h.example.com\n  User ops\n\nHost zeta\n  HostName z.example\n\n\
             Host eta\n  HostName e.example\n  Port 22\n\nHost *\n  Port 2222\n  IdentityFile ~/.ssh/k\n"
        );

        let list = store.list();
        let names: Vec<_> = list.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["web", "db", "zeta", "eta"]);
        let web = &list[0];
        assert_eq!(web.id, "cfg:web");
        assert_eq!(web.source, HostSource::SshConfig);
        assert_eq!(web.hostname, "web.example.com");
        assert_eq!(web.port, 2222);
        assert_eq!(web.user.as_deref(), Some("ops"));
        assert_eq!(web.identity_files, vec![fx.home.join(".ssh/k")]);
        let zeta = &list[2];
        assert_eq!((zeta.port, zeta.group.as_deref()), (2222, Some("prod")));
        assert_eq!(list[3].port, 22);
        let c = zeta.config.as_ref().unwrap();
        assert_eq!((c.file.as_path(), c.line), (fx.cfg.as_path(), 5));
        assert_eq!(c.read_only, None);
        assert_eq!(c.written.hostname.as_deref(), Some("z.example"));
        assert_eq!(c.written.port, None);
        assert!(c.visible_to_ssh);
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
        assert_eq!(h.identity_files, [fx.home.join(".ssh/id_box")]);
        assert_eq!(h.config.unwrap().written.identity_files, ["~/.ssh/id_box"]);
        assert_eq!(
            fx.config(),
            "Host box\n    HostName box.lan\n    IdentityFile ~/.ssh/id_box\n"
        );
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fx.app).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["hostMeta"]["box"]["tags"][1], "b");
        let leftovers = |dir: &Path| fs::read_dir(dir).unwrap().count();
        assert_eq!(leftovers(fx.app.parent().unwrap()), 1);
        assert_eq!(leftovers(&fx.home.join(".ssh")), 1);

        let mut again = fx.store();
        assert_eq!(again.get("cfg:box"), store.get("cfg:box"));
        let updated = again
            .update(
                "cfg:box",
                HostDraft {
                    port: Some(2200),
                    ..draft("box2", "")
                },
            )
            .unwrap();
        assert_eq!(updated.id, "cfg:box2");
        assert_eq!(updated.port, 2200);
        assert_eq!(updated.hostname, "box2");
        assert!(updated.tags.is_empty());
        assert_eq!(again.renamed().get("cfg:box").unwrap(), "cfg:box2");
        assert_eq!(fx.config(), "Host box2\n    Port 2200\n");
        store.reload().unwrap();
        assert!(store.get("cfg:box").is_none());
        assert_eq!(store.get("cfg:box2").unwrap().port, 2200);

        store.delete("cfg:box2").unwrap();
        assert!(fx.store().list().is_empty());
        assert_eq!(fx.config(), "");
        assert!(matches!(
            store.delete("cfg:box2"),
            Err(Error::UnknownHost(_))
        ));
    }

    #[test]
    fn edits_keep_the_rest_of_the_file() {
        let text = "# My hosts\nHost web\n  # the old box\n  HostName web.old\n  ForwardAgent yes\n\n\
                    Host db\n  HostName db.lan\n";
        let fx = Fixture::new(text);
        let mut store = fx.store();
        let mut d = draft("web", "web.new");
        d.user = Some("me".into());
        store.update("cfg:web", d).unwrap();
        assert_eq!(
            fx.config(),
            "# My hosts\nHost web\n  # the old box\n  HostName web.new\n  ForwardAgent yes\n  User me\n\n\
             Host db\n  HostName db.lan\n"
        );
        store.delete("cfg:web").unwrap();
        assert_eq!(fx.config(), "# My hosts\n\nHost db\n  HostName db.lan\n");
    }

    #[test]
    fn read_only_hosts_say_why() {
        let fx = Fixture::new(
            "Include conf.d/*\nHost a b\n  User x\nHost c\n  Include nested\nMatch host d\n  User y\nHost d\n",
        );
        let ssh = fx.home.join(".ssh");
        fs::create_dir(ssh.join("conf.d")).unwrap();
        fs::write(ssh.join("conf.d/work"), "Host e\n  HostName e.corp\n").unwrap();
        fs::write(ssh.join("nested"), "Host f\n").unwrap();
        let mut store = fx.store();
        let reason = |id: &str| info(&store, id).read_only;
        assert!(reason("cfg:a").unwrap().contains("(Host a b)"));
        assert!(reason("cfg:b").is_some());
        assert_eq!(reason("cfg:c"), None);
        assert_eq!(reason("cfg:d"), None);
        // Hosts in included files are edited where they are.
        assert_eq!(reason("cfg:e"), None);
        assert_eq!(info(&store, "cfg:e").file, ssh.join("conf.d/work"));
        assert!(reason("cfg:f").unwrap().contains("included from inside"));
        assert_eq!(info(&store, "cfg:a").written, WrittenOptions::default());

        for id in ["cfg:a", "cfg:f"] {
            assert!(matches!(
                store.update(id, draft("x", "h")),
                Err(Error::ReadOnlyHost(_))
            ));
            assert!(matches!(store.delete(id), Err(Error::ReadOnlyHost(_))));
        }
        store.update("cfg:e", draft("e", "e2.corp")).unwrap();
        assert_eq!(
            fs::read_to_string(ssh.join("conf.d/work")).unwrap(),
            "Host e\n  HostName e2.corp\n"
        );

        // Groups and tags work for every config host.
        let a = store
            .organize("cfg:a", Some(" Work ".into()), vec!["x".into()])
            .unwrap();
        assert_eq!(
            (a.group.as_deref(), a.tags.as_slice()),
            (Some("Work"), &["x".to_owned()][..])
        );
        assert_eq!(
            fx.store().get("cfg:a").unwrap().group.as_deref(),
            Some("Work")
        );
        assert!(fx.config().starts_with("Include conf.d/*\nHost a b\n"));
    }

    #[cfg(unix)]
    #[test]
    fn read_only_config_uses_patchbay_conf() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new("Host web\n");
        fs::set_permissions(&fx.cfg, fs::Permissions::from_mode(0o444)).unwrap();
        if fs::OpenOptions::new().append(true).open(&fx.cfg).is_ok() {
            return; // root
        }
        let mut store = fx.store();
        assert!(
            info(&store, "cfg:web")
                .read_only
                .unwrap()
                .contains("read-only")
        );
        assert_eq!(store.new_host_file(), fx.conf);
        store.create(draft("box", "box.lan")).unwrap();
        assert_eq!(fx.config(), "Host web\n");
        let c = info(&store, "cfg:box");
        assert_eq!(c.file, fx.conf);
        assert_eq!(c.read_only, None);
        assert!(!c.visible_to_ssh);
        // Not moved while ~/.ssh/config can't be written.
        assert!(fx.store().get("cfg:box").is_some());
        assert!(fx.conf.exists());
    }

    #[test]
    fn validation() {
        let fx = Fixture::new("Host cfg1\n");
        let mut store = fx.store();
        for bad in [
            draft("", "h"),
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
        assert_eq!(fx.config(), "Host cfg1\n");
    }

    #[test]
    fn overridden_options_are_reported() {
        let fx = Fixture::new(
            "Host *.lan\n  User lanuser\nHost box.lan\n  User me\n  Port 2\n  HostName %h\n\
             Host other\n  User me\n",
        );
        let store = fx.store();
        let c = info(&store, "cfg:box.lan");
        assert_eq!(c.written.user.as_deref(), Some("me"));
        assert_eq!(
            c.overridden,
            ["ssh uses User lanuser from an earlier block in your SSH config, not me."]
        );
        assert!(info(&store, "cfg:other").overridden.is_empty());
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
    fn new_host_falls_back_to_patterns() {
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
        assert!(explicit.config.unwrap().overridden.is_empty());
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
    fn migrates_schema_1_hosts_to_ssh_config() {
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
        assert_eq!(names, ["cfg:web", "cfg:My-box", "cfg:web-2"]);
        let boxed = store.get("cfg:My-box").unwrap();
        assert_eq!((boxed.port, boxed.user.as_deref()), (2200, Some("me")));
        assert_eq!(boxed.identity_files, [fx.home.join(".ssh/k")]);
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
        assert_eq!(fx.config().matches("Host ").count(), 3, "{}", fx.config());
    }

    #[test]
    fn migrates_patchbay_conf_into_ssh_config() {
        let fx = Fixture::new(
            "# Added by Patchbay: lets ssh use the hosts saved in Patchbay.\nInclude patchbay.conf\n\n\
             Host web\n  User u\n\nHost *\n  ServerAliveInterval 5\n",
        );
        fs::write(
            &fx.conf,
            "# Hosts saved in Patchbay.\n\
             # You can edit this file: Patchbay only rewrites the HostName, User, Port,\n\
             # IdentityFile and ProxyJump lines of the hosts you change in the app.\n\n\
             Host box\n    HostName box.lan\n    ForwardAgent yes\n",
        )
        .unwrap();
        let store = fx.store();
        assert_eq!(
            fx.config(),
            "Host web\n  User u\n\nHost box\n    HostName box.lan\n    ForwardAgent yes\n\n\
             Host *\n  ServerAliveInterval 5\n"
        );
        assert!(!fx.conf.exists());
        assert!(fx.conf.with_extension("conf.bak").exists());
        let c = info(&store, "cfg:box");
        assert_eq!((c.file, c.read_only), (fx.cfg.clone(), None));
        assert_eq!(fx.store().list(), store.list());

        // Not included (the user said "not now"): moved all the same.
        let fx = Fixture::new("");
        fs::write(&fx.conf, "Host box\n    HostName box.lan\n").unwrap();
        fx.store();
        assert_eq!(fx.config(), "Host box\n    HostName box.lan\n");

        // An interrupted move (config written, file not renamed) isn't
        // repeated.
        let fx = Fixture::new("Host box\n    HostName box.lan\n");
        fs::write(&fx.conf, "Host box\n    HostName box.lan\n").unwrap();
        fx.store();
        assert_eq!(fx.config(), "Host box\n    HostName box.lan\n");
        assert!(!fx.conf.exists());
    }

    #[test]
    fn renames_follow_references() {
        let fx = Fixture::new("");
        let ssh = fx.home.join(".ssh");
        let mut store = fx.store();
        store.create(draft("bastion", "b.example")).unwrap();
        let target = store
            .create(HostDraft {
                proxy_jump: Some("u@bastion:2,x".into()),
                ..draft("target", "t.example")
            })
            .unwrap();
        fs::write(ssh.join("work"), "Host w\n  ProxyJump bastion\n").unwrap();
        let mut cfg = fx.config();
        cfg.insert_str(0, "Include work\n\n");
        fs::write(&fx.cfg, cfg).unwrap();
        store.reload().unwrap();

        store
            .update("cfg:bastion", draft("gw", "b.example"))
            .unwrap();
        let jump = |store: &HostStore| store.get(&target.id).unwrap().proxy_jump.unwrap();
        assert_eq!(jump(&store), "u@gw:2,x");
        assert_eq!(
            fs::read_to_string(ssh.join("work")).unwrap(),
            "Host w\n  ProxyJump gw\n"
        );
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
    fn edits_to_a_changed_file_fail() {
        let fx = Fixture::new("Host box\n");
        let mut store = fx.store();
        fs::write(&fx.cfg, "Host other\n").unwrap();
        let err = store.update("cfg:box", draft("box", "b")).unwrap_err();
        assert!(err.to_string().contains("changed on disk"), "{err}");
        assert_eq!(fx.config(), "Host other\n");
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
        // Absolute on every platform (Windows needs a drive).
        let home = std::env::temp_dir().join("me");
        let home = home.as_path();
        assert_eq!(
            portable_path(&home.join(".ssh").join("k"), home),
            "~/.ssh/k"
        );
        let elsewhere = std::env::temp_dir().join("k");
        assert_eq!(portable_path(&elsewhere, home), elsewhere.to_string_lossy());
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
