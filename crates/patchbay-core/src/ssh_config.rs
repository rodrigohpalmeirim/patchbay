//! Parser and resolver for the OpenSSH client configuration (`ssh_config(5)`).
//!
//! [`SshConfig`] reads a config file (following `Include`s) into a flat list of
//! directives, each tagged with the stack of `Host` / `Match` conditions it is
//! nested under. [`SshConfig::resolve`] then evaluates those conditions for one
//! host alias in a single pass, applying OpenSSH's "first obtained value wins"
//! rule, and returns a typed [`ResolvedHost`].
//!
//! Parsing never fails because of content: unknown keywords, unsupported
//! `Match` criteria, unreadable includes and malformed values are reported as
//! warnings instead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use sha1::{Digest, Sha1};

use crate::model::AgentSource;
use crate::{Error, Result};

/// OpenSSH's `READCONF_MAX_DEPTH`.
const MAX_INCLUDE_DEPTH: usize = 16;

/// Keywords whose values accumulate instead of "first wins".
const ACCUMULATING: &[&str] = &[
    "identityfile",
    "certificatefile",
    "localforward",
    "remoteforward",
    "dynamicforward",
    "sendenv",
];

/// Valid keywords that Patchbay does not implement but that change how a
/// connection is made, so silently ignoring them could surprise the user.
const UNSUPPORTED_EFFECTFUL: &[&str] = &["knownhostscommand", "pkcs11provider", "remotecommand"];

/// Every keyword OpenSSH (and common vendor builds) accept. Anything else is
/// reported as unknown unless covered by `IgnoreUnknown`.
const KNOWN_KEYWORDS: &[&str] = &[
    "addkeystoagent",
    "addressfamily",
    "batchmode",
    "bindaddress",
    "bindinterface",
    "canonicaldomains",
    "canonicalizefallbacklocal",
    "canonicalizehostname",
    "canonicalizemaxdots",
    "canonicalizepermittedcnames",
    "casignaturealgorithms",
    "certificatefile",
    "challengeresponseauthentication",
    "channeltimeout",
    "checkhostip",
    "cipher",
    "ciphers",
    "clearallforwardings",
    "compression",
    "compressionlevel",
    "connectionattempts",
    "connecttimeout",
    "controlmaster",
    "controlpath",
    "controlpersist",
    "dynamicforward",
    "enableescapecommandline",
    "enablesshkeysign",
    "escapechar",
    "exitonforwardfailure",
    "fingerprinthash",
    "forkafterauthentication",
    "forwardagent",
    "forwardx11",
    "forwardx11timeout",
    "forwardx11trusted",
    "gatewayports",
    "globalknownhostsfile",
    "gssapiauthentication",
    "gssapiclientidentity",
    "gssapidelegatecredentials",
    "gssapikeyexchange",
    "gssapikexalgorithms",
    "gssapirenewalforcesrekey",
    "gssapiserveridentity",
    "gssapitrustdns",
    "hashknownhosts",
    "host",
    "hostbasedacceptedalgorithms",
    "hostbasedauthentication",
    "hostbasedkeytypes",
    "hostkeyalgorithms",
    "hostkeyalias",
    "hostname",
    "identitiesonly",
    "identityagent",
    "identityfile",
    "ignoreunknown",
    "include",
    "ipqos",
    "kbdinteractiveauthentication",
    "kbdinteractivedevices",
    "kerberosauthentication",
    "kexalgorithms",
    "knownhostscommand",
    "localcommand",
    "localforward",
    "loglevel",
    "logverbose",
    "macs",
    "match",
    "nohostauthenticationforlocalhost",
    "numberofpasswordprompts",
    "obscurekeystroketiming",
    "passwordauthentication",
    "permitlocalcommand",
    "permitremoteopen",
    "pkcs11provider",
    "port",
    "preferredauthentications",
    "protocol",
    "proxycommand",
    "proxyjump",
    "proxyusefdpass",
    "pubkeyacceptedalgorithms",
    "pubkeyacceptedkeytypes",
    "pubkeyauthentication",
    "refuseconnection",
    "rekeylimit",
    "remotecommand",
    "remoteforward",
    "requesttty",
    "requiredrsasize",
    "revokedhostkeys",
    "rhostsrsaauthentication",
    "rsaauthentication",
    "securitykeyprovider",
    "sendenv",
    "serveralivecountmax",
    "serveraliveinterval",
    "sessiontype",
    "setenv",
    "smartcarddevice",
    "stdinnull",
    "streamlocalbindmask",
    "streamlocalbindunlink",
    "stricthostkeychecking",
    "syslogfacility",
    "tag",
    "tcpkeepalive",
    "tunnel",
    "tunneldevice",
    "updatehostkeys",
    "usekeychain",
    "useprivilegedport",
    "user",
    "userknownhostsfile",
    "useroaming",
    "verifyhostkeydns",
    "versionaddendum",
    "visualhostkey",
    "warnweakcrypto",
    "xauthlocation",
];

/// Facts about the local machine used for `~` / `%` expansion and `Include`
/// lookup. Injectable so tests never touch the real home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEnv {
    pub home: PathBuf,
    /// Local login name (`%u`, `Match localuser`, default remote user).
    pub user: String,
    /// Local host name including domain, if known (`%l`; `%L` is its first label).
    pub hostname: String,
    /// Numeric user id (`%i`); `None` where the concept doesn't exist.
    pub uid: Option<u32>,
}

impl LocalEnv {
    /// Detect from the running process. User falls back to `root` when
    /// neither `USER` nor `USERNAME` is set.
    pub fn detect() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let user = ["USER", "USERNAME"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty())
            .unwrap_or_else(|| "root".to_owned());
        let uid = detect_uid(&home);
        Self {
            home,
            user,
            hostname: detect_hostname(),
            uid,
        }
    }

    /// `~/.ssh`.
    pub fn ssh_dir(&self) -> PathBuf {
        self.home.join(".ssh")
    }
}

fn detect_hostname() -> String {
    let from_env = ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.trim().is_empty());
    let from_file = || {
        ["/proc/sys/kernel/hostname", "/etc/hostname"]
            .iter()
            .filter_map(|p| fs::read_to_string(p).ok())
            .map(|s| s.trim().to_owned())
            .find(|s| !s.is_empty())
    };
    from_env
        .or_else(from_file)
        .unwrap_or_else(|| "localhost".to_owned())
}

#[cfg(unix)]
fn detect_uid(home: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/proc/self")
        .or_else(|_| fs::metadata(home))
        .ok()
        .map(|m| m.uid())
}

