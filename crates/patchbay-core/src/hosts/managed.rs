//! `~/.ssh/patchbay.conf`, the ssh_config file holding the hosts created in
//! Patchbay, and the `Include` line in `~/.ssh/config` that lets `ssh` see
//! them.
//!
//! Patchbay owns the file, but people may edit it by hand: saving a host only
//! rewrites the options Patchbay manages (`HostName`, `User`, `Port`,
//! `IdentityFile`, `ProxyJump`) in that host's block and keeps every other
//! line. A host is managed when its block starts with `Host <one concrete
//! alias>`; other blocks (patterns, several aliases, `Match`) are left alone.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::persist;
use crate::ssh_config::{split_args, split_keyword};

/// Name of the file, next to `~/.ssh/config`.
pub(crate) const FILE_NAME: &str = "patchbay.conf";

const HEADER: &[&str] = &[
    "# Hosts saved in Patchbay.",
    "# You can edit this file: Patchbay only rewrites the HostName, User, Port,",
    "# IdentityFile and ProxyJump lines of the hosts you change in the app.",
];

const INCLUDE: &str = "Include patchbay.conf";
const INCLUDE_COMMENT: &str = "# Added by Patchbay: lets ssh use the hosts saved in Patchbay.";

/// Options Patchbay writes. Everything else in a block is kept as is.
const MANAGED: &[&str] = &["hostname", "user", "port", "identityfile", "proxyjump"];

/// The managed options of one host, as written in the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    pub alias: String,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_files: Vec<String>,
    pub proxy_jump: Option<String>,
}

/// The file as a list of lines, edited in place.
#[derive(Debug, Clone, Default)]
pub(crate) struct ManagedConfig {
    lines: Vec<String>,
    crlf: bool,
}

/// A `Host` / `Match` line and the lines up to the next one.
struct Block {
    start: usize,
    end: usize,
    /// Set for managed blocks (`Host <one concrete alias>`).
    alias: Option<String>,
}

