//! Tailnet device discovery through the local Tailscale CLI.
//!
//! Runs `tailscale status --json` and turns its peers into
//! [`TailscaleDevice`]s that could plausibly run an SSH server: self, nodes
//! we shared our device to ("sharee" nodes) and mobile/TV devices are left
//! out. Tailscale's JSON is not a stable API, so every field is optional
//! (missing or `null` falls back to its default).

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{Error, Result};

/// How long `tailscale status` may take before we give up.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// OSes (compared case-insensitively) that don't run SSH servers.
const NON_SSH_OSES: &[&str] = &["ios", "android", "tvos"];

/// Login name Tailscale reports as the "owner" of tagged devices.
const TAGGED_DEVICES_LOGIN: &str = "tagged-devices";

/// A tailnet peer that could run an SSH server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleDevice {
    /// Stable node ID (the `ID` field, e.g. "nAbC123XyZ11CNTRL"); survives renames and IP changes.
    pub id: String,
    /// Display name: first label of the MagicDNS name, else `HostName`.
    pub name: String,
    /// MagicDNS name without the trailing dot, e.g. "nas.tail1234.ts.net". None if empty.
    pub dns_name: Option<String>,
    /// OS hostname as reported by the device (`HostName`).
    pub hostname: String,
    /// Raw Tailscale OS string: "linux", "windows", "macOS", "freebsd", ...
    pub os: String,
    /// Tailscale IPs, IPv4 first.
    pub ips: Vec<IpAddr>,
    pub online: bool,
    /// OpenSSH-format public host keys advertised when the device runs Tailscale SSH (`sshHostKeys`). Empty otherwise.
    pub ssh_host_keys: Vec<String>,
    /// ACL tags, e.g. ["tag:server"].
    pub tags: Vec<String>,
    /// Owner's login name from the top-level `User` map (keyed by the peer's `UserID` as a string); None for tagged devices (login "tagged-devices") or unknown.
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailnetStatus {
    /// `MagicDNSSuffix`, e.g. "tail1234.ts.net" (None if empty).
    pub magic_dns_suffix: Option<String>,
    /// Peers sorted by name (case-insensitive), then id.
    pub devices: Vec<TailscaleDevice>,
}

/// Parse `tailscale status --json` output. Ok(None) when the backend is not
/// running (`BackendState` != "Running", e.g. "Stopped", "NeedsLogin").
pub fn parse_status(json: &str) -> Result<Option<TailnetStatus>> {
    let raw: RawStatus = serde_json::from_str(json)?;
    if raw.backend_state != "Running" {
        return Ok(None);
    }

    let magic_dns_suffix = non_empty(raw.magic_dns_suffix.trim_end_matches('.'))
        .or_else(|| non_empty(raw.current_tailnet.magic_dns_suffix.trim_end_matches('.')));
    let self_id = raw.self_node.id;

    let mut devices: Vec<TailscaleDevice> = raw
        .peer
        .into_values()
        .filter(|p| !p.sharee_node && (self_id.is_empty() || p.id != self_id))
        .filter(|p| !NON_SSH_OSES.iter().any(|os| p.os.eq_ignore_ascii_case(os)))
        .filter_map(|p| device(p, &raw.user))
        .collect();
    devices.sort_by_cached_key(|d| (d.name.to_lowercase(), d.id.clone()));

    Ok(Some(TailnetStatus {
        magic_dns_suffix,
        devices,
    }))
}

/// Convert a raw peer; None if it has neither a DNS name nor an IP.
fn device(peer: RawPeer, users: &HashMap<String, RawUser>) -> Option<TailscaleDevice> {
    let dns_name = non_empty(peer.dns_name.trim_end_matches('.'));
    let mut ips: Vec<IpAddr> = peer
        .tailscale_ips
        .iter()
        .filter_map(|ip| ip.parse().ok())
        .collect();
    ips.sort_by_key(IpAddr::is_ipv6);
    if dns_name.is_none() && ips.is_empty() {
        return None;
    }

    let name = dns_name
        .as_deref()
        .and_then(|dns| non_empty(dns.split('.').next().unwrap_or_default()))
        .or_else(|| non_empty(&peer.host_name))
        .unwrap_or_else(|| ips[0].to_string());
    let owner = peer
        .user_id
        .and_then(|uid| users.get(&uid.to_string()))
        .map(|u| u.login_name.as_str())
        .filter(|login| !login.is_empty() && *login != TAGGED_DEVICES_LOGIN)
        .map(str::to_owned);

    Some(TailscaleDevice {
        id: peer.id,
        name,
        dns_name,
        hostname: peer.host_name,
        os: peer.os,
        ips,
        online: peer.online,
        ssh_host_keys: peer.ssh_host_keys,
        tags: peer.tags,
        owner,
    })
}

