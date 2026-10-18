//! The merged host list (`~/.ssh/config` + app-managed hosts) and resolution
//! of a host into a [`ConnectTarget`].
//!
//! ssh_config hosts are read-only and identified as `cfg:<alias>`. App hosts
//! (`app:<uuid>`) live in a schema-versioned JSON file that is rewritten
//! atomically on every change. App hosts behave like `ssh [-l user] [-p port]
//! [-J jump] <hostname>`: explicitly set fields win, everything else comes
//! from resolving `hostname` through ssh_config (so `Host *` defaults apply).
//!
//! CONTRACT (used by the Tauri layer and `session`): keep these signatures stable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::forward::config_rules;
use crate::model::{
    AgentSource, AuthMethods, ConnectTarget, Host, HostDraft, HostId, HostSource, PortForward,
};
use crate::persist;
use crate::ssh_config::{LocalEnv, ResolveOverrides, ResolvedHost, SshConfig, expand_tilde};
use crate::{Error, Result};

const SCHEMA_VERSION: u32 = 1;

/// OpenSSH's default identities, used when no `IdentityFile` applies.
const DEFAULT_IDENTITIES: &[&str] = &[
    "id_ed25519",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519_sk",
    "id_rsa",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
struct AppHostsFile {
    version: u32,
    #[serde(default)]
    hosts: Vec<StoredHost>,
}

/// Owner of the host list. Not internally synchronized; wrap in a lock when
/// shared.
pub struct HostStore {
    ssh_config_path: PathBuf,
    app_hosts_path: PathBuf,
    env: LocalEnv,
    config: SshConfig,
    cfg_hosts: Vec<Host>,
    cfg_forwards: Vec<PortForward>,
    app_hosts: Vec<StoredHost>,
    warnings: Vec<String>,
}

impl HostStore {
    /// Default locations: `~/.ssh/config` and `<config_dir>/patchbay/hosts.json`.
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

    /// Explicit locations (tests). A missing file means "no hosts".
    pub fn load(ssh_config: PathBuf, app_hosts: PathBuf) -> Result<Self> {
        Self::load_with_env(ssh_config, app_hosts, LocalEnv::detect())
    }

    /// Explicit locations and local environment (home directory, user name).
    pub fn load_with_env(ssh_config: PathBuf, app_hosts: PathBuf, env: LocalEnv) -> Result<Self> {
        let mut store = Self {
            config: SshConfig::empty(env.clone()),
            ssh_config_path: ssh_config,
            app_hosts_path: app_hosts,
            env,
            cfg_hosts: Vec::new(),
            cfg_forwards: Vec::new(),
            app_hosts: Vec::new(),
            warnings: Vec::new(),
        };
        store.reload()?;
        Ok(store)
    }

    /// Re-read both sources from disk.
    pub fn reload(&mut self) -> Result<()> {
        let config = SshConfig::load_with_env(&self.ssh_config_path, self.env.clone())?;
        let app_hosts = read_app_hosts(&self.app_hosts_path)?;
        self.config = config;
        self.app_hosts = app_hosts;
        self.refresh();
        Ok(())
    }

    /// Non-fatal problems found while parsing ssh_config (unsupported
    /// keywords, `Match exec`, bad Include, ...), for display in the UI.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn list(&self) -> Vec<Host> {
        let mut app: Vec<Host> = self.app_hosts.iter().map(|s| self.app_host(s)).collect();
        app.sort_by_cached_key(|h| {
            (
                h.group.as_ref().map(|g| g.to_lowercase()),
                h.name.to_lowercase(),
                h.id.clone(),
            )
        });
        self.cfg_hosts.iter().cloned().chain(app).collect()
    }

    /// Read-only port forward rules from the `LocalForward` /
    /// `RemoteForward` / `DynamicForward` options of ssh_config hosts (see
    /// [`crate::forward::config_rules`]), in config order.
    pub fn config_forwards(&self) -> &[PortForward] {
        &self.cfg_forwards
    }

    pub fn get(&self, id: &str) -> Option<Host> {
        self.cfg_hosts
            .iter()
            .find(|h| h.id == id)
            .cloned()
            .or_else(|| {
                self.app_hosts
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| self.app_host(s))
            })
    }

    pub fn create(&mut self, draft: HostDraft) -> Result<Host> {
        let stored = validate(format!("app:{}", uuid::Uuid::new_v4()), draft)?;
        let mut hosts = self.app_hosts.clone();
        hosts.push(stored.clone());
        self.commit(hosts)?;
        Ok(self.app_host(&stored))
    }

    /// Errors with `ReadOnlyHost` for ssh_config hosts.
    pub fn update(&mut self, id: &str, draft: HostDraft) -> Result<Host> {
        let idx = self.app_index(id)?;
        let stored = validate(id.to_owned(), draft)?;
        let mut hosts = self.app_hosts.clone();
        hosts[idx] = stored.clone();
        self.commit(hosts)?;
        Ok(self.app_host(&stored))
    }

    /// Errors with `ReadOnlyHost` for ssh_config hosts.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let idx = self.app_index(id)?;
        let mut hosts = self.app_hosts.clone();
        hosts.remove(idx);
        self.commit(hosts)
    }

    /// Fully resolve a host for connecting, including its ProxyJump chain.
    pub fn resolve_target(&self, id: &HostId) -> Result<ConnectTarget> {
        let user = &self.env.user;
        if id.starts_with("cfg:") {
            let host = self
                .cfg_hosts
                .iter()
                .find(|h| &h.id == id)
                .ok_or_else(|| Error::UnknownHost(id.clone()))?;
            let resolved = self.config.resolve(&host.name, user);
            let mut visiting = vec![host.name.to_lowercase()];
            let jumps = self.expand_jumps(resolved.proxy_jump.as_deref(), &mut visiting)?;
            let mut target = self.target(&resolved, host.name.clone(), Some(id.clone()));
            target.jumps = jumps;
            return Ok(target);
        }

        let stored = self
            .app_hosts
            .iter()
            .find(|s| &s.id == id)
            .ok_or_else(|| Error::UnknownHost(id.clone()))?;
        let resolved = self.resolve_app(stored);
        let jump_spec = stored
            .proxy_jump
            .as_deref()
            .or(resolved.proxy_jump.as_deref());
        let mut visiting = vec![stored.hostname.to_lowercase()];
        let jumps = self.expand_jumps(jump_spec, &mut visiting)?;
        let mut target = self.target(&resolved, stored.name.clone(), Some(id.clone()));
        if !stored.identity_files.is_empty() {
            target.identity_files = stored
                .identity_files
                .iter()
                .map(|p| PathBuf::from(expand_tilde(&p.to_string_lossy(), &self.env.home)))
                .collect();
        }
        target.jumps = jumps;
        Ok(target)
    }

    fn app_index(&self, id: &str) -> Result<usize> {
        if id.starts_with("cfg:") {
            return Err(Error::ReadOnlyHost(id.to_owned()));
        }
        self.app_hosts
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| Error::UnknownHost(id.to_owned()))
    }

    /// Persist `hosts`, then make them current. On error nothing changes.
    fn commit(&mut self, hosts: Vec<StoredHost>) -> Result<()> {
        write_app_hosts(&self.app_hosts_path, &hosts)?;
        self.app_hosts = hosts;
        self.refresh();
        Ok(())
    }

    /// Recompute the ssh_config host entries and the warning list.
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
        for alias in self.config.host_aliases() {
            let r = self.config.resolve(alias, &self.env.user);
            let (forwards, forward_warnings) = config_rules(alias, &r.forwards);
            cfg_forwards.extend(forwards);
            push(forward_warnings);
            cfg_hosts.push(Host {
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
            });
            push(r.warnings);
        }
        for s in &self.app_hosts {
            push(self.resolve_app(s).warnings);
        }
        self.cfg_hosts = cfg_hosts;
        self.cfg_forwards = cfg_forwards;
        self.warnings = warnings;
    }

    fn resolve_app(&self, s: &StoredHost) -> ResolvedHost {
        let overrides = ResolveOverrides {
            user: s.user.clone(),
            port: s.port,
        };
        self.config
            .resolve_with(&s.hostname, &self.env.user, &overrides)
    }

    /// The sidebar view of an app host: stored fields as entered, except
    /// `port`, which shows the effective port when none was set.
    fn app_host(&self, s: &StoredHost) -> Host {
        let port = s.port.or_else(|| self.resolve_app(s).port).unwrap_or(22);
        Host {
            id: s.id.clone(),
            name: s.name.clone(),
            source: HostSource::App,
            hostname: s.hostname.clone(),
            port,
            user: s.user.clone(),
            identity_files: s.identity_files.clone(),
            proxy_jump: s.proxy_jump.clone(),
            group: s.group.clone(),
            tags: s.tags.clone(),
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

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn validate(id: HostId, draft: HostDraft) -> Result<StoredHost> {
    let name = draft.name.trim().to_owned();
    let hostname = draft.hostname.trim().to_owned();
    if name.is_empty() {
        return Err(Error::Other("Name must not be empty".into()));
    }
    if hostname.is_empty() {
        return Err(Error::Other("Hostname must not be empty".into()));
    }
    if hostname.contains(char::is_whitespace) {
        return Err(Error::Other("Hostname must not contain spaces".into()));
    }
    if draft.port == Some(0) {
        return Err(Error::Other("Port must be between 1 and 65535".into()));
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
    Ok(StoredHost {
        id,
        name,
        hostname,
        port: draft.port,
        user: non_empty(draft.user),
        identity_files: draft
            .identity_files
            .into_iter()
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        proxy_jump,
        group: non_empty(draft.group),
        tags,
    })
}

fn read_app_hosts(path: &Path) -> Result<Vec<StoredHost>> {
    let Some(text) = persist::read_optional(path)? else {
        return Ok(Vec::new());
    };
    let file: AppHostsFile = serde_json::from_str(&text)?;
    if file.version > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "{} was written by a newer version of Patchbay (schema {}, supported {})",
            path.display(),
            file.version,
            SCHEMA_VERSION
        )));
    }
    Ok(file.hosts)
}

