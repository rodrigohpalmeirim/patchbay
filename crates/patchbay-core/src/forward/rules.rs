//! Port forward rules: the app's own (persisted) and those derived from
//! `LocalForward` / `RemoteForward` / `DynamicForward` in ssh_config.
//!
//! App rules live in a schema-versioned JSON file
//! (`<config_dir>/patchbay/forwards.json`) rewritten atomically on every change,
//! with ids `fwd:<uuid>`. ssh_config rules are read-only, never auto-start,
//! and are identified as `cfg:<alias>:<n>` (`n` = 1-based position among the
//! host's forwarding options, so skipped entries do not shift later ids).
//!
//! An empty name is replaced by a generated one describing the forward,
//! e.g. `L 8080 → localhost:80`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{HostId, PortForward, PortForwardDraft, PortForwardKind, PortForwardSource};
use crate::persist;
use crate::session::join_host_port;
use crate::ssh_config::{ForwardEndpoint, ForwardKind, ForwardSpec};
use crate::{Error, Result};

/// Version 2 added the `remoteDynamic` kind. Files are written as version 1
/// unless they use it, so older Patchbay versions keep reading them.
const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredForward {
    id: String,
    name: String,
    kind: PortForwardKind,
    host_id: HostId,
    bind_address: Option<String>,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
    via_host_id: Option<HostId>,
    #[serde(default)]
    auto_start: bool,
}

impl StoredForward {
    fn to_rule(&self) -> PortForward {
        PortForward {
            id: self.id.clone(),
            name: self.name.clone(),
            source: PortForwardSource::App,
            kind: self.kind,
            host_id: self.host_id.clone(),
            bind_address: self.bind_address.clone(),
            bind_port: self.bind_port,
            target_host: self.target_host.clone(),
            target_port: self.target_port,
            via_host_id: self.via_host_id.clone(),
            auto_start: self.auto_start,
        }
    }

    fn from_rule(rule: PortForward) -> Self {
        Self {
            id: rule.id,
            name: rule.name,
            kind: rule.kind,
            host_id: rule.host_id,
            bind_address: rule.bind_address,
            bind_port: rule.bind_port,
            target_host: rule.target_host,
            target_port: rule.target_port,
            via_host_id: rule.via_host_id,
            auto_start: rule.auto_start,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ForwardsFile {
    version: u32,
    #[serde(default)]
    forwards: Vec<StoredForward>,
}

/// Owner of the app's forward rules. Not internally synchronized; wrap in a
/// lock when shared. Validation against the host list is the caller's job
/// (see [`validate`]).
pub struct ForwardStore {
    path: PathBuf,
    rules: Vec<StoredForward>,
}

impl ForwardStore {
    /// Default location: `<config_dir>/patchbay/forwards.json`.
    pub fn load_default() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| Error::Other("cannot determine the configuration directory".into()))?;
        Self::load(config_dir.join("patchbay").join("forwards.json"))
    }

    /// Explicit location. A missing file means "no rules".
    pub fn load(path: PathBuf) -> Result<Self> {
        let rules = read_rules(&path)?;
        Ok(Self { path, rules })
    }

    /// App rules in creation order.
    pub fn list(&self) -> Vec<PortForward> {
        self.rules.iter().map(StoredForward::to_rule).collect()
    }

    pub fn get(&self, id: &str) -> Option<PortForward> {
        self.rules
            .iter()
            .find(|r| r.id == id)
            .map(StoredForward::to_rule)
    }

    /// Store an already [`validate`]d rule under a new id.
    pub fn create(&mut self, mut rule: PortForward) -> Result<PortForward> {
        rule.id = format!("fwd:{}", uuid::Uuid::new_v4());
        rule.source = PortForwardSource::App;
        let mut rules = self.rules.clone();
        rules.push(StoredForward::from_rule(rule.clone()));
        self.commit(rules)?;
        Ok(rule)
    }

    /// Replace rule `id` with an already [`validate`]d rule.
    pub fn update(&mut self, id: &str, mut rule: PortForward) -> Result<PortForward> {
        let idx = self.index(id)?;
        rule.id = id.to_owned();
        rule.source = PortForwardSource::App;
        let mut rules = self.rules.clone();
        rules[idx] = StoredForward::from_rule(rule.clone());
        self.commit(rules)?;
        Ok(rule)
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        let idx = self.index(id)?;
        let mut rules = self.rules.clone();
        rules.remove(idx);
        self.commit(rules)
    }

    /// Points rules at the new ids of renamed hosts (old id -> new id).
    /// Returns whether any rule changed.
    pub fn rename_hosts(&mut self, renamed: &BTreeMap<HostId, HostId>) -> Result<bool> {
        let mut rules = self.rules.clone();
        let mut changed = false;
        for r in &mut rules {
            for id in std::iter::once(&mut r.host_id).chain(r.via_host_id.as_mut()) {
                if let Some(new) = renamed.get(id.as_str()) {
                    id.clone_from(new);
                    changed = true;
                }
            }
        }
        if changed {
            self.commit(rules)?;
        }
        Ok(changed)
    }

    fn index(&self, id: &str) -> Result<usize> {
        if id.starts_with("cfg:") {
            return Err(read_only(id));
        }
        self.rules
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| Error::NotFound(format!("port forward {id}")))
    }