#[cfg(not(unix))]
fn detect_uid(_home: &Path) -> Option<u32> {
    None
}

/// Which kind of port forward a [`ForwardSpec`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardKind {
    /// `LocalForward` (`-L`).
    Local,
    /// `RemoteForward` (`-R`); without a destination it is a remote SOCKS proxy.
    Remote,
    /// `DynamicForward` (`-D`).
    Dynamic,
}

/// One side of a forward: a TCP address or a Unix-domain socket path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardEndpoint {
    /// `host` is the bind address for listeners (`None` = default, `"*"` or
    /// `""` = all interfaces) and the target host for destinations.
    Tcp {
        host: Option<String>,
        port: u16,
    },
    Unix(String),
}

/// A forward as written in the config, syntactically parsed but not validated
/// further (ports may be 0, bind addresses are passed through).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardSpec {
    pub kind: ForwardKind,
    pub listen: ForwardEndpoint,
    /// `None` for dynamic forwards (local or remote SOCKS).
    pub connect: Option<ForwardEndpoint>,
}

/// Values that take precedence over the config file, like `ssh -l` / `-p`.
/// They also feed `Match user` and the `%r` / `%p` tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveOverrides {
    pub user: Option<String>,
    pub port: Option<u16>,
}

/// The effective configuration for one host alias. `Option`s are `None` when
/// the config does not set the option (callers apply their own defaults).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedHost {
    /// The name that was resolved (as given).
    pub alias: String,
    /// `HostName` with `%h` expanded, or the alias if unset.
    pub hostname: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// Expanded, deduplicated, in config order.
    pub identity_files: Vec<PathBuf>,
    pub identities_only: Option<bool>,
    pub identity_agent: Option<AgentSource>,
    pub certificate_files: Vec<PathBuf>,
    /// Raw ProxyJump spec; `None` if unset or `none`.
    pub proxy_jump: Option<String>,
    /// Recorded for display only; Patchbay cannot run proxy commands.
    pub proxy_command: Option<String>,
    pub host_key_alias: Option<String>,
    /// `Some(vec![])` means `UserKnownHostsFile none`.
    pub user_known_hosts_files: Option<Vec<PathBuf>>,
    pub connect_timeout: Option<Duration>,
    pub server_alive_interval: Option<Duration>,
    /// Local, remote and dynamic forwards in config order.
    pub forwards: Vec<ForwardSpec>,
    /// Raw arguments of every other keyword that applied (lowercase keys).
    /// Single-valued keywords hold their first value; `SendEnv` accumulates.
    pub options: BTreeMap<String, Vec<String>>,
    /// Problems specific to this host (unsupported options, bad values).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Pattern {
    negated: bool,
    glob: String,
}

impl Pattern {
    fn parse(s: &str) -> Self {
        match s.strip_prefix('!') {
            Some(rest) => Self {
                negated: true,
                glob: rest.to_owned(),
            },
            None => Self {
                negated: false,
                glob: s.to_owned(),
            },
        }
    }
}

#[derive(Debug, Clone)]
enum CriterionKind {
    All,
    /// `canonical` / `final`: every resolution here is the single final pass.
    Pass,
    Host(String),
    OriginalHost(String),
    User(String),
    LocalUser(String),
    Unsupported,
}

#[derive(Debug, Clone)]
struct Criterion {
    negated: bool,
    kind: CriterionKind,
}

#[derive(Debug, Clone)]
enum Condition {
    Host(Vec<Pattern>),
    Match(Vec<Criterion>),
}

#[derive(Debug, Clone)]
struct Directive {
    conds: Vec<usize>,
    keyword: String,
    args: Vec<String>,
    file: PathBuf,
    line: usize,
}

impl Directive {
    fn origin(&self) -> String {
        format!("{}:{}", self.file.display(), self.line)
    }
}

#[derive(Debug, Clone)]
enum Item {
    /// A `Host` / `Match` line: evaluate the condition at this point.
    Cond(usize),
    Directive(Directive),
}

/// A parsed OpenSSH client config, ready to resolve hosts against.
#[derive(Debug, Clone)]
pub struct SshConfig {
    env: LocalEnv,
    items: Vec<Item>,
    conditions: Vec<Condition>,
    host_aliases: Vec<String>,
    warnings: Vec<String>,
}

impl SshConfig {
    /// A config with no directives.
    pub fn empty(env: LocalEnv) -> Self {
        Self {
            env,
            items: Vec::new(),
            conditions: Vec::new(),
            host_aliases: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// `~/.ssh/config` of the current user.
    pub fn load_default() -> Result<Self> {
        let env = LocalEnv::detect();
        let path = env.ssh_dir().join("config");
        Self::load_with_env(&path, env)
    }

    /// Load `path` using the detected [`LocalEnv`]. A missing file yields an
    /// empty config.
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_with_env(path, LocalEnv::detect())
    }

    /// Load `path` with an explicit environment. A missing file yields an
    /// empty config; other read errors are returned.
    pub fn load_with_env(path: &Path, env: LocalEnv) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text, path, env)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::empty(env)),
            Err(source) => Err(Error::ReadFile {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Parse config text. `path` is used for messages only; `Include`s are
    /// resolved relative to `~/.ssh` as OpenSSH does for user configs.
    pub fn parse(text: &str, path: &Path, env: LocalEnv) -> Self {
        let mut parser = Parser {
            env: &env,
            items: Vec::new(),
            conditions: Vec::new(),
            host_aliases: Vec::new(),
            seen_aliases: HashSet::new(),
            warnings: Vec::new(),
            ignore_unknown: Vec::new(),
        };
        parser.parse_text(text, path, &[], 0);
        let Parser {
            items,
            conditions,
            host_aliases,
            warnings,
            ..
        } = parser;
        Self {
            env,
            items,
            conditions,
            host_aliases,
            warnings,
        }
    }

    pub fn env(&self) -> &LocalEnv {
        &self.env
    }

    /// Concrete (wildcard-free, non-negated) names from `Host` lines, in file
    /// order, deduplicated case-insensitively.
    pub fn host_aliases(&self) -> &[String] {
        &self.host_aliases
    }

    /// Whether `name` is one of [`host_aliases`](Self::host_aliases)
    /// (case-insensitive).
    pub fn has_alias(&self, name: &str) -> bool {
        self.host_aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case(name))
    }

    /// Problems found while reading the files (unknown keywords, `Match exec`,
    /// unreadable includes, syntax errors on individual lines).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Resolve `alias` as `ssh <alias>` would, run by `local_user`.
    pub fn resolve(&self, alias: &str, local_user: &str) -> ResolvedHost {
        self.resolve_with(alias, local_user, &ResolveOverrides::default())
    }

    /// Resolve with command-line style overrides (`ssh -l user -p port alias`).
    pub fn resolve_with(
        &self,
        alias: &str,
        local_user: &str,
        overrides: &ResolveOverrides,
    ) -> ResolvedHost {
        let original = alias.to_lowercase();
        let mut results: Vec<bool> = vec![false; self.conditions.len()];
        let mut first: HashMap<&str, &Directive> = HashMap::new();
        let mut accumulated: Vec<&Directive> = Vec::new();

        for item in &self.items {
            match item {
                Item::Cond(id) => {
                    let ctx = MatchCtx {
                        original: &original,
                        hostname: first.get("hostname").and_then(|d| d.args.first()),
                        user: overrides
                            .user
                            .as_deref()
                            .or_else(|| {
                                first
                                    .get("user")
                                    .and_then(|d| d.args.first())
                                    .map(String::as_str)
                            })
                            .unwrap_or(local_user),
                        local_user,
                    };
                    results[*id] = ctx.eval(&self.conditions[*id]);
                }
                Item::Directive(d) => {
                    if !d.conds.iter().all(|c| results[*c]) {
                        continue;
                    }
                    let k = d.keyword.as_str();
                    if ACCUMULATING.contains(&k) {
                        accumulated.push(d);
                    } else if k == "proxyjump" || k == "proxycommand" {
                        if !first.contains_key("proxyjump") && !first.contains_key("proxycommand") {
                            first.insert(k, d);
                        }
                    } else {
                        first.entry(k).or_insert(d);
                    }
                }
            }
        }

        Builder {
            env: &self.env,
            alias,
            local_user,
            overrides,
            first,
            accumulated,
            out: ResolvedHost {
                alias: alias.to_owned(),
                ..ResolvedHost::default()
            },
        }
        .build()
    }
}