fn write_app_hosts(path: &Path, hosts: &[StoredHost]) -> Result<()> {
    let body = serde_json::to_vec_pretty(&AppHostsFile {
        version: SCHEMA_VERSION,
        hosts: hosts.to_vec(),
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
    }

    impl Fixture {
        fn new(config: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            fs::create_dir_all(home.join(".ssh")).unwrap();
            let cfg = home.join(".ssh/config");
            fs::write(&cfg, config).unwrap();
            let app = dir.path().join("conf/patchbay/hosts.json");
            Self {
                _dir: dir,
                home,
                cfg,
                app,
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
        assert!(list[2].id.starts_with("app:"));
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
                ..draft(" box ", "box.lan")
            })
            .unwrap();
        assert_eq!(h.name, "box");
        assert_eq!(h.user, None);
        assert_eq!(h.tags, ["a", "b"]);

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fx.app).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["hosts"][0]["hostname"], "box.lan");
        let leftovers = fs::read_dir(fx.app.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1);

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
        assert_eq!(updated.id, h.id);
        assert_eq!(updated.port, 2200);
        store.reload().unwrap();
        assert_eq!(store.get(&h.id).unwrap().name, "box2");

        store.delete(&h.id).unwrap();
        assert!(fx.store().list().is_empty());
        assert!(matches!(store.delete(&h.id), Err(Error::UnknownHost(_))));
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
        ] {
            assert!(matches!(store.create(bad), Err(Error::Other(_))));
        }
        assert!(!fx.app.exists());
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
        let fx = Fixture::new(
            "Host *.corp\n  User corpuser\n  Port 2200\n\
             Host *\n  IdentityFile ~/.ssh/shared\n  ServerAliveInterval 20\n",
        );
        let mut store = fx.store();
        let inherit = store.create(draft("Inherit", "srv.corp")).unwrap();
        let t = store.resolve_target(&inherit.id).unwrap();
        assert_eq!(t.label, "Inherit");
        assert_eq!(t.host_id.as_deref(), Some(inherit.id.as_str()));
        assert_eq!(t.hostname, "srv.corp");
        assert_eq!(t.user, "corpuser");
        assert_eq!(t.port, 2200);
        assert_eq!(t.identity_files, vec![fx.home.join(".ssh/shared")]);
        assert_eq!(t.server_alive_interval, Some(Duration::from_secs(20)));

        let explicit = store
            .create(HostDraft {
                user: Some("me".into()),
                port: Some(2),
                identity_files: vec!["~/.ssh/mine".into()],
                ..draft("Explicit", "srv.corp")
            })
            .unwrap();
        let t = store.resolve_target(&explicit.id).unwrap();
        assert_eq!(t.user, "me");
        assert_eq!(t.port, 2);
        assert_eq!(t.identity_files, vec![fx.home.join(".ssh/mine")]);
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
        let fx = Fixture::new("Host *.corp\n  ProxyJump gw\nHost gw\n  ProxyJump none\n");
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
                proxy_jump: Some("x@other:2".into()),
                ..draft("other", "c.corp")
            })
            .unwrap();
        let t = store.resolve_target(&other.id).unwrap();
        assert_eq!(t.jumps.len(), 1);
        assert_eq!(
            (t.jumps[0].hostname.as_str(), t.jumps[0].port),
            ("other", 2)
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