    /// Persist first, so memory never runs ahead of the file.
    fn commit(&mut self, rules: Vec<StoredForward>) -> Result<()> {
        let uses_v2 = rules
            .iter()
            .any(|r| r.kind == PortForwardKind::RemoteDynamic);
        let body = serde_json::to_vec_pretty(&ForwardsFile {
            version: if uses_v2 { SCHEMA_VERSION } else { 1 },
            forwards: rules.clone(),
        })?;
        persist::write_atomic(&self.path, &body)?;
        self.rules = rules;
        Ok(())
    }
}

pub(crate) fn read_only(id: &str) -> Error {
    Error::Unsupported(format!(
        "port forward {id} comes from ~/.ssh/config and is read-only"
    ))
}

fn read_rules(path: &Path) -> Result<Vec<StoredForward>> {
    let Some(text) = persist::read_optional(path)? else {
        return Ok(Vec::new());
    };
    let file: ForwardsFile = serde_json::from_str(&text)?;
    if file.version > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "{} was written by a newer version of Patchbay (schema {}, supported {})",
            path.display(),
            file.version,
            SCHEMA_VERSION
        )));
    }
    Ok(file.forwards)
}

/// Check and normalize a draft (trimmed strings, empty → `None`, IPv6
/// brackets removed, target fields cleared for SOCKS forwards, generated
/// name if empty). `host_exists` tells whether a host id is known. The
/// returned rule has `id` and `source` left for the caller to fill in.
pub fn validate(
    draft: PortForwardDraft,
    host_exists: impl Fn(&str) -> bool,
) -> Result<PortForward> {
    let invalid = |msg: &str| Error::Other(msg.to_owned());
    let host_id = draft.host_id.trim().to_owned();
    if host_id.is_empty() {
        return Err(invalid("Choose the SSH host that carries the forward"));
    }
    if !host_exists(&host_id) {
        return Err(Error::UnknownHost(host_id));
    }
    let bind_address =
        address(draft.bind_address, true).map_err(|e| invalid(&format!("Listen address {e}")))?;

    let (target_host, target_port) = match draft.kind {
        PortForwardKind::Dynamic | PortForwardKind::RemoteDynamic => (None, None),
        PortForwardKind::Local | PortForwardKind::Remote => {
            let host = address(draft.target_host, false)
                .map_err(|e| invalid(&format!("Destination host {e}")))?
                .ok_or_else(|| invalid("Destination host is required"))?;
            let port = match draft.target_port {
                None => return Err(invalid("Destination port is required")),
                Some(0) => return Err(invalid("Destination port must be between 1 and 65535")),
                Some(p) => p,
            };
            (Some(host), Some(port))
        }
    };

    if draft.kind.listens_remotely() && draft.bind_port == 0 {
        return Err(invalid(
            "Remote forwards need a listen port between 1 and 65535",
        ));
    }

    let via_host_id = draft
        .via_host_id
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    if let Some(via) = &via_host_id {
        if !draft.kind.listens_remotely() {
            return Err(invalid(
                "Only forwards that listen on a server can connect out through another host",
            ));
        }
        if *via == host_id {
            return Err(invalid(
                "The host to connect out from must differ from the listening host",
            ));
        }
        if !host_exists(via) {
            return Err(Error::UnknownHost(via.clone()));
        }
    }

    let mut rule = PortForward {
        id: String::new(),
        name: draft.name.trim().to_owned(),
        source: PortForwardSource::App,
        kind: draft.kind,
        host_id,
        bind_address,
        bind_port: draft.bind_port,
        target_host,
        target_port,
        via_host_id,
        auto_start: draft.auto_start,
    };
    if rule.name.is_empty() {
        rule.name = describe(&rule);
    }
    Ok(rule)
}

