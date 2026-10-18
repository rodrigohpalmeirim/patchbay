//! Path arithmetic on path strings.
//!
//! Remote paths are POSIX-style as spoken by SFTP. Windows OpenSSH servers
//! present drives as `/C:/...`; a drive root (`/C:` or `/C:/`) is treated as
//! a root, so its parent is `None` like for `/` (matching
//! [`DirListing::parent`](crate::model::DirListing)). Local paths are native
//! and handled with [`std::path`].

/// Suffix of the temporary file a transfer writes before renaming it into place.
pub const PARTIAL_SUFFIX: &str = ".patchbaypart";

/// POSIX-style paths as used by SFTP.
pub mod remote {
    /// `/C:` or `/C:/` (a Windows OpenSSH drive root).
    pub fn is_drive_root(path: &str) -> bool {
        let b = path.as_bytes();
        (b.len() == 3 || (b.len() == 4 && b[3] == b'/'))
            && b[0] == b'/'
            && b[1].is_ascii_alphabetic()
            && b[2] == b':'
    }

    /// Strip trailing slashes (keeping `/` and drive roots as `/C:/`).
    pub fn trim(path: &str) -> &str {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            if path.is_empty() { "" } else { "/" }
        } else if is_drive_root(trimmed) {
            &path[..4.min(path.len())]
        } else {
            trimmed
        }
    }

    pub fn join(dir: &str, name: &str) -> String {
        if dir.is_empty() {
            name.to_owned()
        } else if dir.ends_with('/') {
            format!("{dir}{name}")
        } else {
            format!("{dir}/{name}")
        }
    }

    /// Parent directory; `None` for `/`, a drive root or a single relative
    /// component.
    pub fn parent(path: &str) -> Option<String> {
        let path = trim(path);
        if path == "/" || is_drive_root(path) {
            return None;
        }
        let idx = path.rfind('/')?;
        let parent = &path[..idx];
        Some(if parent.is_empty() {
            "/".to_owned()
        } else if is_drive_root(parent) {
            format!("{parent}/")
        } else {
            parent.to_owned()
        })
    }

    /// Last component (`""` for `/`; the drive like `C:` for a drive root).
    pub fn file_name(path: &str) -> &str {
        let path = trim(path).trim_end_matches('/');
        path.rsplit('/').next().unwrap_or(path)
    }

    /// True if `path` equals `ancestor` or lies below it.
    pub fn is_within(path: &str, ancestor: &str) -> bool {
        let (path, ancestor) = (trim(path), trim(ancestor));
        path == ancestor
            || ancestor == "/"
            || path
                .strip_prefix(ancestor)
                .is_some_and(|rest| ancestor.ends_with('/') || rest.starts_with('/'))
    }
}

/// Native paths on this machine.
pub mod local {
    use std::path::{Component, Path};

    pub fn join(dir: &str, name: &str) -> String {
        Path::new(dir).join(name).to_string_lossy().into_owned()
    }

    /// Parent directory; `None` at `/`, a Windows drive root or the drive list (`""`).
    pub fn parent(path: &str) -> Option<String> {
        let parent = Path::new(path).parent()?;
        (!parent.as_os_str().is_empty()).then(|| parent.to_string_lossy().into_owned())
    }

    pub fn file_name(path: &str) -> String {
        match Path::new(path).file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => path.to_owned(),
        }
    }

    /// True if `path` equals `ancestor` or lies below it (lexically).
    pub fn is_within(path: &str, ancestor: &str) -> bool {
        fn clean(p: &str) -> Vec<Component<'_>> {
            Path::new(p)
                .components()
                .filter(|c| !matches!(c, Component::CurDir))
                .collect()
        }
        clean(path).starts_with(&clean(ancestor))
    }

    /// Strip the `\\?\` prefix `std::fs::canonicalize` adds on Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn simplify_verbatim(path: String) -> String {
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = path.strip_prefix(r"\\?\") {
            let b = rest.as_bytes();
            if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
                rest.to_owned()
            } else {
                path
            }
        } else {
            path
        }
    }
}

/// `name (n).ext` for keeping both files on a conflict. Hidden files without
/// another dot keep the whole name as stem, and `.tar.*` double extensions
/// stay together.
pub fn numbered_name(name: &str, n: u32) -> String {
    let (stem, ext) = split_extension(name);
    format!("{stem} ({n}){ext}")
}