impl ManagedConfig {
    pub(crate) fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
            crlf: text.contains("\r\n"),
        }
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        Ok(Self::parse(
            &persist::read_optional(path)?.unwrap_or_default(),
        ))
    }

    pub(crate) fn render(&self) -> String {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut out = self.lines.join(nl);
        out.push_str(nl);
        out
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        write_ssh_file(path, &self.render())
    }

    /// Managed hosts in file order. If an alias has several managed blocks,
    /// only the first counts (as it does for ssh).
    pub(crate) fn entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = Vec::new();
        for b in self.blocks() {
            if let Some(alias) = &b.alias
                && !out.iter().any(|e| e.alias.eq_ignore_ascii_case(alias))
            {
                out.push(self.entry(&b, alias));
            }
        }
        out
    }

    pub(crate) fn get(&self, alias: &str) -> Option<Entry> {
        let b = self.find(alias)?;
        let alias = b.alias.clone()?;
        Some(self.entry(&b, &alias))
    }

    /// Writes `entry`, replacing the block of `old_alias` (keeping its
    /// unmanaged lines) or appending a new block.
    pub(crate) fn upsert(&mut self, old_alias: Option<&str>, entry: &Entry) {
        let mut block = vec![format!("Host {}", quote(&entry.alias))];
        let mut opt = |key: &str, value: &str| block.push(format!("    {key} {}", quote(value)));
        if let Some(v) = &entry.hostname {
            opt("HostName", v);
        }
        if let Some(v) = &entry.user {
            opt("User", v);
        }
        if let Some(v) = entry.port {
            opt("Port", &v.to_string());
        }
        for v in &entry.identity_files {
            opt("IdentityFile", v);
        }
        if let Some(v) = &entry.proxy_jump {
            opt("ProxyJump", v);
        }

        match old_alias.and_then(|a| self.find(a)) {
            Some(b) => {
                let kept = self.lines[b.start + 1..b.end]
                    .iter()
                    .filter(|l| !is_managed_line(l))
                    .cloned();
                block.extend(kept);
                self.lines.splice(b.start..b.end, block);
            }
            None => {
                while self.lines.last().is_some_and(|l| l.trim().is_empty()) {
                    self.lines.pop();
                }
                if self.lines.is_empty() {
                    self.lines.extend(HEADER.iter().map(|l| (*l).to_owned()));
                }
                self.lines.push(String::new());
                self.lines.extend(block);
            }
        }
    }

    /// Removes the block of `alias`. Returns false if there is none.
    pub(crate) fn remove(&mut self, alias: &str) -> bool {
        let Some(b) = self.find(alias) else {
            return false;
        };
        self.lines.drain(b.start..b.end);
        // Don't leave two blank lines (or a trailing one) behind.
        let blank = |l: Option<&String>| l.is_none_or(|l| l.trim().is_empty());
        if b.start > 0 && blank(self.lines.get(b.start - 1)) && blank(self.lines.get(b.start)) {
            self.lines.remove(b.start - 1);
        }
        true
    }

    fn find(&self, alias: &str) -> Option<Block> {
        self.blocks().into_iter().find(|b| {
            b.alias
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(alias))
        })
    }

    fn blocks(&self) -> Vec<Block> {
        let mut out: Vec<Block> = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            let Some((keyword, rest)) = split_keyword(line) else {
                continue;
            };
            let keyword = keyword.to_ascii_lowercase();
            if keyword != "host" && keyword != "match" {
                continue;
            }
            if let Some(last) = out.last_mut() {
                last.end = i;
            }
            let alias = match (keyword.as_str(), split_args(rest).as_deref()) {
                ("host", Ok([alias])) if valid_alias(alias).is_ok() => Some(alias.clone()),
                _ => None,
            };
            out.push(Block {
                start: i,
                end: self.lines.len(),
                alias,
            });
        }
        out
    }

    fn entry(&self, b: &Block, alias: &str) -> Entry {
        let mut e = Entry {
            alias: alias.to_owned(),
            ..Entry::default()
        };
        for line in &self.lines[b.start + 1..b.end] {
            let Some((keyword, rest)) = split_keyword(line) else {
                continue;
            };
            let Some(value) = split_args(rest).ok().and_then(|a| a.into_iter().next()) else {
                continue;
            };
            // First value wins, except for IdentityFile, which accumulates.
            match keyword.to_ascii_lowercase().as_str() {
                "hostname" => {
                    e.hostname.get_or_insert(value);
                }
                "user" => {
                    e.user.get_or_insert(value);
                }
                "port" => {
                    if e.port.is_none() {
                        e.port = value.parse().ok();
                    }
                }
                "identityfile" => e.identity_files.push(value),
                "proxyjump" => {
                    e.proxy_jump.get_or_insert(value);
                }
                _ => {}
            }
        }
        e
    }
}

fn is_managed_line(line: &str) -> bool {
    split_keyword(line).is_some_and(|(k, _)| MANAGED.contains(&k.to_ascii_lowercase().as_str()))
}

/// Whether `alias` can be a `Host` alias that `ssh <alias>` and ProxyJump
/// can refer to. The error says why not.
pub(crate) fn valid_alias(alias: &str) -> std::result::Result<(), &'static str> {
    if alias.is_empty() {
        Err("Name must not be empty")
    } else if alias.starts_with('-') {
        Err("Name must not start with '-'")
    } else if alias.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Err("Name must not contain spaces")
    } else if alias.contains(['*', '?', '!', ',', '"', '\'', '#', '=', '\\', '@']) {
        Err("Name must not contain any of * ? ! , \" ' # = \\ @")
    } else {
        Ok(())
    }
}

/// Quote a value for ssh_config if needed (OpenSSH `argv_split` rules).
pub(crate) fn quote(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value.starts_with('#')
        || value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'');
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len() + 2);
    if needs_quotes {
        out.push('"');
    }
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' | '\'' => {
                out.push('\\');
                out.push(c);
            }
            // A lone backslash is literal; only escape it where it would
            // otherwise combine with what follows.
            '\\' if chars
                .get(i + 1)
                .is_none_or(|n| matches!(n, '\\' | '"' | '\'') || n.is_whitespace()) =>
            {
                out.push_str("\\\\");
            }
            _ => out.push(c),
        }
    }
    if needs_quotes {
        out.push('"');
    }
    out
}

