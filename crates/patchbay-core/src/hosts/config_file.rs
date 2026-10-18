//! Line-level edits to an ssh_config file (`~/.ssh/config` or one of its
//! includes).
//!
//! The file belongs to the user, so edits are as small as possible: saving a
//! host only touches the `Host` line and the `HostName`, `User`, `Port`,
//! `IdentityFile` and `ProxyJump` lines of that host's own block, keeping
//! their indentation and spelling. Comments, blank lines, other options and
//! other blocks are left alone. A host can be edited when its block starts
//! with `Host <alias>` naming just that alias.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::persist;
use crate::ssh_config::{split_args, split_keyword};

/// Where hosts used to be saved (Patchbay 0.1), next to `~/.ssh/config`.
pub(crate) const LEGACY_FILE_NAME: &str = "patchbay.conf";

/// The comment Patchbay 0.1 put above `Include patchbay.conf`.
const INCLUDE_COMMENT: &str = "# Added by Patchbay: lets ssh use the hosts saved in Patchbay.";

/// The header Patchbay 0.1 wrote at the top of patchbay.conf.
const LEGACY_HEADER: &[&str] = &[
    "# Hosts saved in Patchbay.",
    "# You can edit this file: Patchbay only rewrites the HostName, User, Port,",
    "# IdentityFile and ProxyJump lines of the hosts you change in the app.",
];

const DEFAULT_INDENT: &str = "    ";

/// The options of one host Patchbay edits, as written in its block.
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
pub(crate) struct ConfigFile {
    lines: Vec<String>,
    crlf: bool,
    bom: bool,
}

/// A `Host` / `Match` line and the lines up to the next one.
#[derive(Debug, Clone, Copy)]
struct Block {
    start: usize,
    end: usize,
}

/// A `Keyword value` line taken apart, so a new value can be put back in
/// the same style (`  User bob`, `user=bob`, ...).
struct Directive<'a> {
    /// Everything before the value: indentation, keyword and separator.
    head: &'a str,
    keyword: String,
    args: Vec<String>,
}

fn directive(line: &str) -> Option<Directive<'_>> {
    let (keyword, rest) = split_keyword(line)?;
    let args = split_args(rest).ok()?;
    let head = &line[..line.len() - rest.len()];
    Some(Directive {
        head,
        keyword: keyword.to_ascii_lowercase(),
        args,
    })
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

impl ConfigFile {
    pub(crate) fn parse(text: &str) -> Self {
        let (bom, text) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        Self {
            lines: text.lines().map(str::to_owned).collect(),
            crlf: text.contains("\r\n"),
            bom,
        }
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        Ok(Self::parse(
            &persist::read_optional(path)?.unwrap_or_default(),
        ))
    }

    pub(crate) fn render(&self) -> String {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        out.push_str(&self.lines.join(nl));
        if !self.lines.is_empty() {
            out.push_str(nl);
        }
        out
    }