/// Locate the CLI: `tailscale` (or `tailscale.exe`) on PATH, then
/// well-known install locations. GUI apps on macOS often lack Homebrew dirs
/// on PATH, hence the explicit list.
pub fn find_cli() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join(exe));
    on_path
        .chain(well_known_paths())
        .find(|candidate| candidate.is_file())
}

fn well_known_paths() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        [
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/opt/homebrew/bin/tailscale",
            "/usr/local/bin/tailscale",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    } else if cfg!(windows) {
        let program_files = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        std::iter::once(program_files)
            .chain(std::env::var_os("ProgramFiles(x86)").map(PathBuf::from))
            .map(|dir| dir.join("Tailscale").join("tailscale.exe"))
            .collect()
    } else {
        [
            "/usr/bin/tailscale",
            "/usr/local/bin/tailscale",
            "/usr/sbin/tailscale",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
}

/// Run `<cli> status --json`. Ok(None) if the CLI is not installed, exits
/// non-zero (e.g. tailscaled not running) or the backend is not running.
/// Timeout or unparsable output is an error.
pub async fn status() -> Result<Option<TailnetStatus>> {
    let Some(cli) = find_cli() else {
        return Ok(None);
    };

    let mut cmd = tokio::process::Command::new(&cli);
    cmd.args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = match tokio::time::timeout(STATUS_TIMEOUT, cmd.output()).await {
        Err(_) => {
            return Err(Error::Timeout(format!(
                "`{} status --json` did not finish within {}s",
                cli.display(),
                STATUS_TIMEOUT.as_secs()
            )));
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(Err(e)) => return Err(e.into()),
        Ok(Ok(output)) => output,
    };

    if !output.status.success() {
        tracing::debug!(
            cli = %cli.display(),
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "tailscale status failed; treating tailnet as unavailable"
        );
        return Ok(None);
    }
    parse_status(&String::from_utf8_lossy(&output.stdout))
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// `#[serde(default)]` only covers missing fields; this also maps `null` to
/// the default.
fn null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct RawStatus {
    #[serde(deserialize_with = "null_default")]
    backend_state: String,
    #[serde(rename = "MagicDNSSuffix", deserialize_with = "null_default")]
    magic_dns_suffix: String,
    #[serde(deserialize_with = "null_default")]
    current_tailnet: RawTailnet,
    #[serde(rename = "Self", deserialize_with = "null_default")]
    self_node: RawPeer,
    #[serde(deserialize_with = "null_default")]
    peer: HashMap<String, RawPeer>,
    #[serde(deserialize_with = "null_default")]
    user: HashMap<String, RawUser>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTailnet {
    #[serde(rename = "MagicDNSSuffix", deserialize_with = "null_default")]
    magic_dns_suffix: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct RawPeer {
    #[serde(rename = "ID", deserialize_with = "null_default")]
    id: String,
    #[serde(deserialize_with = "null_default")]
    host_name: String,
    #[serde(rename = "DNSName", deserialize_with = "null_default")]
    dns_name: String,
    #[serde(rename = "OS", deserialize_with = "null_default")]
    os: String,
    /// A JSON number; the `User` map is keyed by its decimal string.
    #[serde(rename = "UserID")]
    user_id: Option<serde_json::Number>,
    #[serde(rename = "TailscaleIPs", deserialize_with = "null_default")]
    tailscale_ips: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    online: bool,
    #[serde(rename = "sshHostKeys", deserialize_with = "null_default")]
    ssh_host_keys: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    tags: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    sharee_node: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct RawUser {
    #[serde(deserialize_with = "null_default")]
    login_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/tailscale-status.json");

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|ip| ip.parse().unwrap()).collect()
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_fixture() {
        let status = parse_status(FIXTURE).unwrap().unwrap();
        assert_eq!(status.magic_dns_suffix.as_deref(), Some("tail1234.ts.net"));

        let names: Vec<&str> = status.devices.iter().map(|d| d.name.as_str()).collect();
        // Self, the android phone and the sharee node are filtered out.
        assert_eq!(
            names,
            [
                "alices-macbook-pro",
                "ci-runner",
                "desktop-7q2k",
                "mary-server",
                "nas"
            ]
        );

        let mac = &status.devices[0];
        assert_eq!(
            mac,
            &TailscaleDevice {
                id: "nMAC0000011CNTRL".into(),
                name: "alices-macbook-pro".into(),
                dns_name: Some("alices-macbook-pro.tail1234.ts.net".into()),
                hostname: "Alices-MacBook-Pro".into(),
                os: "macOS".into(),
                // Listed IPv6-first in the JSON.
                ips: ips(&["100.64.0.3", "fd7a:115c:a1e0::3"]),
                online: false,
                ssh_host_keys: vec![],
                tags: vec![],
                owner: Some("alice@example.com".into()),
            }
        );

        let ci = &status.devices[1];
        assert_eq!(ci.tags, strings(&["tag:ci"]));
        assert_eq!(ci.owner, None);
        assert_eq!(ci.ssh_host_keys.len(), 1);

        let desktop = &status.devices[2];
        assert_eq!(desktop.hostname, "DESKTOP-7Q2K");
        assert_eq!(desktop.os, "windows");
        assert_eq!(desktop.owner.as_deref(), Some("bob@example.com"));
        assert!(desktop.online);
        assert!(desktop.ssh_host_keys.is_empty());

        let shared_in = &status.devices[3];
        assert_eq!(
            shared_in,
            &TailscaleDevice {
                id: "nSHARED0011CNTRL".into(),
                name: "mary-server".into(),
                dns_name: Some("mary-server.fairy-otter.ts.net".into()),
                hostname: "tailscale".into(),
                os: "linux".into(),
                ips: ips(&["100.64.0.7", "fd7a:115c:a1e0::7"]),
                online: true,
                ssh_host_keys: vec![],
                tags: strings(&["tag:server", "tag:web"]),
                owner: None,
            }
        );

        let nas = &status.devices[4];
        assert_eq!(nas.id, "nNAS0000011CNTRL");
        assert_eq!(nas.dns_name.as_deref(), Some("nas.tail1234.ts.net"));
        assert_eq!(nas.ips, ips(&["100.64.0.2", "fd7a:115c:a1e0::2"]));
        assert_eq!(nas.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(nas.ssh_host_keys.len(), 2);
        assert!(nas.ssh_host_keys[0].starts_with("ecdsa-sha2-nistp256 AAAA"));
        assert!(nas.ssh_host_keys[1].starts_with("ssh-ed25519 AAAA"));
    }

    #[test]
    fn serializes_camel_case() {
        let status = parse_status(FIXTURE).unwrap().unwrap();
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["magicDnsSuffix"], "tail1234.ts.net");
        let nas = &json["devices"][4];
        assert_eq!(nas["dnsName"], "nas.tail1234.ts.net");
        assert_eq!(nas["ips"][0], "100.64.0.2");
        assert!(nas["sshHostKeys"].is_array());
    }

    #[test]
    fn backend_not_running_is_none() {
        for state in ["Stopped", "NeedsLogin", "NoState", ""] {
            let json = format!(r#"{{"BackendState": "{state}", "Peer": null, "Self": null}}"#);
            assert_eq!(parse_status(&json).unwrap(), None, "state {state:?}");
        }
        assert_eq!(parse_status("{}").unwrap(), None);
    }

    #[test]
    fn minimal_json_with_nulls_parses() {
        let json = r#"{
            "BackendState": "Running",
            "MagicDNSSuffix": "",
            "User": null,
            "Peer": {
                "k1": {"ID": "n1", "HostName": "Zeta", "TailscaleIPs": ["fd7a:115c:a1e0::9", "100.64.0.9"], "Tags": null, "sshHostKeys": null, "UserID": 42},
                "k2": {"ID": "n2", "HostName": "alpha", "DNSName": "", "TailscaleIPs": ["100.64.0.10", "not-an-ip"]},
                "k3": {"ID": "n3", "HostName": "ghost", "DNSName": "", "TailscaleIPs": null},
                "k4": {"ID": "n4", "HostName": "phone", "OS": "IOS", "TailscaleIPs": ["100.64.0.11"]},
                "k5": {"ID": "n5", "HostName": "", "OS": "freebsd", "TailscaleIPs": ["100.64.0.12"], "Online": null}
            }
        }"#;
        let status = parse_status(json).unwrap().unwrap();
        assert_eq!(status.magic_dns_suffix, None);
        let summary: Vec<(&str, &str, Vec<IpAddr>)> = status
            .devices
            .iter()
            .map(|d| (d.id.as_str(), d.name.as_str(), d.ips.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                ("n5", "100.64.0.12", ips(&["100.64.0.12"])),
                ("n2", "alpha", ips(&["100.64.0.10"])),
                ("n1", "Zeta", ips(&["100.64.0.9", "fd7a:115c:a1e0::9"])),
            ]
        );
        let zeta = &status.devices[2];
        assert_eq!(zeta.dns_name, None);
        assert_eq!(zeta.owner, None);
        assert!(zeta.tags.is_empty() && zeta.ssh_host_keys.is_empty());
        assert!(!status.devices[0].online);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_status("").is_err());
        assert!(parse_status("failed to connect to local tailscaled").is_err());
        assert!(parse_status(r#"{"BackendState": 3}"#).is_err());
    }
}