/// Adds `Include patchbay.conf` (with a comment) to the top of the ssh
/// config at `config`, creating the file if needed. Symlinks are followed,
/// so dotfile managers keep working.
pub(crate) fn add_include(config: &Path) -> Result<()> {
    let target = follow(config);
    let text = persist::read_optional(&target)?.unwrap_or_default();
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text.as_str()),
    };
    let nl = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = format!("{bom}{INCLUDE_COMMENT}{nl}{INCLUDE}{nl}");
    if !body.is_empty() {
        out.push_str(nl);
        out.push_str(body);
    }
    write_ssh_file(&target, &out)
}

/// Removes the lines [`add_include`] added (any `Include` of just
/// `patchbay.conf` and our comment). Returns false if there were none.
pub(crate) fn remove_include(config: &Path, managed: &Path) -> Result<bool> {
    let target = follow(config);
    let Some(text) = persist::read_optional(&target)? else {
        return Ok(false);
    };
    let is_ours = |line: &str| {
        if line.trim() == INCLUDE_COMMENT {
            return true;
        }
        let Some((keyword, rest)) = split_keyword(line) else {
            return false;
        };
        keyword.eq_ignore_ascii_case("include")
            && matches!(split_args(rest).as_deref(), Ok([arg]) if is_managed_path(arg, managed))
    };
    let mut removed = false;
    let mut out: Vec<&str> = Vec::new();
    let mut drop_blank = false;
    for line in text.split_inclusive('\n') {
        if is_ours(line.trim_end_matches(['\r', '\n'])) {
            removed = true;
            drop_blank = true;
            continue;
        }
        if drop_blank && line.trim().is_empty() {
            drop_blank = false;
            continue;
        }
        drop_blank = false;
        out.push(line);
    }
    if removed {
        write_ssh_file(&target, &out.concat())?;
    }
    Ok(removed)
}

fn is_managed_path(arg: &str, managed: &Path) -> bool {
    arg == FILE_NAME || arg == format!("~/.ssh/{FILE_NAME}") || Path::new(arg) == managed
}