    /// Writes the file (following a symlink to it).
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        write_ssh_file(&follow(path), &self.render())
    }

    /// Hosts with a block of their own, in file order (first block per
    /// alias, as ssh uses).
    pub(crate) fn entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = Vec::new();
        for b in self.blocks() {
            if let Some(alias) = self.sole_alias(b)
                && !out.iter().any(|e| e.alias.eq_ignore_ascii_case(&alias))
            {
                out.push(self.entry_of(b, alias));
            }
        }
        out
    }

    /// The options written for `alias`, if its first `Host` line in this
    /// file names only it.
    pub(crate) fn get(&self, alias: &str) -> Option<Entry> {
        let b = self.find(alias)?;
        let alias = self.sole_alias(b)?;
        Some(self.entry_of(b, alias))
    }

    /// Rewrites the block of `alias` to hold `entry` (renaming it if the
    /// aliases differ). Returns false if `alias` has no block of its own.
    pub(crate) fn update(&mut self, alias: &str, entry: &Entry) -> bool {
        let Some(b) = self.find(alias) else {
            return false;
        };
        if self.sole_alias(b).is_none() {
            return false;
        }
        if !entry.alias.eq(alias)
            && let Some(d) = directive(&self.lines[b.start])
        {
            self.lines[b.start] = format!("{}{}", d.head, quote(&entry.alias));
        }

        let indent = self.block_indent(b);
        let body: Vec<String> = self.lines[b.start + 1..b.end].to_vec();
        let mut edit = BodyEdit::new(body, indent);
        edit.set_single("hostname", "HostName", entry.hostname.as_deref());
        edit.set_single("user", "User", entry.user.as_deref());
        edit.set_single("port", "Port", entry.port.map(|p| p.to_string()).as_deref());
        edit.set_list("identityfile", "IdentityFile", &entry.identity_files);
        edit.set_single("proxyjump", "ProxyJump", entry.proxy_jump.as_deref());
        self.lines.splice(b.start + 1..b.end, edit.finish());
        true
    }

    /// Adds a block for `entry` (see [`insert_lines`](Self::insert_lines)).
    pub(crate) fn insert(&mut self, entry: &Entry) {
        let indent = self.file_indent();
        let mut block = vec![format!("Host {}", quote(&entry.alias))];
        let mut opt =
            |key: &str, value: &str| block.push(format!("{indent}{key} {}", quote(value)));
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

        self.insert_lines(block);
    }

    /// Removes the block of `alias`: its `Host` line through its last
    /// option (comments after that are likely about what follows). Returns
    /// false if `alias` has no block of its own.
    pub(crate) fn remove(&mut self, alias: &str) -> bool {
        let Some(b) = self.find(alias) else {
            return false;
        };
        if self.sole_alias(b).is_none() {
            return false;
        }
        let end = (b.start + 1..b.end)
            .rev()
            .find(|&i| !is_blank(&self.lines[i]) && !is_comment(&self.lines[i]))
            .map_or(b.start + 1, |i| i + 1);
        self.lines.drain(b.start..end);
        // Don't leave two blank lines (or a leading/trailing one) behind.
        let at = b.start;
        let blank = |l: Option<&String>| l.is_some_and(|l| is_blank(l));
        while blank(self.lines.get(at))
            && (at == 0 || blank(self.lines.get(at - 1)) || at + 1 >= self.lines.len())
        {
            self.lines.remove(at);
        }
        while self.lines.last().is_some_and(|l| is_blank(l)) {
            self.lines.pop();
        }
        true
    }

    /// Points `ProxyJump` hops to `old` at `new` instead. Returns whether
    /// anything changed.
    pub(crate) fn rename_jumps(&mut self, old: &str, new: &str) -> bool {
        let mut changed = false;
        for line in &mut self.lines {
            let Some(d) = directive(line) else { continue };
            if d.keyword != "proxyjump" {
                continue;
            }
            if let [spec] = d.args.as_slice()
                && let Some(renamed) = super::rename_hop(spec, old, new)
            {
                *line = format!("{}{}", d.head, quote(&renamed));
                changed = true;
            }
        }
        changed
    }

    /// The lines Patchbay 0.1 wrote at the top of patchbay.conf removed, for
    /// moving the rest elsewhere.
    pub(crate) fn without_legacy_header(&self) -> Vec<String> {
        let mut lines = self.lines.as_slice();
        for h in LEGACY_HEADER {
            match lines.split_first() {
                Some((first, rest)) if first.trim() == *h => lines = rest,
                _ => break,
            }
        }
        let start = lines
            .iter()
            .position(|l| !is_blank(l))
            .unwrap_or(lines.len());
        let end = lines
            .iter()
            .rposition(|l| !is_blank(l))
            .map_or(start, |i| i + 1);
        lines[start..end].to_vec()
    }

    /// Adds `lines` (blocks): before the first block with patterns (`Host
    /// *`, `Host *.lan`, `Match ...`), whose values would otherwise win over
    /// the new ones, else at the end.
    pub(crate) fn insert_lines(&mut self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        let mut block = lines;
        match self.first_pattern_block() {
            Some(mut at) => {
                // Keep the comment above that block with it.
                while at > 0 && is_comment(&self.lines[at - 1]) {
                    at -= 1;
                }
                block.push(String::new());
                if at > 0 && !is_blank(&self.lines[at - 1]) {
                    block.insert(0, String::new());
                }
                self.lines.splice(at..at, block);
            }
            None => {
                while self.lines.last().is_some_and(|l| is_blank(l)) {
                    self.lines.pop();
                }
                if !self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.lines.extend(block);
            }
        }
    }

    /// The first block whose `Host` line lists `alias`.
    fn find(&self, alias: &str) -> Option<Block> {
        self.blocks().into_iter().find(|b| {
            directive(&self.lines[b.start]).is_some_and(|d| {
                d.keyword == "host" && d.args.iter().any(|a| a.eq_ignore_ascii_case(alias))
            })
        })
    }

    /// The alias of a `Host <one concrete alias>` block.
    fn sole_alias(&self, b: Block) -> Option<String> {
        let d = directive(&self.lines[b.start])?;
        match (d.keyword.as_str(), d.args.as_slice()) {
            ("host", [alias]) if valid_alias(alias).is_ok() => Some(alias.clone()),
            _ => None,
        }
    }

    /// Start of the first `Match` block or `Host` block with a wildcard.
    fn first_pattern_block(&self) -> Option<usize> {
        self.blocks().into_iter().map(|b| b.start).find(|&i| {
            directive(&self.lines[i]).is_some_and(|d| {
                d.keyword == "match" || d.args.iter().any(|a| a.contains(['*', '?']))
            })
        })
    }

    fn blocks(&self) -> Vec<Block> {
        let mut out: Vec<Block> = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            let Some((keyword, _)) = split_keyword(line) else {
                continue;
            };
            if !keyword.eq_ignore_ascii_case("host") && !keyword.eq_ignore_ascii_case("match") {
                continue;
            }
            if let Some(last) = out.last_mut() {
                last.end = i;
            }
            out.push(Block {
                start: i,
                end: self.lines.len(),
            });
        }
        out
    }

    fn entry_of(&self, b: Block, alias: String) -> Entry {
        let mut e = Entry {
            alias,
            ..Entry::default()
        };
        for line in &self.lines[b.start + 1..b.end] {
            let Some(d) = directive(line) else { continue };
            let Some(value) = d.args.into_iter().next() else {
                continue;
            };
            // First value wins, except for IdentityFile, which accumulates.
            match d.keyword.as_str() {
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

    /// Indentation of the options in block `b`, else the file's.
    fn block_indent(&self, b: Block) -> String {
        self.lines[b.start + 1..b.end]
            .iter()
            .find(|l| !is_blank(l) && !is_comment(l))
            .map(|l| indent_of(l).to_owned())
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| self.file_indent())
    }

    /// Indentation of the first indented option in the file.
    fn file_indent(&self) -> String {
        self.lines
            .iter()
            .filter(|l| !is_blank(l) && !is_comment(l))
            .map(|l| indent_of(l))
            .find(|i| !i.is_empty())
            .unwrap_or(DEFAULT_INDENT)
            .to_owned()
    }
}

fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Edits to the option lines of one block. Lines are replaced or removed in
/// place; new ones go after the last option.
struct BodyEdit {
    lines: Vec<Option<String>>,
    /// `before[i]`: new lines to put before `lines[i]` (`before[len]`: at
    /// the end).
    before: Vec<Vec<String>>,
    indent: String,
    /// Where new options go: after the last option line.
    tail: usize,
}

impl BodyEdit {
    fn new(lines: Vec<String>, indent: String) -> Self {
        let tail = lines
            .iter()
            .rposition(|l| !is_blank(l) && !is_comment(l))
            .map_or(0, |i| i + 1);
        Self {
            before: vec![Vec::new(); lines.len() + 1],
            lines: lines.into_iter().map(Some).collect(),
            indent,
            tail,
        }
    }

    fn positions(&self, keyword: &str) -> Vec<usize> {
        (0..self.lines.len())
            .filter(|&i| {
                self.lines[i]
                    .as_deref()
                    .and_then(directive)
                    .is_some_and(|d| d.keyword == keyword)
            })
            .collect()
    }

    fn value_at(&self, i: usize) -> Option<String> {
        self.lines[i]
            .as_deref()
            .and_then(directive)
            .and_then(|d| d.args.into_iter().next())
    }

    fn rewrite(&mut self, i: usize, value: &str) {
        if self.value_at(i).as_deref() == Some(value) {
            return;
        }
        if let Some(line) = &self.lines[i]
            && let Some(d) = directive(line)
        {
            self.lines[i] = Some(format!("{}{}", d.head, quote(value)));
        }
    }

    fn new_line(&self, name: &str, value: &str) -> String {
        format!("{}{name} {}", self.indent, quote(value))
    }

    /// A single-valued option: the first line counts (later ones are
    /// ignored by ssh and left alone), `None` removes them all.
    fn set_single(&mut self, keyword: &str, name: &str, value: Option<&str>) {
        let at = self.positions(keyword);
        match (at.first(), value) {
            (Some(&i), Some(v)) => self.rewrite(i, v),
            (Some(_), None) => {
                for i in at {
                    self.lines[i] = None;
                }
            }
            (None, Some(v)) => {
                let line = self.new_line(name, v);
                self.before[self.tail].push(line);
            }
            (None, None) => {}
        }
    }

    /// An accumulating option: lines are matched up in order.
    fn set_list(&mut self, keyword: &str, name: &str, values: &[String]) {
        let at = self.positions(keyword);
        let current: Vec<Option<String>> = at.iter().map(|&i| self.value_at(i)).collect();
        if current
            .iter()
            .map(Option::as_deref)
            .eq(values.iter().map(|v| Some(v.as_str())))
        {
            return;
        }
        for (k, &i) in at.iter().enumerate() {
            match values.get(k) {
                Some(v) => self.rewrite(i, v),
                None => self.lines[i] = None,
            }
        }
        // Right after the last one (ahead of other new options there).
        let after = at.last().map_or(self.tail, |&i| i + 1);
        let new: Vec<String> = values
            .iter()
            .skip(at.len())
            .map(|v| self.new_line(name, v))
            .collect();
        self.before[after].splice(0..0, new);
    }

    fn finish(self) -> Vec<String> {
        let mut out = Vec::new();
        let mut before = self.before.into_iter();
        for line in self.lines {
            out.extend(before.next().unwrap_or_default());
            out.extend(line);
        }
        out.extend(before.next().unwrap_or_default());
        out
    }
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

/// Removes `Include patchbay.conf` (any spelling of that path) and the
/// comment Patchbay 0.1 put above it from `config`'s lines.
pub(crate) fn remove_legacy_include(config: &mut ConfigFile, legacy: &Path) {
    let is_ours = |line: &str| {
        if line.trim() == INCLUDE_COMMENT {
            return true;
        }
        directive(line).is_some_and(|d| {
            d.keyword == "include"
                && matches!(d.args.as_slice(), [arg] if is_legacy_path(arg, legacy))
        })
    };
    let mut out: Vec<String> = Vec::new();
    let mut drop_blank = false;
    for line in std::mem::take(&mut config.lines) {
        if is_ours(&line) {
            drop_blank = true;
            continue;
        }
        if drop_blank && is_blank(&line) {
            drop_blank = false;
            continue;
        }
        drop_blank = false;
        out.push(line);
    }
    config.lines = out;
}

fn is_legacy_path(arg: &str, legacy: &Path) -> bool {
    arg == LEGACY_FILE_NAME
        || arg == format!("~/.ssh/{LEGACY_FILE_NAME}")
        || Path::new(arg) == legacy
}

/// Why `path` (an ssh_config file, followed if a symlink) can't be written,
/// or `None` if it can.
pub(crate) fn write_problem(path: &Path) -> Option<String> {
    let target = follow(path);
    match fs::metadata(&target) {
        Ok(meta) => {
            if meta.permissions().readonly()
                || fs::OpenOptions::new().append(true).open(&target).is_err()
            {
                return Some("the file is read-only".into());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Some(format!("the file can't be read ({e})")),
    }
    // Saving replaces the file, which needs a writable directory (the
    // nearest existing one, if it has to be created).
    let mut dir = target.parent().map(Path::to_owned);
    while let Some(d) = dir {
        if d.exists() {
            return (!dir_writable(&d)).then(|| format!("{} is read-only", d.display()));
        }
        dir = d.parent().map(Path::to_owned);
    }
    None
}

#[cfg(unix)]
fn dir_writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Windows has no useful read-only flag on folders (Explorer sets it on
/// customized ones); a failed save reports the problem instead.
#[cfg(not(unix))]
fn dir_writable(_dir: &Path) -> bool {
    true
}

/// The file a symlink points to (or `path` itself).
pub(crate) fn follow(path: &Path) -> PathBuf {
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

    fn edited(text: &str, f: impl FnOnce(&mut ConfigFile)) -> String {
        let mut c = ConfigFile::parse(text);
        f(&mut c);
        c.render()
    }

    #[test]
    fn insert_goes_before_pattern_blocks() {
        let full = Entry {
            user: Some("ops".into()),
            port: Some(2222),
            identity_files: vec!["~/.ssh/a key".into(), "~/.ssh/b".into()],
            proxy_jump: Some("bastion".into()),
            ..entry("web", "web.example")
        };
        assert_eq!(
            edited("", |c| c.insert(&full)),
            "Host web\n    HostName web.example\n    User ops\n    Port 2222\n    \
             IdentityFile \"~/.ssh/a key\"\n    IdentityFile ~/.ssh/b\n    ProxyJump bastion\n"
        );
        // Appended, with the file's indentation, when there are no patterns.
        assert_eq!(
            edited("Host a\n  User x\n\n\n", |c| c.insert(&entry("b", "b.lan"))),
            "Host a\n  User x\n\nHost b\n  HostName b.lan\n"
        );
        // Before the first pattern and the comment above it.
        assert_eq!(
            edited(
                "Include x\nHost a\n\tUser x\n# defaults\nHost * !c\n\tUser y\n",
                |c| c.insert(&entry("b", "b.lan"))
            ),
            "Include x\nHost a\n\tUser x\n\nHost b\n\tHostName b.lan\n\n# defaults\nHost * !c\n\tUser y\n"
        );
        assert_eq!(
            edited("User g\r\nMatch all\r\n  User y\r\n", |c| c
                .insert(&entry("b", "b.lan"))),
            "User g\r\n\r\nHost b\r\n  HostName b.lan\r\n\r\nMatch all\r\n  User y\r\n"
        );
        assert_eq!(
            edited("Host a\nHost x?\nHost *\n", |c| c
                .insert(&entry("b", "b.lan"))),
            "Host a\n\nHost b\n    HostName b.lan\n\nHost x?\nHost *\n"
        );
        let entries = ConfigFile::parse(&edited("", |c| c.insert(&full))).entries();
        assert_eq!(entries, [full]);
    }

    #[test]
    fn update_edits_lines_in_place() {
        let text = "# mine\r\nHost *.lan\r\n  User lanuser\r\n\r\nhost web\r\n  hostname=old\r\n  \
                    ForwardAgent yes\r\n  # note\r\n  Port 1\r\n  IdentityFile ~/.ssh/a\r\n  \
                    IdentityFile ~/.ssh/b\r\n  # trailing comment\r\n\r\nHost a b\r\n  User x\r\n";
        let mut c = ConfigFile::parse(text);
        assert_eq!(
            c.entries().iter().map(|e| &e.alias).collect::<Vec<_>>(),
            ["web"]
        );
        let mut e = c.get("WEB").unwrap();
        assert_eq!(e.hostname.as_deref(), Some("old"));
        assert_eq!(e.identity_files, ["~/.ssh/a", "~/.ssh/b"]);
        e.alias = "site".into();
        e.hostname = Some("new host".into());
        e.port = None;
        e.user = Some("me".into());
        e.identity_files = vec!["~/.ssh/b".into(), "~/.ssh/a".into(), "~/.ssh/c".into()];
        assert!(c.update("web", &e));
        assert_eq!(
            c.render(),
            "# mine\r\nHost *.lan\r\n  User lanuser\r\n\r\nhost site\r\n  hostname=\"new host\"\r\n  \
             ForwardAgent yes\r\n  # note\r\n  IdentityFile ~/.ssh/b\r\n  IdentityFile ~/.ssh/a\r\n  \
             IdentityFile ~/.ssh/c\r\n  User me\r\n  # trailing comment\r\n\r\nHost a b\r\n  User x\r\n"
        );
        assert!(c.get("web").is_none());
        assert_eq!(c.get("Site").unwrap(), e);
        // Hosts sharing a Host line can't be edited.
        assert!(!c.update("a", &entry("a", "x")));
        assert!(c.get("a").is_none());
    }

    #[test]
    fn unchanged_values_keep_their_spelling() {
        let text = "Host w\n  HostName \"w.lan\"\n  User bob\n  User ignored\n";
        let mut c = ConfigFile::parse(text);
        let e = c.get("w").unwrap();
        assert_eq!(e.user.as_deref(), Some("bob"));
        assert!(c.update("w", &e));
        assert_eq!(c.render(), text);
        assert!(c.update("w", &Entry { user: None, ..e }));
        assert_eq!(c.render(), "Host w\n  HostName \"w.lan\"\n");
    }

    #[test]
    fn options_are_added_to_empty_blocks() {
        assert_eq!(
            edited("Host w\nHost x\n  Port 1\n", |c| {
                c.update("w", &entry("w", "w.lan"));
            }),
            "Host w\n  HostName w.lan\nHost x\n  Port 1\n"
        );
    }

    #[test]
    fn remove_block() {
        let text = "# h\n\nHost a\n  HostName x\n  # about b\n\nHost b\n  HostName y\n\nHost c\n";
        assert_eq!(
            edited(text, |c| assert!(c.remove("a"))),
            "# h\n\n  # about b\n\nHost b\n  HostName y\n\nHost c\n"
        );
        assert_eq!(
            edited(text, |c| assert!(c.remove("b"))),
            "# h\n\nHost a\n  HostName x\n  # about b\n\nHost c\n"
        );
        assert_eq!(
            edited(text, |c| assert!(c.remove("c"))),
            "# h\n\nHost a\n  HostName x\n  # about b\n\nHost b\n  HostName y\n"
        );
        assert_eq!(edited("Host a\n", |c| assert!(c.remove("a"))), "");
        assert_eq!(
            edited("Host a b\n", |c| assert!(!c.remove("a"))),
            "Host a b\n"
        );
    }

    #[test]
    fn rename_jumps() {
        let mut c = ConfigFile::parse("Host a\n  ProxyJump=u@gw:2,x\nHost b\n  proxyjump x\n");
        assert!(c.rename_jumps("gw", "gate"));
        assert!(!c.rename_jumps("gw", "gate"));
        assert_eq!(
            c.render(),
            "Host a\n  ProxyJump=u@gate:2,x\nHost b\n  proxyjump x\n"
        );
    }

    #[test]
    fn bom_is_kept() {
        let text = "\u{feff}Host a\n";
        assert_eq!(
            edited(text, |c| c.insert(&entry("b", "b"))),
            "\u{feff}Host a\n\nHost b\n    HostName b\n"
        );
    }

    #[test]
    fn legacy_include_and_header_are_dropped() {
        let legacy = Path::new("/home/me/.ssh/patchbay.conf");
        let mut c = ConfigFile::parse(&format!(
            "{INCLUDE_COMMENT}\nInclude patchbay.conf\n\nHost a\nInclude ~/.ssh/other\n"
        ));
        remove_legacy_include(&mut c, legacy);
        assert_eq!(c.render(), "Host a\nInclude ~/.ssh/other\n");
        let mut c = ConfigFile::parse("Include /home/me/.ssh/patchbay.conf\nHost a\n");
        remove_legacy_include(&mut c, legacy);
        assert_eq!(c.render(), "Host a\n");

        let conf = ConfigFile::parse(&format!(
            "{}\n\nHost a\n    HostName x\n\n",
            LEGACY_HEADER.join("\n")
        ));
        assert_eq!(conf.without_legacy_header(), ["Host a", "    HostName x"]);
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
        let mut c = ConfigFile::load(&cfg).unwrap();
        c.insert(&entry("b", "b.lan"));
        c.save(&cfg).unwrap();
        assert!(fs::symlink_metadata(&cfg).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(&real).unwrap().contains("Host b"));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&real), 0o640);
        assert_eq!(write_problem(&cfg), None);

        let fresh = dir.path().join("new/.ssh/config");
        assert_eq!(write_problem(&fresh), None);
        c.save(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o600);
        assert_eq!(mode(fresh.parent().unwrap()), 0o700);

        fs::set_permissions(&real, fs::Permissions::from_mode(0o440)).unwrap();
        // Root can write anyway.
        if fs::OpenOptions::new().append(true).open(&real).is_err() {
            assert!(write_problem(&cfg).is_some());
        }
    }
}