struct Parser<'e> {
    env: &'e LocalEnv,
    items: Vec<Item>,
    conditions: Vec<Condition>,
    host_aliases: Vec<String>,
    seen_aliases: HashSet<String>,
    warnings: Vec<String>,
    ignore_unknown: Vec<String>,
}

impl Parser<'_> {
    fn warn(&mut self, file: &Path, line: usize, msg: impl std::fmt::Display) {
        self.warnings
            .push(format!("{}:{}: {}", file.display(), line, msg));
    }

    fn parse_text(&mut self, text: &str, file: &Path, outer: &[usize], depth: usize) {
        let mut own: Option<usize> = None;
        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let Some((keyword, rest)) = split_keyword(raw) else {
                continue;
            };
            let keyword = keyword.to_ascii_lowercase();
            if keyword.is_empty() {
                self.warn(file, line_no, "missing keyword");
                continue;
            }
            let args = match split_args(rest) {
                Ok(a) => a,
                Err(e) => {
                    self.warn(file, line_no, e);
                    continue;
                }
            };
            let conds = || {
                let mut c = outer.to_vec();
                c.extend(own);
                c
            };
            match keyword.as_str() {
                "host" => {
                    if args.is_empty() {
                        self.warn(file, line_no, "Host without patterns never matches");
                    }
                    for a in &args {
                        if !a.starts_with('!')
                            && !a.contains(['*', '?'])
                            && !a.is_empty()
                            && self.seen_aliases.insert(a.to_lowercase())
                        {
                            self.host_aliases.push(a.clone());
                        }
                    }
                    let patterns = args.iter().map(|a| Pattern::parse(a)).collect();
                    own = Some(self.push_condition(Condition::Host(patterns)));
                }
                "match" => {
                    let criteria = self.parse_match(&args, file, line_no);
                    own = Some(self.push_condition(Condition::Match(criteria)));
                }
                "include" => {
                    if args.is_empty() {
                        self.warn(file, line_no, "Include without a path");
                    }
                    let conds = conds();
                    for a in &args {
                        self.include(a, file, line_no, &conds, depth);
                    }
                }
                _ => {
                    if args.is_empty() {
                        self.warn(file, line_no, format!("missing argument for {keyword}"));
                        continue;
                    }
                    if keyword == "ignoreunknown" {
                        self.ignore_unknown
                            .extend(args.iter().map(|a| a.to_lowercase()));
                    } else if !KNOWN_KEYWORDS.contains(&keyword.as_str())
                        && !self
                            .ignore_unknown
                            .iter()
                            .any(|p| match_pattern_list(&keyword, p))
                    {
                        self.warn(
                            file,
                            line_no,
                            format!("unknown keyword {keyword:?} ignored"),
                        );
                    }
                    self.items.push(Item::Directive(Directive {
                        conds: conds(),
                        keyword,
                        args,
                        file: file.to_owned(),
                        line: line_no,
                    }));
                }
            }
        }
    }

    fn push_condition(&mut self, cond: Condition) -> usize {
        let id = self.conditions.len();
        self.conditions.push(cond);
        self.items.push(Item::Cond(id));
        id
    }

    fn parse_match(&mut self, args: &[String], file: &Path, line: usize) -> Vec<Criterion> {
        let mut out = Vec::new();
        if args.is_empty() {
            self.warn(file, line, "Match without criteria never matches");
            out.push(Criterion {
                negated: false,
                kind: CriterionKind::Unsupported,
            });
        }
        let mut it = args.iter();
        while let Some(tok) = it.next() {
            let (negated, name) = match tok.strip_prefix('!') {
                Some(n) => (true, n),
                None => (false, tok.as_str()),
            };
            let name = name.to_ascii_lowercase();
            let kind = match name.as_str() {
                "all" => CriterionKind::All,
                "canonical" | "final" => CriterionKind::Pass,
                "host" | "originalhost" | "user" | "localuser" => match it.next() {
                    Some(list) => {
                        let list = list.clone();
                        match name.as_str() {
                            "host" => CriterionKind::Host(list.to_lowercase()),
                            "originalhost" => CriterionKind::OriginalHost(list.to_lowercase()),
                            "user" => CriterionKind::User(list),
                            _ => CriterionKind::LocalUser(list),
                        }
                    }
                    None => {
                        self.warn(
                            file,
                            line,
                            format!("Match {name} needs an argument; block never matches"),
                        );
                        CriterionKind::Unsupported
                    }
                },
                "exec" | "localnetwork" | "tagged" | "version" | "sessiontype" | "command" => {
                    it.next();
                    self.warn(
                        file,
                        line,
                        format!("Match {name} is not supported; block never matches"),
                    );
                    CriterionKind::Unsupported
                }
                _ => {
                    self.warn(
                        file,
                        line,
                        format!("unknown Match criterion {name:?}; block never matches"),
                    );
                    CriterionKind::Unsupported
                }
            };
            out.push(Criterion { negated, kind });
        }
        out
    }

    fn include(&mut self, arg: &str, file: &Path, line: usize, conds: &[usize], depth: usize) {
        if depth + 1 > MAX_INCLUDE_DEPTH {
            self.warn(
                file,
                line,
                format!("Include {arg:?} exceeds maximum nesting depth"),
            );
            return;
        }
        let expanded = expand_tilde(arg, &self.env.home);
        let path = PathBuf::from(&expanded);
        let path = if path.has_root() {
            path
        } else {
            self.env.ssh_dir().join(path)
        };
        for target in expand_glob(&path) {
            match fs::read_to_string(&target) {
                Ok(text) => self.parse_text(&text, &target, conds, depth + 1),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => self.warn(
                    file,
                    line,
                    format!("cannot read included file {}: {e}", target.display()),
                ),
            }
        }
    }
}