/// The file a symlink points to (or `path` itself).
fn follow(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// Atomically replace a file under `~/.ssh`. Keeps the permissions of the
/// file being replaced; new files (and a new `~/.ssh`) are private to the
/// user, as OpenSSH insists.
fn write_ssh_file(path: &Path, text: &str) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        set_private(dir, 0o700)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        match fs::metadata(path) {
            Ok(meta) => fs::set_permissions(&tmp, meta.permissions())?,
            Err(_) => set_private(&tmp, 0o600)?,
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(unix)]
fn set_private(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_private(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(alias: &str, hostname: &str) -> Entry {
        Entry {
            alias: alias.into(),
            hostname: Some(hostname.into()),
            ..Entry::default()
        }
    }

    #[test]
    fn new_file_gets_header_and_blocks() {
        let mut m = ManagedConfig::parse("");
        m.upsert(
            None,
            &Entry {
                user: Some("ops".into()),
                port: Some(2222),
                identity_files: vec!["~/.ssh/a key".into(), "~/.ssh/b".into()],
                proxy_jump: Some("bastion".into()),
                ..entry("web", "web.example")
            },
        );
        m.upsert(None, &entry("db", "db.example"));
        let text = m.render();
        assert!(text.starts_with("# Hosts saved in Patchbay."));
        assert!(text.ends_with(
            "\nHost web\n    HostName web.example\n    User ops\n    Port 2222\n    \
             IdentityFile \"~/.ssh/a key\"\n    IdentityFile ~/.ssh/b\n    ProxyJump bastion\n\
             \nHost db\n    HostName db.example\n"
        ));
        let entries = ManagedConfig::parse(&text).entries();
        assert_eq!(entries[0].identity_files, ["~/.ssh/a key", "~/.ssh/b"]);
        assert_eq!(entries[0].port, Some(2222));
        assert_eq!(entries[1], entry("db", "db.example"));
    }

    #[test]
    fn update_keeps_unmanaged_lines_and_other_blocks() {
        let text = "# mine\r\nHost *.lan\r\n  User lanuser\r\n\r\nHost web\r\n  HostName old\r\n  \
                    ForwardAgent yes\r\n  # note\r\n  Port 1\r\n\r\nHost a b\r\n  User x\r\n";
        let mut m = ManagedConfig::parse(text);
        assert_eq!(
            m.entries().iter().map(|e| &e.alias).collect::<Vec<_>>(),
            ["web"]
        );
        m.upsert(
            Some("WEB"),
            &Entry {
                user: Some("me".into()),
                ..entry("site", "new")
            },
        );
        assert_eq!(
            m.render(),
            "# mine\r\nHost *.lan\r\n  User lanuser\r\n\r\nHost site\r\n    HostName new\r\n    \
             User me\r\n  ForwardAgent yes\r\n  # note\r\n\r\nHost a b\r\n  User x\r\n"
        );
        assert!(m.get("web").is_none());
        assert_eq!(m.get("Site").unwrap().user.as_deref(), Some("me"));
    }

    #[test]
    fn remove_block() {
        let mut m = ManagedConfig::parse("# h\n\nHost a\n  HostName x\n\nHost b\n  HostName y\n");
        assert!(m.remove("a"));
        assert_eq!(m.render(), "# h\n\nHost b\n  HostName y\n");
        assert!(m.remove("b"));
        assert_eq!(m.render(), "# h\n");
        assert!(!m.remove("b"));
    }

    #[test]
    fn quoting_round_trips() {
        for v in [
            "plain",
            "with space",
            r"C:\Users\Rod Smith\.ssh\id",
            r"\\server\share\key",
            r#"q"uote"#,
            "it's",
            r"trailing\",
            "#hash",
            "",
        ] {
            let quoted = quote(v);
            assert_eq!(split_args(&quoted).unwrap(), [v], "{v} -> {quoted}");
        }
        assert_eq!(quote(r"C:\Users\me\.ssh\id"), r"C:\Users\me\.ssh\id");
    }

    #[test]
    fn aliases() {
        for ok in ["web", "web-1.example", "Pi_4"] {
            assert!(valid_alias(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "a b", "a*", "a?", "!a", "a,b", "u@h", "a=b", "a#"] {
            assert!(valid_alias(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn include_line_is_added_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let managed = dir.path().join(FILE_NAME);
        let cfg = dir.path().join("config");
        add_include(&cfg).unwrap();
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            format!("{INCLUDE_COMMENT}\n{INCLUDE}\n")
        );
        assert!(remove_include(&cfg, &managed).unwrap());
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "");

        fs::write(&cfg, "Host a\r\n  User b\r\n").unwrap();
        add_include(&cfg).unwrap();
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            format!("{INCLUDE_COMMENT}\r\n{INCLUDE}\r\n\r\nHost a\r\n  User b\r\n")
        );
        assert!(remove_include(&cfg, &managed).unwrap());
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "Host a\r\n  User b\r\n");
        assert!(!remove_include(&cfg, &managed).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn writes_follow_symlinks_and_keep_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles-config");
        fs::write(&real, "Host a\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let ssh = dir.path().join("ssh");
        let cfg = ssh.join("config");
        fs::create_dir(&ssh).unwrap();
        symlink(&real, &cfg).unwrap();
        add_include(&cfg).unwrap();
        assert!(fs::symlink_metadata(&cfg).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(&real).unwrap().contains(INCLUDE));
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);

        let fresh = dir.path().join("new/.ssh/config");
        add_include(&fresh).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&fresh), 0o600);
        assert_eq!(mode(fresh.parent().unwrap()), 0o700);
    }
}
