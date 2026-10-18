//! Reading and appending OpenSSH `known_hosts` files.
//!
//! Supports plain and hashed (`|1|salt|hash`) host fields, `[host]:port`
//! entries, comma-separated pattern lists with `*` / `?` / `!negation`, and
//! the `@revoked` marker. `@cert-authority` lines are skipped (host
//! certificates are not supported).
//!
//! CONTRACT (used by `session`): keep these signatures stable.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use data_encoding::BASE64;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{Algorithm, HashAlg, PublicKey};
use sha1::Sha1;

use crate::ssh_config::wildcard_match;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// A matching key is recorded.
    Known,
    /// No entry for this host (with any key type that conflicts).
    Unknown,
    /// An entry for this host with the same key type but a different key.
    Changed { file: PathBuf, line: usize },
    /// Key is marked `@revoked`.
    Revoked { file: PathBuf, line: usize },
}

/// Check `key` for `host:port` against the given known_hosts files (missing
/// files are skipped). `host` is the HostKeyAlias if set, else the hostname.
///
/// Precedence across all files: `Revoked` > `Known` > `Changed` (first
/// conflicting entry) > `Unknown`. Entries for the host with only other key
/// algorithms yield `Unknown`.
pub fn check(files: &[PathBuf], host: &str, port: u16, key: &PublicKey) -> Result<HostKeyStatus> {
    let name = host_field(host, port);
    let mut known = false;
    let mut changed: Option<(PathBuf, usize)> = None;

    for file in files {
        let Some(text) = read_known_hosts(file)? else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let Some(entry) = Entry::parse(line) else {
                continue;
            };
            if entry.marker == Marker::CertAuthority || !entry.matches_host(&name) {
                continue;
            }
            let Some(recorded) = entry.key() else {
                continue;
            };
            let same_key = recorded.key_data() == key.key_data();
            match entry.marker {
                Marker::Revoked if same_key => {
                    return Ok(HostKeyStatus::Revoked {
                        file: file.clone(),
                        line: idx + 1,
                    });
                }
                Marker::Revoked | Marker::CertAuthority => {}
                Marker::None if same_key => known = true,
                Marker::None => {
                    if changed.is_none() && recorded.algorithm() == key.algorithm() {
                        changed = Some((file.clone(), idx + 1));
                    }
                }
            }
        }
    }

    Ok(match (known, changed) {
        (true, _) => HostKeyStatus::Known,
        (false, Some((file, line))) => HostKeyStatus::Changed { file, line },
        (false, None) => HostKeyStatus::Unknown,
    })
}

/// Key algorithms recorded for `host:port` (same matching rules as
/// [`check`]; `@revoked` and `@cert-authority` lines are ignored), in order of
/// first appearance and without duplicates. Used to prefer host key
/// algorithms we can already verify, as OpenSSH does. RSA keys are reported
/// as plain `ssh-rsa` ([`Algorithm::Rsa`] with no hash); any RSA signature
/// variant verifies against them.
pub fn recorded_algorithms(files: &[PathBuf], host: &str, port: u16) -> Result<Vec<Algorithm>> {
    let name = host_field(host, port);
    let mut algorithms = Vec::new();
    for file in files {
        let Some(text) = read_known_hosts(file)? else {
            continue;
        };
        for line in text.lines() {
            let Some(entry) = Entry::parse(line) else {
                continue;
            };
            if entry.marker != Marker::None || !entry.matches_host(&name) {
                continue;
            }
            if let Some(key) = entry.key()
                && !algorithms.contains(&key.algorithm())
            {
                algorithms.push(key.algorithm());
            }
        }
    }
    Ok(algorithms)
}

/// Contents of a known_hosts file; `None` if it does not exist.
fn read_known_hosts(file: &Path) -> Result<Option<String>> {
    match fs::read(file) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::ReadFile {
            path: file.to_path_buf(),
            source,
        }),
    }
}

/// Append an entry for `host:port` (`[host]:port` when port != 22), creating
/// the file and its parent directory with private permissions if needed.
pub fn append(file: &Path, host: &str, port: u16, key: &PublicKey) -> Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    let mut f = open_private_append(file)?;
    let needs_newline = {
        let len = f.metadata()?.len();
        if len == 0 {
            false
        } else {
            let mut last = [0u8; 1];
            f.seek(SeekFrom::Start(len - 1))?;
            f.read_exact(&mut last)?;
            last[0] != b'\n'
        }
    };
    let encoded = PublicKey::new(key.key_data().clone(), "")
        .to_openssh()
        .map_err(russh::keys::Error::from)?;
    let mut out = String::new();
    if needs_newline {
        out.push('\n');
    }
    out.push_str(&host_field(host, port));
    out.push(' ');
    out.push_str(encoded.trim_end());
    out.push('\n');
    f.write_all(out.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// `SHA256:...` fingerprint as printed by OpenSSH.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// The name OpenSSH looks up / records: lowercased host, bracketed with the
/// port when it is not 22.
fn host_field(host: &str, port: u16) -> String {
    let host = host.to_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

fn open_private_append(file: &Path) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(file)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    None,
    Revoked,
    CertAuthority,
}

struct Entry<'a> {
    marker: Marker,
    hosts: &'a str,
    key_type: &'a str,
    key_b64: &'a str,
}