struct MatchCtx<'a> {
    original: &'a str,
    hostname: Option<&'a String>,
    user: &'a str,
    local_user: &'a str,
}

impl MatchCtx<'_> {
    fn eval(&self, cond: &Condition) -> bool {
        match cond {
            Condition::Host(patterns) => {
                let mut matched = false;
                for p in patterns {
                    if wildcard_match(&p.glob.to_lowercase(), self.original) {
                        if p.negated {
                            return false;
                        }
                        matched = true;
                    }
                }
                matched
            }
            Condition::Match(criteria) => criteria.iter().all(|c| {
                let hit = match &c.kind {
                    CriterionKind::Unsupported => return false,
                    CriterionKind::All | CriterionKind::Pass => true,
                    CriterionKind::Host(list) => {
                        let host = match self.hostname {
                            Some(h) => expand_percent(h, |t| match t {
                                'h' => Some(self.original.to_owned()),
                                _ => None,
                            })
                            .0
                            .to_lowercase(),
                            None => self.original.to_owned(),
                        };
                        match_pattern_list(&host, list)
                    }
                    CriterionKind::OriginalHost(list) => match_pattern_list(self.original, list),
                    CriterionKind::User(list) => match_pattern_list(self.user, list),
                    CriterionKind::LocalUser(list) => match_pattern_list(self.local_user, list),
                };
                hit != c.negated
            }),
        }
    }
}

struct Builder<'a> {
    env: &'a LocalEnv,
    alias: &'a str,
    local_user: &'a str,
    overrides: &'a ResolveOverrides,
    first: HashMap<&'a str, &'a Directive>,
    accumulated: Vec<&'a Directive>,
    out: ResolvedHost,
}

impl<'a> Builder<'a> {
    fn warn(&mut self, d: &Directive, msg: impl std::fmt::Display) {
        self.out
            .warnings
            .push(format!("host {:?}: {msg} ({})", self.alias, d.origin()));
    }