/// Trim, unbracket and sanity-check a host/address field. `*` is only
/// allowed for listen addresses.
fn address(value: Option<String>, listen: bool) -> std::result::Result<Option<String>, String> {
    let Some(v) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let v = v
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .map(str::to_owned)
        .unwrap_or(v);
    if listen && v == "*" {
        return Ok(Some(v));
    }
    let ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%'));
    if ok {
        Ok(Some(v))
    } else {
        Err(format!("{v:?} is not a valid host name or IP address"))
    }
}

/// Short human description: `L 8080 → localhost:80`, `D 1080`,
/// `R 0.0.0.0:9000 → db:5432 via cfg:bastion`, `R 1080 SOCKS`.
pub fn describe(rule: &PortForward) -> String {
    let listen = match rule.bind_address.as_deref() {
        None => rule.bind_port.to_string(),
        Some(addr) => join_host_port(addr, rule.bind_port),
    };
    let target = match (&rule.target_host, rule.target_port) {
        (Some(h), Some(p)) => join_host_port(h, p),
        _ => String::new(),
    };
    match rule.kind {
        PortForwardKind::Dynamic => format!("D {listen}"),
        PortForwardKind::Local => format!("L {listen} → {target}"),
        PortForwardKind::Remote => match &rule.via_host_id {
            Some(via) => format!("R {listen} → {target} via {via}"),
            None => format!("R {listen} → {target}"),
        },
        PortForwardKind::RemoteDynamic => match &rule.via_host_id {
            Some(via) => format!("R {listen} SOCKS via {via}"),
            None => format!("R {listen} SOCKS"),
        },
    }
}