impl<'a> Entry<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut fields = line.split_whitespace();
        let mut first = fields.next()?;
        let marker = if first.starts_with('@') {
            let m = match first {
                "@revoked" => Marker::Revoked,
                "@cert-authority" => Marker::CertAuthority,
                _ => return None,
            };
            first = fields.next()?;
            m
        } else {
            Marker::None
        };
        Some(Self {
            marker,
            hosts: first,
            key_type: fields.next()?,
            key_b64: fields.next()?,
        })
    }

    fn matches_host(&self, name: &str) -> bool {
        if let Some(hashed) = self.hosts.strip_prefix("|1|") {
            return hashed_matches(hashed, name);
        }
        let mut positive = false;
        for pattern in self.hosts.split(',') {
            let (negated, pattern) = match pattern.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, pattern),
            };
            if wildcard_match(&pattern.to_lowercase(), name) {
                if negated {
                    return false;
                }
                positive = true;
            }
        }
        positive
    }

    fn key(&self) -> Option<PublicKey> {
        PublicKey::from_openssh(&format!("{} {}", self.key_type, self.key_b64)).ok()
    }
}

/// `salt|hash` (both base64) where hash = HMAC-SHA1(key = salt, name).
fn hashed_matches(field: &str, name: &str) -> bool {
    let Some((salt, hash)) = field.split_once('|') else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (
        BASE64.decode(salt.as_bytes()),
        BASE64.decode(hash.as_bytes()),
    ) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(name.as_bytes());
    mac.verify_slice(&hash).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICzv4YpCBPOrdov3w09nlclbSGxDFLQ63CyBUrzVifqq";
    const ED_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDTDI5X/QFvCxbEWVv+EgnFBFJdX/38nZSMkUhpLbO5o";
    const ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBGNaokTp50LVhKCTqVsSeldcM/NLyfGdtOrRfGkwpgKv2GmiIbHqewSAYzK9X1aRDaDg+VtDHTYISLPeTRCU040=";

    fn key(s: &str) -> PublicKey {
        PublicKey::from_openssh(s).unwrap()
    }

    fn hashed(name: &str, salt: &[u8]) -> String {
        let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(salt).unwrap();
        mac.update(name.as_bytes());
        let hash = mac.finalize().into_bytes();
        format!("|1|{}|{}", BASE64.encode(salt), BASE64.encode(&hash))
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn fingerprint_matches_ssh_keygen() {
        assert_eq!(
            fingerprint(&key(ED_A)),
            "SHA256:C3scAF1yxydIVD9W3zJ4Wl8RGrjr9hasEVtSh8OBHiQ"
        );
        assert_eq!(
            fingerprint(&key(ECDSA)),
            "SHA256:2xYPxbLuAwKiOMy9AsNi1Qh/KhWQci8mrrJYzfMTDtw"
        );
    }

    #[test]
    fn hashed_entry_from_ssh_keygen() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(
            dir.path(),
            "kh",
            &format!("|1|A5d3ESYm9FIkh9r4X+j7K8Z/94E=|F+qlhaItJygZnTqHvZ3/S+D4MOE= {ED_A}\n"),
        );
        let files = [f];
        assert_eq!(
            check(&files, "host.example", 2200, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "host.example", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Unknown
        );
    }

    #[test]
    fn plain_entries() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(
            dir.path(),
            "kh",
            &format!("# comment\n\nexample.com,10.0.0.1 {ED_A} c\n[example.com]:2222 {ED_B}\n"),
        );
        let files = [dir.path().join("missing"), f.clone()];
        assert_eq!(
            check(&files, "example.com", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "EXAMPLE.com", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "10.0.0.1", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "example.com", 22, &key(ED_B)).unwrap(),
            HostKeyStatus::Changed {
                file: f.clone(),
                line: 3
            }
        );
        assert_eq!(
            check(&files, "example.com", 2222, &key(ED_B)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "example.com", 2222, &key(ED_A)).unwrap(),
            HostKeyStatus::Changed {
                file: f.clone(),
                line: 4
            }
        );
        assert_eq!(
            check(&files, "other", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            check(&files, "example.com", 22, &key(ECDSA)).unwrap(),
            HostKeyStatus::Unknown
        );
    }

    #[test]
    fn hashed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(
            dir.path(),
            "kh",
            &format!(
                "{} {ED_A}\n{} {ED_A}\n",
                hashed("host.example", b"0123456789abcdefghij"),
                hashed("[host.example]:2200", b"jihgfedcba9876543210"),
            ),
        );
        let files = [f.clone()];
        assert_eq!(
            check(&files, "host.example", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "host.example", 2200, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "host.example", 22, &key(ED_B)).unwrap(),
            HostKeyStatus::Changed { file: f, line: 1 }
        );
        assert_eq!(
            check(&files, "host.example", 2201, &key(ED_A)).unwrap(),
            HostKeyStatus::Unknown
        );
    }

    #[test]
    fn patterns_markers_and_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = write(
            dir.path(),
            "a",
            &format!(
                "*.corp,!secret.corp {ED_A}\n@cert-authority *.corp {ED_B}\n@revoked * {ECDSA}\n\
                 @bogus x {ED_A}\nbroken-line\nweb.corp ssh-ed25519 notbase64!!\n"
            ),
        );
        let f2 = write(dir.path(), "b", &format!("web.corp {ED_B}\n"));
        let files = [f1.clone(), f2.clone()];
        assert_eq!(
            check(&files, "web.corp", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "web.corp", 22, &key(ED_B)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "secret.corp", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(
            check(&files, "db.corp", 22, &key(ED_B)).unwrap(),
            HostKeyStatus::Changed {
                file: f1.clone(),
                line: 1
            }
        );
        assert_eq!(
            check(&files, "anything", 22, &key(ECDSA)).unwrap(),
            HostKeyStatus::Revoked { file: f1, line: 3 }
        );
    }

    const RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQC33cVbJNix/wkx2qwiEhnV0N+Um0+ZaZY7EcGePomfC54XPWGnvNJ0QG/PmzlSyOAyVnX1gvwfCk/iJgqZ5ee0/1h00Y3eQvd86jPvM77zck/WCsiOmMATUuHkWl7E/fUJe4bdGvwo9JxMnf/OC0R4C6dY1Nw4TbCMMWpY9rjHnQ==";

    #[test]
    fn recorded_algorithms_for_host() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = write(
            dir.path(),
            "a",
            &format!(
                "other {ED_A}\n{} {ECDSA}\n@revoked host.example {ED_B}\n\
                 @cert-authority *.example {ED_A}\n*.example,!skip.example {RSA}\n",
                hashed("host.example", b"0123456789abcdefghij")
            ),
        );
        let f2 = write(
            dir.path(),
            "b",
            &format!("host.example {ECDSA}\n[host.example]:2222 {ED_A}\nhost.example {ED_B}\n"),
        );
        let files = [dir.path().join("missing"), f1, f2];
        let ecdsa = key(ECDSA).algorithm();
        let rsa = key(RSA).algorithm();
        assert_eq!(rsa, Algorithm::Rsa { hash: None });
        assert_eq!(
            recorded_algorithms(&files, "Host.Example", 22).unwrap(),
            [ecdsa, rsa, Algorithm::Ed25519]
        );
        assert_eq!(
            recorded_algorithms(&files, "host.example", 2222).unwrap(),
            [Algorithm::Ed25519]
        );
        assert_eq!(recorded_algorithms(&files, "skip.example", 22).unwrap(), []);
        assert_eq!(recorded_algorithms(&files, "nowhere", 22).unwrap(), []);
    }

    #[test]
    fn append_creates_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new/.ssh/known_hosts");
        append(&file, "Host.Example", 22, &key(ED_A)).unwrap();
        append(&file, "host.example", 2222, &key(ECDSA)).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            format!("host.example {ED_A}\n[host.example]:2222 {ECDSA}\n")
        );
        let files = [file.clone()];
        assert_eq!(
            check(&files, "host.example", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Known
        );
        assert_eq!(
            check(&files, "host.example", 2222, &key(ECDSA)).unwrap(),
            HostKeyStatus::Known
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file), 0o600);
            assert_eq!(mode(file.parent().unwrap()), 0o700);
        }
    }

    #[test]
    fn append_adds_missing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "kh", &format!("a {ED_B}"));
        append(&file, "b", 22, &key(ED_A)).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            format!("a {ED_B}\nb {ED_A}\n")
        );
    }
}