    fn take(&mut self, key: &str) -> Option<&'a Directive> {
        self.first.remove(key)
    }

    fn take_parsed<T>(&mut self, key: &str, parse: impl Fn(&str) -> Option<T>) -> Option<T> {
        let d = self.take(key)?;
        let v = parse(&d.args[0]);
        if v.is_none() {
            self.warn(d, format!("invalid {key} value {:?}", d.args[0]));
        }
        v
    }

    fn build(mut self) -> ResolvedHost {
        let alias_lc = self.alias.to_lowercase();
        self.out.hostname = match self.take("hostname") {
            Some(d) => {
                let raw = d.args[0].clone();
                self.expand(d, &raw, |t| match t {
                    'h' => Some(alias_lc.clone()),
                    _ => None,
                })
            }
            None => self.alias.to_owned(),
        };

        let port = self.take_parsed("port", |s| s.parse::<u16>().ok().filter(|p| *p != 0));
        self.out.port = self.overrides.port.or(port);
        let user = self.take("user").map(|d| d.args[0].clone());
        self.out.user = self.overrides.user.clone().or(user);
        self.out.identities_only = self.take_parsed("identitiesonly", parse_bool);
        self.out.connect_timeout = self.take_parsed("connecttimeout", parse_timeout).flatten();
        self.out.server_alive_interval = self
            .take_parsed("serveraliveinterval", parse_timeout)
            .flatten();
        self.out.host_key_alias = self.take("hostkeyalias").map(|d| d.args[0].clone());

        if let Some(d) = self.take("proxyjump")
            && !d.args[0].eq_ignore_ascii_case("none")
        {
            self.out.proxy_jump = Some(d.args.join(","));
        }
        if let Some(d) = self.take("proxycommand")
            && !d.args[0].eq_ignore_ascii_case("none")
        {
            self.out.proxy_command = Some(d.args.join(" "));
            self.warn(d, "ProxyCommand is not supported and will be ignored");
        }
        if let Some(d) = self.take("canonicalizehostname") {
            if !d.args[0].eq_ignore_ascii_case("no") {
                self.warn(
                    d,
                    "CanonicalizeHostname is not supported and will be ignored",
                );
            }
            self.out.options.insert(d.keyword.clone(), d.args.clone());
        }

        let tokens = self.token_values();
        let path_tokens = |t: char| tokens.get(&t).cloned();

        if let Some(d) = self.take("identityagent") {
            let raw = d.args[0].clone();
            self.out.identity_agent = Some(if raw.eq_ignore_ascii_case("none") {
                AgentSource::Disabled
            } else if raw == "SSH_AUTH_SOCK" || raw == "$SSH_AUTH_SOCK" {
                AgentSource::Default
            } else if let Some(var) = raw.strip_prefix('$') {
                match std::env::var(var) {
                    Ok(v) if !v.is_empty() => AgentSource::Path(PathBuf::from(v)),
                    _ => {
                        self.warn(d, format!("IdentityAgent variable ${var} is not set"));
                        AgentSource::Default
                    }
                }
            } else {
                AgentSource::Path(PathBuf::from(self.expand_path(d, &raw, &path_tokens)))
            });
        }

        if let Some(d) = self.take("userknownhostsfile") {
            let files = if d.args[0].eq_ignore_ascii_case("none") {
                Vec::new()
            } else {
                d.args
                    .iter()
                    .map(|a| PathBuf::from(self.expand_path(d, a, &path_tokens)))
                    .collect()
            };
            self.out.user_known_hosts_files = Some(files);
        }

        for d in std::mem::take(&mut self.accumulated) {
            match d.keyword.as_str() {
                "identityfile" | "certificatefile" => {
                    let p = PathBuf::from(self.expand_path(d, &d.args[0], &path_tokens));
                    let list = if d.keyword == "identityfile" {
                        &mut self.out.identity_files
                    } else {
                        &mut self.out.certificate_files
                    };
                    if !list.contains(&p) {
                        list.push(p);
                    }
                }
                "localforward" | "remoteforward" | "dynamicforward" => {
                    let kind = match d.keyword.as_str() {
                        "localforward" => ForwardKind::Local,
                        "remoteforward" => ForwardKind::Remote,
                        _ => ForwardKind::Dynamic,
                    };
                    match parse_forward(kind, &d.args) {
                        Ok(f) => self.out.forwards.push(f),
                        Err(e) => self.warn(d, format!("invalid {}: {e}", d.keyword)),
                    }
                }
                _ => self
                    .out
                    .options
                    .entry(d.keyword.clone())
                    .or_default()
                    .extend(d.args.iter().cloned()),
            }
        }

        let rest: Vec<_> = self.first.drain().collect();
        for (k, d) in rest {
            if UNSUPPORTED_EFFECTFUL.contains(&k) {
                let name = d.keyword.clone();
                self.warn(d, format!("{name} is not supported and will be ignored"));
            }
            self.out.options.insert(k.to_owned(), d.args.clone());
        }
        self.out
    }

    /// Values for `%` tokens in path-like options, from the final settings.
    fn token_values(&self) -> HashMap<char, String> {
        let port = self.out.port.unwrap_or(22).to_string();
        let ruser = self
            .out
            .user
            .clone()
            .unwrap_or_else(|| self.local_user.to_owned());
        let jump = self.out.proxy_jump.clone().unwrap_or_default();
        let local_host = self.env.hostname.clone();
        let short_host = local_host.split('.').next().unwrap_or_default().to_owned();
        let conn_hash = Sha1::digest(
            format!("{local_host}{}{port}{ruser}{jump}", self.out.hostname).as_bytes(),
        )
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
        let mut t = HashMap::from([
            ('h', self.out.hostname.clone()),
            ('n', self.alias.to_lowercase()),
            ('p', port),
            ('r', ruser),
            ('u', self.local_user.to_owned()),
            ('d', self.env.home.to_string_lossy().into_owned()),
            ('l', local_host),
            ('L', short_host),
            ('j', jump),
            ('C', conn_hash),
            (
                'k',
                self.out
                    .host_key_alias
                    .clone()
                    .unwrap_or_else(|| self.alias.to_lowercase()),
            ),
        ]);
        if let Some(uid) = self.env.uid {
            t.insert('i', uid.to_string());
        }
        t
    }

    fn expand(
        &mut self,
        d: &Directive,
        s: &str,
        lookup: impl Fn(char) -> Option<String>,
    ) -> String {
        let (out, unknown) = expand_percent(s, lookup);
        for t in unknown {
            self.warn(
                d,
                format!("unknown or unavailable token %{t} in {}", d.keyword),
            );
        }
        out
    }

    /// `~`, `${VAR}` and `%` expansion for file names, in OpenSSH's order.
    fn expand_path(
        &mut self,
        d: &Directive,
        s: &str,
        lookup: &impl Fn(char) -> Option<String>,
    ) -> String {
        let s = expand_tilde(s, &self.env.home);
        let (s, missing) = expand_env(&s);
        for var in missing {
            self.warn(d, format!("environment variable {var} is not set"));
        }
        self.expand(d, &s, lookup)
    }
}

/// Split a config line into keyword and the unparsed rest, handling
/// `Key value`, `Key=value` and `Key = value`. `None` for blank/comment lines.
fn split_keyword(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let mut rest = line[end..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    Some((&line[..end], rest))
}

/// OpenSSH `argv_split`: whitespace-separated words, `'`/`"` quoting (also
/// mid-word), backslash escaping only of quotes, backslash and (unquoted)
/// space, and `#` at the start of a word ending the line.
fn split_args(s: &str) -> std::result::Result<Vec<String>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] == '#' {
            break;
        }
        let mut arg = String::new();
        let mut quote: Option<char> = None;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                if let Some(&n) = chars.get(i + 1)
                    && (matches!(n, '\'' | '"' | '\\') || (quote.is_none() && n.is_whitespace()))
                {
                    arg.push(n);
                    i += 2;
                    continue;
                }
                arg.push(c);
            } else if let Some(q) = quote {
                if c == q {
                    quote = None;
                } else {
                    arg.push(c);
                }
            } else if c.is_whitespace() {
                break;
            } else if c == '"' || c == '\'' {
                quote = Some(c);
            } else {
                arg.push(c);
            }
            i += 1;
        }
        if quote.is_some() {
            return Err("unterminated quote; line ignored".to_owned());
        }
        args.push(arg);
    }
    Ok(args)
}

/// Glob match supporting `*` and `?` (case-sensitive; callers fold case).
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// OpenSSH `match_pattern_list`: comma-separated globs; any matching negated
/// entry rejects, otherwise at least one positive entry must match.
pub fn match_pattern_list(s: &str, list: &str) -> bool {
    let mut positive = false;
    for p in list.split(',') {
        let (neg, p) = match p.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, p),
        };
        if wildcard_match(p, s) {
            if neg {
                return false;
            }
            positive = true;
        }
    }
    positive
}

/// Expand a leading `~` / `~/` (or `~\`) to `home`. `~user` is left alone.
/// On Windows the rest gets `\` too, so `~/.ssh/id` doesn't become
/// `C:\Users\me\.ssh/id` (shown in the UI and used as a keychain account).
pub fn expand_tilde(s: &str, home: &Path) -> String {
    if s == "~" {
        return home.to_string_lossy().into_owned();
    }
    match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        Some(rest) if cfg!(windows) => home
            .join(rest.replace('/', "\\"))
            .to_string_lossy()
            .into_owned(),
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => s.to_owned(),
    }
}

/// Expand `${VAR}`; returns the result and names of unset variables (left
/// unexpanded).
fn expand_env(s: &str) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut missing = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        missing.push(name.to_owned());
                        out.push_str(&rest[start..start + 3 + end]);
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    (out, missing)
}