/// The read-only rules for ssh_config host `alias`, plus warnings for
/// forwards Patchbay cannot run (Unix sockets, remote port 0).
pub fn config_rules(alias: &str, specs: &[ForwardSpec]) -> (Vec<PortForward>, Vec<String>) {
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let keyword = match spec.kind {
            ForwardKind::Local => "LocalForward",
            ForwardKind::Remote => "RemoteForward",
            ForwardKind::Dynamic => "DynamicForward",
        };
        let mut skip =
            |why: &str| warnings.push(format!("Host {alias}: {keyword} #{} skipped: {why}", i + 1));
        let ForwardEndpoint::Tcp { host, port } = &spec.listen else {
            skip("Unix-domain socket forwarding is not supported");
            continue;
        };
        let (target_host, target_port) = match &spec.connect {
            Some(ForwardEndpoint::Tcp { host, port }) => (host.clone(), Some(*port)),
            Some(ForwardEndpoint::Unix(_)) => {
                skip("Unix-domain socket forwarding is not supported");
                continue;
            }
            None => (None, None),
        };
        let kind = match (spec.kind, &spec.connect) {
            (ForwardKind::Local, _) => PortForwardKind::Local,
            // `RemoteForward [addr:]port` without a destination: a SOCKS proxy on the server.
            (ForwardKind::Remote, None) => PortForwardKind::RemoteDynamic,
            (ForwardKind::Remote, Some(_)) => PortForwardKind::Remote,
            (ForwardKind::Dynamic, _) => PortForwardKind::Dynamic,
        };
        let bind_address = host.as_ref().map(|h| {
            if h.is_empty() {
                "*".to_owned()
            } else {
                h.clone()
            }
        });
        let draft = PortForwardDraft {
            name: String::new(),
            kind,
            host_id: format!("cfg:{alias}"),
            bind_address,
            bind_port: *port,
            target_host,
            target_port,
            via_host_id: None,
            auto_start: false,
        };
        match validate(draft, |_| true) {
            Ok(mut rule) => {
                rule.id = format!("cfg:{alias}:{}", i + 1);
                rule.source = PortForwardSource::SshConfig;
                rule.name = format!("{alias}: {}", rule.name);
                rules.push(rule);
            }
            Err(e) => skip(&e.to_string()),
        }
    }
    (rules, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn draft(kind: PortForwardKind) -> PortForwardDraft {
        PortForwardDraft {
            name: String::new(),
            kind,
            host_id: "cfg:a".into(),
            bind_address: None,
            bind_port: 8080,
            target_host: Some("localhost".into()),
            target_port: Some(80),
            via_host_id: None,
            auto_start: false,
        }
    }

    fn known(id: &str) -> bool {
        matches!(id, "cfg:a" | "app:b")
    }

    fn err(d: PortForwardDraft) -> String {
        validate(d, known).unwrap_err().to_string()
    }

    #[test]
    fn renamed_hosts_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forwards.json");
        let mut store = ForwardStore::load(path.clone()).unwrap();
        let rule = validate(
            PortForwardDraft {
                via_host_id: Some("app:b".into()),
                ..draft(PortForwardKind::Remote)
            },
            known,
        )
        .unwrap();
        let rule = store.create(rule).unwrap();
        let renamed = BTreeMap::from([
            ("app:b".to_owned(), "cfg:b".to_owned()),
            ("cfg:a".to_owned(), "cfg:c".to_owned()),
        ]);
        assert!(store.rename_hosts(&renamed).unwrap());
        assert!(!store.rename_hosts(&renamed).unwrap());
        let again = ForwardStore::load(path).unwrap().get(&rule.id).unwrap();
        assert_eq!(again.host_id, "cfg:c");
        assert_eq!(again.via_host_id.as_deref(), Some("cfg:b"));
    }

    #[test]
    fn validation_normalizes() {
        let rule = validate(
            PortForwardDraft {
                name: "  web ".into(),
                bind_address: Some(" [::1] ".into()),
                target_host: Some(" [fe80::1%eth0] ".into()),
                ..draft(PortForwardKind::Local)
            },
            known,
        )
        .unwrap();
        assert_eq!(rule.name, "web");
        assert_eq!(rule.bind_address.as_deref(), Some("::1"));
        assert_eq!(rule.target_host.as_deref(), Some("fe80::1%eth0"));

        let rule = validate(draft(PortForwardKind::Local), known).unwrap();
        assert_eq!(rule.name, "L 8080 → localhost:80");
        assert_eq!(rule.bind_address, None);

        let rule = validate(
            PortForwardDraft {
                bind_address: Some("*".into()),
                bind_port: 0,
                ..draft(PortForwardKind::Dynamic)
            },
            known,
        )
        .unwrap();
        assert_eq!((rule.target_host, rule.target_port), (None, None));
        assert_eq!(rule.name, "D *:0");

        let rule = validate(
            PortForwardDraft {
                via_host_id: Some("app:b".into()),
                bind_address: Some("0.0.0.0".into()),
                ..draft(PortForwardKind::Remote)
            },
            known,
        )
        .unwrap();
        assert_eq!(rule.name, "R 0.0.0.0:8080 → localhost:80 via app:b");

        let rule = validate(
            PortForwardDraft {
                via_host_id: Some("  ".into()),
                ..draft(PortForwardKind::Remote)
            },
            known,
        )
        .unwrap();
        assert_eq!(rule.via_host_id, None);
    }

    #[test]
    fn validation_rejects() {
        use PortForwardKind::*;
        assert!(matches!(
            validate(
                PortForwardDraft {
                    host_id: "cfg:nope".into(),
                    ..draft(Local)
                },
                known
            ),
            Err(Error::UnknownHost(_))
        ));
        assert!(
            err(PortForwardDraft {
                host_id: " ".into(),
                ..draft(Local)
            })
            .contains("host")
        );
        for kind in [Local, Remote] {
            assert!(
                err(PortForwardDraft {
                    target_host: Some(" ".into()),
                    ..draft(kind)
                })
                .contains("Destination host is required")
            );
            assert!(
                err(PortForwardDraft {
                    target_port: None,
                    ..draft(kind)
                })
                .contains("Destination port is required")
            );
            assert!(
                err(PortForwardDraft {
                    target_port: Some(0),
                    ..draft(kind)
                })
                .contains("between 1 and 65535")
            );
        }
        for kind in [Remote, RemoteDynamic] {
            assert!(
                err(PortForwardDraft {
                    bind_port: 0,
                    ..draft(kind)
                })
                .contains("listen port")
            );
        }
        assert!(
            validate(
                PortForwardDraft {
                    bind_port: 0,
                    ..draft(Local)
                },
                known
            )
            .is_ok()
        );
        for kind in [Local, Dynamic] {
            assert!(
                err(PortForwardDraft {
                    via_host_id: Some("app:b".into()),
                    ..draft(kind)
                })
                .contains("Only forwards that listen on a server")
            );
        }
        for kind in [Remote, RemoteDynamic] {
            assert!(
                err(PortForwardDraft {
                    via_host_id: Some("cfg:a".into()),
                    ..draft(kind)
                })
                .contains("must differ")
            );
        }
        // A SOCKS proxy on the server, going online from another host.
        let rule = validate(
            PortForwardDraft {
                via_host_id: Some("app:b".into()),
                ..draft(RemoteDynamic)
            },
            known,
        )
        .unwrap();
        assert_eq!((rule.target_host, rule.target_port), (None, None));
        assert_eq!(rule.name, "R 8080 SOCKS via app:b");
        assert!(matches!(
            validate(
                PortForwardDraft {
                    via_host_id: Some("app:zzz".into()),
                    ..draft(Remote)
                },
                known
            ),
            Err(Error::UnknownHost(id)) if id == "app:zzz"
        ));
        assert!(
            err(PortForwardDraft {
                bind_address: Some("a b".into()),
                ..draft(Local)
            })
            .contains("Listen address")
        );
        assert!(
            err(PortForwardDraft {
                target_host: Some("*".into()),
                ..draft(Local)
            })
            .contains("Destination host")
        );
    }

    #[test]
    fn crud_persists_and_protects_config_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patchbay/forwards.json");
        let mut store = ForwardStore::load(path.clone()).unwrap();
        assert!(store.list().is_empty());
        assert!(!path.exists());

        let rule = validate(
            PortForwardDraft {
                auto_start: true,
                ..draft(PortForwardKind::Local)
            },
            known,
        )
        .unwrap();
        let created = store.create(rule).unwrap();
        assert!(created.id.starts_with("fwd:"));
        assert_eq!(created.source, PortForwardSource::App);

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["forwards"][0]["hostId"], "cfg:a");
        assert_eq!(json["forwards"][0]["autoStart"], true);

        let again = ForwardStore::load(path.clone()).unwrap();
        assert_eq!(again.list(), store.list());
        assert_eq!(again.get(&created.id).unwrap(), created);

        let changed = validate(
            PortForwardDraft {
                name: "db".into(),
                ..draft(PortForwardKind::Dynamic)
            },
            known,
        )
        .unwrap();
        let updated = store.update(&created.id, changed).unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.kind, PortForwardKind::Dynamic);
        assert_eq!(
            ForwardStore::load(path.clone())
                .unwrap()
                .get(&created.id)
                .unwrap(),
            updated
        );

        assert!(matches!(
            store.delete("cfg:a:1"),
            Err(Error::Unsupported(_))
        ));
        let rule = validate(draft(PortForwardKind::Local), known).unwrap();
        assert!(matches!(
            store.update("cfg:a:1", rule),
            Err(Error::Unsupported(_))
        ));
        store.delete(&created.id).unwrap();
        assert!(matches!(store.delete(&created.id), Err(Error::NotFound(_))));
        assert!(ForwardStore::load(path.clone()).unwrap().list().is_empty());

        // Version 2 only when a rule needs it, so older versions keep reading the file.
        let rule = validate(draft(PortForwardKind::RemoteDynamic), known).unwrap();
        let created = store.create(rule).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["forwards"][0]["kind"], "remoteDynamic");
        assert_eq!(
            ForwardStore::load(path.clone()).unwrap().get(&created.id),
            Some(created)
        );

        fs::write(&path, r#"{"version": 3, "forwards": []}"#).unwrap();
        assert!(ForwardStore::load(path).is_err());
    }

    #[test]
    fn ssh_config_import() {
        let config = crate::ssh_config::SshConfig::parse(
            "Host f\n  LocalForward 8080 localhost:80\n  LocalForward [::1]:9000 [fe80::1]:22\n\
             RemoteForward 0.0.0.0:2222 /run/sock\n  RemoteForward 1080\n  DynamicForward *:1081\n\
             RemoteForward 9000 db:5432\n  RemoteForward 0 db:5432\n  LocalForward /tmp/s host:1\n",
            Path::new("/cfg"),
            crate::ssh_config::LocalEnv::detect(),
        );
        let resolved = config.resolve("f", "me");
        let (rules, warnings) = config_rules("f", &resolved.forwards);
        let summary: Vec<_> = rules
            .iter()
            .map(|r| (r.id.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("cfg:f:1", "f: L 8080 → localhost:80"),
                ("cfg:f:2", "f: L [::1]:9000 → [fe80::1]:22"),
                ("cfg:f:4", "f: R 1080 SOCKS"),
                ("cfg:f:5", "f: D *:1081"),
                ("cfg:f:6", "f: R 9000 → db:5432"),
            ]
        );
        assert!(
            rules
                .iter()
                .all(|r| r.source == PortForwardSource::SshConfig
                    && !r.auto_start
                    && r.host_id == "cfg:f")
        );
        assert_eq!(rules[2].kind, PortForwardKind::RemoteDynamic);
        assert_eq!(rules[3].kind, PortForwardKind::Dynamic);
        assert_eq!(rules[4].kind, PortForwardKind::Remote);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("RemoteForward #3") && warnings[0].contains("Unix"));
        assert!(warnings[1].contains("RemoteForward #7"));
        assert!(warnings[2].contains("LocalForward #8"));
    }
}