fn split_extension(name: &str) -> (&str, &str) {
    let Some(dot) = name.rfind('.').filter(|&i| i > 0) else {
        return (name, "");
    };
    let stem = &name[..dot];
    if let Some(tar) = stem.len().checked_sub(4).filter(|&i| i > 0)
        && stem[tar..].eq_ignore_ascii_case(".tar")
    {
        return (&name[..tar], &name[tar..]);
    }
    (stem, &name[dot..])
}

/// File name of the partial copy of `name`.
pub fn partial_name(name: &str) -> String {
    format!("{name}{PARTIAL_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_parent() {
        assert_eq!(remote::parent("/"), None);
        assert_eq!(remote::parent("/home"), Some("/".into()));
        assert_eq!(remote::parent("/home/me/"), Some("/home".into()));
        assert_eq!(remote::parent("/home//me"), Some("/home/".into()));
        assert_eq!(remote::parent("/C:"), None);
        assert_eq!(remote::parent("/C:/"), None);
        assert_eq!(remote::parent("/C:/Users"), Some("/C:/".into()));
        assert_eq!(remote::parent("/C:/Users/me/"), Some("/C:/Users".into()));
        assert_eq!(remote::parent("relative"), None);
        assert_eq!(remote::parent("a/b"), Some("a".into()));
    }

    #[test]
    fn remote_names_and_join() {
        assert_eq!(remote::file_name("/home/me"), "me");
        assert_eq!(remote::file_name("/home/me/"), "me");
        assert_eq!(remote::file_name("/"), "");
        assert_eq!(remote::file_name("/C:/"), "C:");
        assert_eq!(remote::join("/", "a"), "/a");
        assert_eq!(remote::join("/a", "b"), "/a/b");
        assert_eq!(remote::join("/C:/", "Users"), "/C:/Users");
        assert_eq!(remote::trim("/a/b//"), "/a/b");
        assert_eq!(remote::trim("///"), "/");
        assert_eq!(remote::trim("/C:"), "/C:");
        assert_eq!(remote::trim("/C://"), "/C:/");
        assert!(remote::is_within("/a/b", "/a"));
        assert!(remote::is_within("/a", "/a/"));
        assert!(remote::is_within("/a/b", "/"));
        assert!(!remote::is_within("/ab", "/a"));
        assert!(remote::is_within("/C:/x", "/C:/"));
    }

    #[cfg(unix)]
    #[test]
    fn local_paths() {
        assert_eq!(local::parent("/"), None);
        assert_eq!(local::parent("/tmp"), Some("/".into()));
        assert_eq!(local::parent("/tmp/x/"), Some("/tmp".into()));
        assert_eq!(local::parent(""), None);
        assert_eq!(local::join("/tmp", "a b"), "/tmp/a b");
        assert_eq!(local::file_name("/tmp/a.txt"), "a.txt");
        assert!(local::is_within("/tmp/a/b", "/tmp/a"));
        assert!(local::is_within("/tmp/a", "/tmp/a/"));
        assert!(!local::is_within("/tmp/ab", "/tmp/a"));
    }

    #[test]
    fn verbatim_prefix() {
        assert_eq!(
            local::simplify_verbatim(r"\\?\C:\Users".into()),
            r"C:\Users"
        );
        assert_eq!(
            local::simplify_verbatim(r"\\?\UNC\srv\share".into()),
            r"\\srv\share"
        );
        assert_eq!(
            local::simplify_verbatim(r"\\?\Volume{x}\a".into()),
            r"\\?\Volume{x}\a"
        );
        assert_eq!(local::simplify_verbatim("/tmp".into()), "/tmp");
    }

    #[test]
    fn conflict_names() {
        assert_eq!(numbered_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered_name("report", 2), "report (2)");
        assert_eq!(numbered_name(".bashrc", 1), ".bashrc (1)");
        assert_eq!(numbered_name(".config.json", 1), ".config (1).json");
        assert_eq!(numbered_name("backup.tar.gz", 3), "backup (3).tar.gz");
        assert_eq!(numbered_name("a.b.c", 1), "a.b (1).c");
        assert_eq!(numbered_name(".tar.gz", 1), ".tar (1).gz");
        assert_eq!(partial_name("x.bin"), "x.bin.patchbaypart");
    }
}