/// Expand `%x` tokens via `lookup` (`%%` is always `%`). Unknown tokens are
/// kept literally and returned.
fn expand_percent(s: &str, lookup: impl Fn(char) -> Option<String>) -> (String, Vec<char>) {
    let mut out = String::with_capacity(s.len());
    let mut unknown = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(t) => match lookup(t) {
                Some(v) => out.push_str(&v),
                None => {
                    unknown.push(t);
                    out.push('%');
                    out.push(t);
                }
            },
            None => out.push('%'),
        }
    }
    (out, unknown)
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// OpenSSH `convtime` (`90`, `1m30s`, `2h`), with `none`/`0` meaning unset.
fn parse_timeout(s: &str) -> Option<Option<Duration>> {
    if s.eq_ignore_ascii_case("none") {
        return Some(None);
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let mult = match c.to_ascii_lowercase() {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 604_800,
            _ => return None,
        };
        let n: u64 = num.parse().ok()?;
        total = total.checked_add(n.checked_mul(mult)?)?;
        num.clear();
    }
    if !num.is_empty() {
        total = total.checked_add(num.parse().ok()?)?;
    } else if s.is_empty() || s.ends_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some((total > 0).then(|| Duration::from_secs(total)))
}

/// Split `a:b:c` into fields, treating `[...]` as one literal field (IPv6).
fn split_fields(s: &str) -> std::result::Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut rest = s;
    loop {
        if let Some(inner) = rest.strip_prefix('[') {
            let end = inner.find(']').ok_or("unterminated '['")?;
            fields.push(inner[..end].to_owned());
            rest = &inner[end + 1..];
            if rest.is_empty() {
                break;
            }
            rest = rest.strip_prefix(':').ok_or("expected ':' after ']'")?;
        } else {
            match rest.find(':') {
                Some(i) => {
                    fields.push(rest[..i].to_owned());
                    rest = &rest[i + 1..];
                }
                None => {
                    fields.push(rest.to_owned());
                    break;
                }
            }
        }
    }
    Ok(fields)
}

fn parse_port(s: &str) -> std::result::Result<u16, String> {
    s.parse().map_err(|_| format!("bad port {s:?}"))
}

fn parse_endpoint(s: &str, listener: bool) -> std::result::Result<ForwardEndpoint, String> {
    if s.contains('/') && !s.starts_with('[') {
        return Ok(ForwardEndpoint::Unix(s.to_owned()));
    }
    let fields = split_fields(s)?;
    match (fields.as_slice(), listener) {
        ([port], true) => Ok(ForwardEndpoint::Tcp {
            host: None,
            port: parse_port(port)?,
        }),
        ([host, port], _) => Ok(ForwardEndpoint::Tcp {
            host: Some(host.clone()),
            port: parse_port(port)?,
        }),
        _ => Err(format!("cannot parse {s:?}")),
    }
}

fn parse_forward(kind: ForwardKind, args: &[String]) -> std::result::Result<ForwardSpec, String> {
    let (listen, connect) = match (kind, args) {
        (ForwardKind::Dynamic, [l]) | (ForwardKind::Remote, [l]) => (l, None),
        (ForwardKind::Local | ForwardKind::Remote, [l, c]) => (l, Some(c)),
        _ => return Err("wrong number of arguments".to_owned()),
    };
    Ok(ForwardSpec {
        kind,
        listen: parse_endpoint(listen, true)?,
        connect: connect.map(|c| parse_endpoint(c, false)).transpose()?,
    })
}

/// Expand a path containing `*` / `?` wildcards into existing paths (sorted
/// per directory, hidden entries only matched by patterns starting with `.`).
/// Paths without wildcards are returned as-is.
fn expand_glob(path: &Path) -> Vec<PathBuf> {
    let has_wild = |s: &str| s.contains(['*', '?']);
    if !has_wild(&path.to_string_lossy()) {
        return vec![path.to_owned()];
    }
    let mut candidates = vec![PathBuf::new()];
    for comp in path.components() {
        let name = comp.as_os_str().to_string_lossy();
        match comp {
            Component::Normal(_) if has_wild(&name) => {
                let mut next = Vec::new();
                for dir in &candidates {
                    let read_from = if dir.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        dir.as_path()
                    };
                    let Ok(entries) = fs::read_dir(read_from) else {
                        continue;
                    };
                    let mut names: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| {
                            (!n.starts_with('.') || name.starts_with('.'))
                                && wildcard_match(&name, n)
                        })
                        .collect();
                    names.sort();
                    next.extend(names.into_iter().map(|n| dir.join(n)));
                }
                candidates = next;
            }
            _ => {
                for c in &mut candidates {
                    c.push(comp.as_os_str());
                }
            }
        }
    }
    candidates.retain(|p| p.exists());
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn env(home: &Path) -> LocalEnv {
        LocalEnv {
            home: home.to_owned(),
            user: "me".into(),
            hostname: "box.example.org".into(),
            uid: Some(1000),
        }
    }

    fn parse(text: &str) -> SshConfig {
        SshConfig::parse(text, Path::new("/cfg"), env(Path::new("/home/me")))
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*.example.com", "a.example.com"));
        assert!(!wildcard_match("*.example.com", "example.com"));
        assert!(wildcard_match("web-??", "web-01"));
        assert!(!wildcard_match("web-??", "web-1"));
        assert!(wildcard_match("a*b*c", "aXXbYYbc"));
        assert!(!wildcard_match("a*b*c", "aXXbYYb"));
        assert!(match_pattern_list("foo", "bar,f*"));
        assert!(!match_pattern_list("foo", "f*,!foo"));
        assert!(!match_pattern_list("foo", "!bar"));
    }

    #[test]
    fn tokenizer() {
        assert_eq!(split_keyword("  Port=22"), Some(("Port", "22")));
        assert_eq!(split_keyword("Port = 22"), Some(("Port", "22")));
        assert_eq!(split_keyword("Port  22  "), Some(("Port", "22  ")));
        assert_eq!(split_keyword("   # comment"), None);
        assert_eq!(split_keyword("   "), None);
        assert_eq!(
            split_args(r#"a "b c" 'd e' f"g h"i # trailing"#).unwrap(),
            vec!["a", "b c", "d e", "fg hi"]
        );
        assert_eq!(
            split_args(r#"a\ b "x\"y" C:\Users\x"#).unwrap(),
            vec!["a b", "x\"y", r"C:\Users\x"]
        );
        assert_eq!(split_args(r#""""#).unwrap(), vec![""]);
        assert_eq!(split_args("x#y").unwrap(), vec!["x#y"]);
        assert!(split_args("\"open").is_err());
    }

    #[test]
    fn first_value_wins_and_accumulation() {
        let c = parse(
            "Host web\n  HostName web.internal\n  User alice\n  IdentityFile ~/.ssh/web\n\
             Host *\n  User nobody\n  Port 2222\n  IdentityFile ~/.ssh/default\n  IdentityFile ~/.ssh/web\n",
        );
        let r = c.resolve("web", "me");
        assert_eq!(r.hostname, "web.internal");
        assert_eq!(r.user.as_deref(), Some("alice"));
        assert_eq!(r.port, Some(2222));
        assert_eq!(
            r.identity_files,
            vec![
                PathBuf::from("/home/me/.ssh/web"),
                PathBuf::from("/home/me/.ssh/default")
            ]
        );
        let other = c.resolve("other", "me");
        assert_eq!(other.hostname, "other");
        assert_eq!(other.user.as_deref(), Some("nobody"));
    }

    #[test]
    fn host_patterns_and_negation() {
        let c = parse(
            "Host *.prod !db.prod\n  User deploy\nHost db.prod\n  User dba\n\
             Host a b c?\n  Port 2000\nHost \"quoted name\"\n  Port 3000\n",
        );
        assert_eq!(c.resolve("api.prod", "me").user.as_deref(), Some("deploy"));
        assert_eq!(c.resolve("db.prod", "me").user.as_deref(), Some("dba"));
        assert_eq!(c.resolve("DB.PROD", "me").user.as_deref(), Some("dba"));
        assert_eq!(c.resolve("b", "me").port, Some(2000));
        assert_eq!(c.resolve("cx", "me").port, Some(2000));
        assert_eq!(c.resolve("cxx", "me").port, None);
        assert_eq!(c.resolve("quoted name", "me").port, Some(3000));
        assert_eq!(c.host_aliases(), ["db.prod", "a", "b", "quoted name"]);
    }

    #[test]
    fn aliases_dedup_in_order() {
        let c = parse("Host b a\nHost A c *\nHost !d e?\n");
        assert_eq!(c.host_aliases(), ["b", "a", "c"]);
        assert!(c.has_alias("C"));
    }

    #[test]
    fn match_criteria() {
        let c = parse(
            "Host gw\n  HostName gw.corp.example\n\
             Match host *.corp.example user root\n  Port 1\n\
             Match host *.corp.example\n  Port 2\n\
             Match originalhost gw !localuser me\n  User x\n\
             Match originalhost gw localuser me\n  User y\n\
             Match exec \"true\"\n  ConnectTimeout 5\n\
             Match all\n  ServerAliveInterval 30\n\
             Match canonical final host gw*\n  IdentitiesOnly yes\n",
        );
        let r = c.resolve("gw", "me");
        assert_eq!(r.port, Some(2));
        assert_eq!(r.user.as_deref(), Some("y"));
        assert_eq!(r.connect_timeout, None);
        assert_eq!(r.server_alive_interval, Some(Duration::from_secs(30)));
        assert_eq!(r.identities_only, Some(true));
        assert!(c.warnings().iter().any(|w| w.contains("Match exec")));

        let r = c.resolve_with(
            "gw",
            "me",
            &ResolveOverrides {
                user: Some("root".into()),
                port: None,
            },
        );
        assert_eq!(r.port, Some(1));
        assert_eq!(r.user.as_deref(), Some("root"));
    }

    #[test]
    fn unknown_match_criterion_never_matches() {
        let c = parse("Match bogus\n  Port 9\nMatch !exec foo\n  Port 8\n");
        assert_eq!(c.resolve("x", "me").port, None);
        assert_eq!(c.warnings().len(), 2);
    }

    #[test]
    fn tokens_and_tilde() {
        let c = parse(
            "Host h1\n  HostName %h.example.com\n  User bob\n  Port 2022\n\
             IdentityFile ~/.ssh/%r@%h-%p\n  IdentityFile %d/keys/%u-%n-%L-%l-%i-%%\n\
             UserKnownHostsFile ~/.ssh/kh1 /etc/kh2\n  IdentityAgent ~/agent.sock\n\
             CertificateFile ~/.ssh/%k-cert.pub\n  HostKeyAlias hk\n  IdentityFile ~/%Z\n",
        );
        let r = c.resolve("h1", "me");
        assert_eq!(r.hostname, "h1.example.com");
        assert_eq!(
            r.identity_files,
            vec![
                PathBuf::from("/home/me/.ssh/bob@h1.example.com-2022"),
                PathBuf::from("/home/me/keys/me-h1-box-box.example.org-1000-%"),
                PathBuf::from("/home/me/%Z"),
            ]
        );
        assert!(r.warnings.iter().any(|w| w.contains("%Z")));
        assert_eq!(
            r.user_known_hosts_files,
            Some(vec![
                PathBuf::from("/home/me/.ssh/kh1"),
                PathBuf::from("/etc/kh2")
            ])
        );
        assert_eq!(
            r.identity_agent,
            Some(AgentSource::Path("/home/me/agent.sock".into()))
        );
        assert_eq!(
            r.certificate_files,
            vec![PathBuf::from("/home/me/.ssh/hk-cert.pub")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn tilde_uses_backslashes_on_windows() {
        let home = Path::new(r"C:\Users\me");
        assert_eq!(expand_tilde("~/.ssh/id", home), r"C:\Users\me\.ssh\id");
        assert_eq!(expand_tilde(r"~\.ssh/a b", home), r"C:\Users\me\.ssh\a b");
        assert_eq!(expand_tilde("C:/keys/id", home), "C:/keys/id");
    }

    #[test]
    fn connection_hash_token() {
        let c = parse("Host x\n  IdentityFile %C\n");
        let r = c.resolve("x", "me");
        let name = r.identity_files[0].to_string_lossy().into_owned();
        assert_eq!(name.len(), 40);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn agent_values() {
        let c = parse("Host a\n IdentityAgent none\nHost b\n IdentityAgent SSH_AUTH_SOCK\n");
        assert_eq!(
            c.resolve("a", "me").identity_agent,
            Some(AgentSource::Disabled)
        );
        assert_eq!(
            c.resolve("b", "me").identity_agent,
            Some(AgentSource::Default)
        );
        assert_eq!(c.resolve("c", "me").identity_agent, None);
    }

    #[test]
    fn proxy_options() {
        let c = parse(
            "Host a\n  ProxyJump none\nHost b\n  ProxyCommand ssh -W %h:%p gw\n\
             Host *\n  ProxyJump bastion\n",
        );
        assert_eq!(c.resolve("a", "me").proxy_jump, None);
        let b = c.resolve("b", "me");
        assert_eq!(b.proxy_jump, None);
        assert_eq!(b.proxy_command.as_deref(), Some("ssh -W %h:%p gw"));
        assert!(b.warnings.iter().any(|w| w.contains("ProxyCommand")));
        assert_eq!(c.resolve("z", "me").proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn values_and_other_options() {
        let c = parse(
            "Host x\n  ConnectTimeout 1m30s\n  ServerAliveInterval 0\n  Port nope\n\
             ForwardAgent yes\n  SendEnv LANG\n  SendEnv LC_*\n  Bogus 1\n\
             IgnoreUnknown UseKeychain2\n  UseKeychain2 yes\n  UserKnownHostsFile none\n",
        );
        let r = c.resolve("x", "me");
        assert_eq!(r.connect_timeout, Some(Duration::from_secs(90)));
        assert_eq!(r.server_alive_interval, None);
        assert_eq!(r.port, None);
        assert!(r.warnings.iter().any(|w| w.contains("invalid port")));
        assert_eq!(r.options["forwardagent"], vec!["yes"]);
        assert_eq!(r.options["sendenv"], vec!["LANG", "LC_*"]);
        assert_eq!(r.options["bogus"], vec!["1"]);
        assert_eq!(r.user_known_hosts_files, Some(vec![]));
        let unknown: Vec<_> = c
            .warnings()
            .iter()
            .filter(|w| w.contains("unknown keyword"))
            .collect();
        assert_eq!(unknown.len(), 1, "{unknown:?}");
    }

    #[test]
    fn timeouts() {
        assert_eq!(parse_timeout("10"), Some(Some(Duration::from_secs(10))));
        assert_eq!(parse_timeout("1h5M"), Some(Some(Duration::from_secs(3900))));
        assert_eq!(parse_timeout("none"), Some(None));
        assert_eq!(parse_timeout("0"), Some(None));
        assert_eq!(parse_timeout("5x"), None);
        assert_eq!(parse_timeout("m"), None);
    }

    #[test]
    fn forwards() {
        let c = parse(
            "Host f\n  LocalForward 8080 localhost:80\n  LocalForward [::1]:9000 [fe80::1]:22\n\
             RemoteForward 0.0.0.0:2222 /run/sock\n  RemoteForward 1080\n  DynamicForward *:1081\n\
             LocalForward 1 2 3\n",
        );
        let r = c.resolve("f", "me");
        assert_eq!(
            r.forwards,
            vec![
                ForwardSpec {
                    kind: ForwardKind::Local,
                    listen: ForwardEndpoint::Tcp {
                        host: None,
                        port: 8080
                    },
                    connect: Some(ForwardEndpoint::Tcp {
                        host: Some("localhost".into()),
                        port: 80
                    }),
                },
                ForwardSpec {
                    kind: ForwardKind::Local,
                    listen: ForwardEndpoint::Tcp {
                        host: Some("::1".into()),
                        port: 9000
                    },
                    connect: Some(ForwardEndpoint::Tcp {
                        host: Some("fe80::1".into()),
                        port: 22
                    }),
                },
                ForwardSpec {
                    kind: ForwardKind::Remote,
                    listen: ForwardEndpoint::Tcp {
                        host: Some("0.0.0.0".into()),
                        port: 2222
                    },
                    connect: Some(ForwardEndpoint::Unix("/run/sock".into())),
                },
                ForwardSpec {
                    kind: ForwardKind::Remote,
                    listen: ForwardEndpoint::Tcp {
                        host: None,
                        port: 1080
                    },
                    connect: None,
                },
                ForwardSpec {
                    kind: ForwardKind::Dynamic,
                    listen: ForwardEndpoint::Tcp {
                        host: Some("*".into()),
                        port: 1081
                    },
                    connect: None,
                },
            ]
        );
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn include_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let ssh = home.join(".ssh");
        fs::create_dir_all(ssh.join("conf.d")).unwrap();
        fs::write(
            ssh.join("conf.d/10-a.conf"),
            "Host inc-a\n  HostName a.example\nHost *\n  User from-include\n",
        )
        .unwrap();
        fs::write(ssh.join("conf.d/20-b.conf"), "Host inc-b\n  Port 2200\n").unwrap();
        fs::write(ssh.join("conf.d/.hidden"), "Host hidden\n").unwrap();
        fs::write(ssh.join("scoped"), "Port 4444\nUser scoped\n").unwrap();
        fs::write(ssh.join("loop"), "Include loop\n").unwrap();
        let cfg = ssh.join("config");
        fs::write(
            &cfg,
            "Include conf.d/*.conf missing.conf\n\
             Host only-me\n  Include ~/.ssh/scoped\n  HostName only.example\n\
             Host other\n  Include loop\n",
        )
        .unwrap();
        let c = SshConfig::load_with_env(&cfg, env(home)).unwrap();
        assert_eq!(c.host_aliases(), ["inc-a", "inc-b", "only-me", "other"]);
        let a = c.resolve("inc-a", "me");
        assert_eq!(a.hostname, "a.example");
        assert_eq!(a.user.as_deref(), Some("from-include"));
        assert_eq!(c.resolve("inc-b", "me").port, Some(2200));
        let only = c.resolve("only-me", "me");
        assert_eq!(only.port, Some(4444));
        assert_eq!(only.user.as_deref(), Some("from-include"));
        assert_eq!(only.hostname, "only.example");
        assert_eq!(c.resolve("x", "me").port, None);
        assert!(c.warnings().iter().any(|w| w.contains("nesting depth")));
    }

    #[test]
    fn include_resets_host_block_after_file() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("inc"), "Host never\n  Port 1\n").unwrap();
        let cfg = ssh.join("config");
        fs::write(&cfg, "Include inc\nPort 2\n").unwrap();
        let c = SshConfig::load_with_env(&cfg, env(dir.path())).unwrap();
        assert_eq!(c.resolve("x", "me").port, Some(2));
        assert_eq!(c.resolve("never", "me").port, Some(1));
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let c = SshConfig::load_with_env(&dir.path().join("nope"), env(dir.path())).unwrap();
        assert!(c.host_aliases().is_empty());
        assert_eq!(c.resolve("x", "me").hostname, "x");
    }

    #[test]
    fn syntax_errors_are_warnings() {
        let c = parse("Host ok\n  Port \"22\n  User u\nPort\n");
        assert_eq!(c.resolve("ok", "me").user.as_deref(), Some("u"));
        assert_eq!(c.warnings().len(), 2);
    }
}
