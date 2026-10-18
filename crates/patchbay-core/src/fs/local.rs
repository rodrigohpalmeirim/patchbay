//! Local filesystem operations. Everything here blocks; [`super::FileSystem`]
//! runs it on tokio's blocking pool.

use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Meta;
use super::path::local as lpath;
use crate::model::{DirListing, FileEntry, FileKind};
use crate::{Error, Result};

/// Map an I/O error for an operation on `path`.
pub(crate) fn io_err(e: io::Error, path: &str) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => Error::NotFound(path.to_owned()),
        io::ErrorKind::PermissionDenied => Error::PermissionDenied(path.to_owned()),
        io::ErrorKind::AlreadyExists => Error::AlreadyExists(path.to_owned()),
        kind => Error::Io(io::Error::new(kind, format!("{path}: {e}"))),
    }
}

pub(crate) fn home() -> Result<String> {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| Error::Other("cannot determine the home directory".into()))
}

/// Absolute path with symlinks resolved (without `\\?\` on Windows).
pub(crate) fn canonical(path: &str) -> Result<String> {
    let canonical = fs::canonicalize(path).map_err(|e| io_err(e, path))?;
    let canonical = canonical.to_string_lossy().into_owned();
    #[cfg(windows)]
    let canonical = lpath::simplify_verbatim(canonical);
    Ok(canonical)
}

pub(crate) fn list(path: &str) -> Result<DirListing> {
    #[cfg(windows)]
    if path.is_empty() {
        return Ok(DirListing {
            path: String::new(),
            parent: None,
            entries: drives(),
        });
    }
    let path = if path.is_empty() { "/" } else { path };
    let dir = canonical(path)?;
    let entries = read_dir(&dir)?;
    // On Windows, up from a drive (or UNC share) root is the drive list.
    let parent = lpath::parent(&dir).or_else(|| cfg!(windows).then(String::new));
    Ok(DirListing {
        parent,
        path: dir,
        entries,
    })
}

/// Entries of a directory; unreadable entries are left out.
pub(crate) fn read_dir(dir: &str) -> Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir).map_err(|e| io_err(e, dir))? {
        let Ok(item) = item else { continue };
        let path = item.path();
        let Ok(md) = item.metadata() else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        entries.push(entry(&path, name, &md));
    }
    Ok(entries)
}

/// Subdirectories (not following symlinks) and entries matching `want`.
pub(crate) fn scan_dir(
    dir: &str,
    want: &dyn Fn(&str) -> bool,
) -> Result<(Vec<String>, Vec<FileEntry>)> {
    let mut subdirs = Vec::new();
    let mut matches = Vec::new();
    for item in fs::read_dir(dir).map_err(|e| io_err(e, dir))? {
        let Ok(item) = item else { continue };
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            subdirs.push(path.to_string_lossy().into_owned());
        }
        if want(&name)
            && let Ok(md) = item.metadata()
        {
            matches.push(entry(&path, name, &md));
        }
    }
    Ok((subdirs, matches))
}

/// The drive letters that exist, as directory entries.
#[cfg(windows)]
fn drives() -> Vec<FileEntry> {
    (b'A'..=b'Z')
        .filter_map(|letter| {
            let root = format!("{}:\\", letter as char);
            let md = fs::metadata(&root).ok()?;
            Some(FileEntry {
                name: format!("{}:", letter as char),
                path: root,
                kind: FileKind::Dir,
                link_target: None,
                target_kind: None,
                size: 0,
                modified: unix_secs(md.modified().ok()),
                permissions: None,
                owner: None,
                group: None,
                hidden: false,
            })
        })
        .collect()
}

pub(crate) fn stat(path: &str) -> Result<FileEntry> {
    let md = fs::symlink_metadata(path).map_err(|e| io_err(e, path))?;
    Ok(entry(Path::new(path), lpath::file_name(path), &md))
}

fn entry(path: &Path, name: String, md: &Metadata) -> FileEntry {
    let kind = kind_of(md);
    let (link_target, target_kind) = if kind == FileKind::Symlink {
        (
            fs::read_link(path)
                .ok()
                .map(|t| t.to_string_lossy().into_owned()),
            fs::metadata(path).ok().map(|m| kind_of(&m)),
        )
    } else {
        (None, None)
    };
    let (owner, group) = owner_group(md);
    FileEntry {
        hidden: name.starts_with('.') || is_hidden_attr(md),
        name,
        path: path.to_string_lossy().into_owned(),
        kind,
        link_target,
        target_kind,
        size: md.len(),
        modified: unix_secs(md.modified().ok()),
        permissions: permissions(md),
        owner,
        group,
    }
}

pub(crate) fn kind_of(md: &Metadata) -> FileKind {
    let ft = md.file_type();
    if ft.is_symlink() {
        FileKind::Symlink
    } else if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn unix_secs(t: Option<SystemTime>) -> Option<i64> {
    let t = t?;
    Some(match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    })
}

#[cfg(unix)]
fn permissions(md: &Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}

#[cfg(not(unix))]
fn permissions(_md: &Metadata) -> Option<u32> {
    None
}

#[cfg(windows)]
fn is_hidden_attr(md: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    md.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden_attr(_md: &Metadata) -> bool {
    false
}

#[cfg(unix)]
fn owner_group(md: &Metadata) -> (Option<String>, Option<String>) {
    use std::os::unix::fs::MetadataExt;
    (
        Some(ids::user_name(md.uid())),
        Some(ids::group_name(md.gid())),
    )
}

#[cfg(not(unix))]
fn owner_group(_md: &Metadata) -> (Option<String>, Option<String>) {
    (None, None)
}

pub(crate) fn meta(md: &Metadata) -> Meta {
    #[cfg(unix)]
    let (uid, gid) = {
        use std::os::unix::fs::MetadataExt;
        (Some(md.uid()), Some(md.gid()))
    };
    #[cfg(not(unix))]
    let (uid, gid) = (None, None);
    Meta {
        kind: kind_of(md),
        size: md.len(),
        mtime: unix_secs(md.modified().ok()),
        perms: permissions(md),
        uid,
        gid,
    }
}

/// `Ok(None)` if nothing exists at `path`.
pub(crate) fn lstat(path: &str) -> Result<Option<Meta>> {
    match fs::symlink_metadata(path) {
        Ok(md) => Ok(Some(meta(&md))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(e, path)),
    }
}

/// Like [`lstat`] but follows symlinks.
pub(crate) fn stat_meta(path: &str) -> Result<Option<Meta>> {
    match fs::metadata(path) {
        Ok(md) => Ok(Some(meta(&md))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(e, path)),
    }
}

/// Names and lstat metadata of a directory's entries.
pub(crate) fn read_dir_meta(dir: &str) -> Result<Vec<(String, Meta)>> {
    let mut out = Vec::new();
    for item in fs::read_dir(dir).map_err(|e| io_err(e, dir))? {
        let item = item.map_err(|e| io_err(e, dir))?;
        let md = item.metadata().map_err(|e| io_err(e, dir))?;
        out.push((item.file_name().to_string_lossy().into_owned(), meta(&md)));
    }
    Ok(out)
}

pub(crate) fn mkdir(path: &str) -> Result<()> {
    fs::create_dir(path).map_err(|e| io_err(e, path))
}

pub(crate) fn create_file(path: &str) -> Result<()> {
    File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
        .map_err(|e| io_err(e, path))
}

/// Rename without replacing an existing target.
pub(crate) fn rename(from: &str, to: &str) -> Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(Error::AlreadyExists(to.to_owned()));
    }
    fs::rename(from, to).map_err(|e| io_err(e, from))
}

/// Rename, replacing a file at `to`.
pub(crate) fn replace(from: &str, to: &str) -> Result<()> {
    fs::rename(from, to).map_err(|e| io_err(e, to))
}

/// Delete a file, a symlink (never its target) or a whole directory tree.
pub(crate) fn delete(path: &str) -> Result<()> {
    let md = fs::symlink_metadata(path).map_err(|e| io_err(e, path))?;
    let result = if md.file_type().is_symlink() {
        // Directory symlinks / junctions on Windows need remove_dir.
        fs::remove_file(path).or_else(|e| fs::remove_dir(path).map_err(|_| e))
    } else if md.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|e| io_err(e, path))
}

pub(crate) fn remove_file(path: &str) -> Result<()> {
    fs::remove_file(path).map_err(|e| io_err(e, path))
}

/// Visit `path` and, for directories, everything below it without following
/// symlinks. Directories are visited before their contents when `pre_order`.
fn walk(
    path: &Path,
    pre_order: bool,
    f: &mut impl FnMut(&Path, &Metadata) -> io::Result<()>,
) -> io::Result<()> {
    let md = fs::symlink_metadata(path)?;
    if md.is_dir() {
        if pre_order {
            f(path, &md)?;
        }
        for item in fs::read_dir(path)? {
            walk(&item?.path(), pre_order, f)?;
        }
        if !pre_order {
            f(path, &md)?;
        }
        Ok(())
    } else {
        f(path, &md)
    }
}

/// Whether a recursive change applied with `mode` should visit directories
/// before their contents (the directory stays traversable) or after.
pub(crate) fn mode_keeps_dir_traversable(mode: u32) -> bool {
    mode & 0o500 == 0o500
}

/// Set permission bits; with `recursive`, also below directories (symlinks
/// are not followed below the top level). On Windows only the read-only flag
/// is derived from the owner write bit.
pub(crate) fn chmod(path: &str, mode: u32, recursive: bool) -> Result<()> {
    let set = |p: &Path| -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(p, fs::Permissions::from_mode(mode & 0o7777))
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(p)?.permissions();
            perms.set_readonly(mode & 0o200 == 0);
            fs::set_permissions(p, perms)
        }
    };
    let top = Path::new(path);
    let is_dir = fs::symlink_metadata(top)
        .map_err(|e| io_err(e, path))?
        .is_dir();
    if !(recursive && is_dir) {
        return set(top).map_err(|e| io_err(e, path));
    }
    walk(top, mode_keeps_dir_traversable(mode), &mut |p, md| {
        if md.file_type().is_symlink() {
            Ok(())
        } else {
            set(p)
        }
    })
    .map_err(|e| io_err(e, path))
}

/// Change owner and/or group (numeric ids or names).
#[cfg(unix)]
pub(crate) fn chown(
    path: &str,
    owner: Option<&str>,
    group: Option<&str>,
    recursive: bool,
) -> Result<()> {
    let uid = owner.map(ids::resolve_user).transpose()?;
    let gid = group.map(ids::resolve_group).transpose()?;
    let top = Path::new(path);
    let md = fs::symlink_metadata(top).map_err(|e| io_err(e, path))?;
    if !(recursive && md.is_dir()) {
        return std::os::unix::fs::chown(top, uid, gid).map_err(|e| io_err(e, path));
    }
    walk(top, true, &mut |p, _| {
        std::os::unix::fs::lchown(p, uid, gid)
    })
    .map_err(|e| io_err(e, path))
}

#[cfg(not(unix))]
pub(crate) fn chown(_: &str, _: Option<&str>, _: Option<&str>, _: bool) -> Result<()> {
    Err(Error::Unsupported(
        "changing the owner is not supported on this system".into(),
    ))
}

/// Up to `max` bytes from the start of a file.
pub(crate) fn read_head(path: &str, max: u64) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|e| io_err(e, path))?;
    let mut buf = Vec::new();
    file.take(max)
        .read_to_end(&mut buf)
        .map_err(|e| io_err(e, path))?;
    Ok(buf)
}

/// Set modification/access time and permission bits (best effort on
/// Windows: times only).
pub(crate) fn set_times_and_mode(path: &str, mtime: Option<i64>, perms: Option<u32>) -> Result<()> {
    if let Some(t) = mtime {
        let t = if t >= 0 {
            UNIX_EPOCH + Duration::from_secs(t as u64)
        } else {
            UNIX_EPOCH - Duration::from_secs(t.unsigned_abs())
        };
        let times = fs::FileTimes::new().set_accessed(t).set_modified(t);
        open_for_times(path)
            .and_then(|f| f.set_times(times))
            .map_err(|e| io_err(e, path))?;
    }
    #[cfg(unix)]
    if let Some(mode) = perms {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|e| io_err(e, path))?;
    }
    #[cfg(not(unix))]
    let _ = perms;
    Ok(())
}

#[cfg(unix)]
fn open_for_times(path: &str) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_for_times(path: &str) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    File::options()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(unix)]
pub(crate) fn symlink(link: &str, target: &str) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| io_err(e, link))
}

#[cfg(not(unix))]
pub(crate) fn symlink(link: &str, _target: &str) -> Result<()> {
    Err(Error::Unsupported(format!("{link}: creating symlinks")))
}

pub(crate) fn read_link(path: &str) -> Result<String> {
    fs::read_link(path)
        .map(|t| t.to_string_lossy().into_owned())
        .map_err(|e| io_err(e, path))
}

/// User and group names via the C library (NSS), cached.
#[cfg(unix)]
mod ids {
    use std::collections::HashMap;
    use std::ffi::{CStr, CString};
    use std::sync::{LazyLock, Mutex};

    use crate::{Error, Result};

    static USERS: LazyLock<Mutex<HashMap<u32, String>>> = LazyLock::new(Default::default);
    static GROUPS: LazyLock<Mutex<HashMap<u32, String>>> = LazyLock::new(Default::default);

    fn cached(
        cache: &Mutex<HashMap<u32, String>>,
        id: u32,
        lookup: fn(u32) -> Option<String>,
    ) -> String {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry(id)
            .or_insert_with(|| lookup(id).unwrap_or_else(|| id.to_string()))
            .clone()
    }

    pub(super) fn user_name(uid: u32) -> String {
        cached(&USERS, uid, |uid| {
            // SAFETY: getpwuid_r writes into `pwd`/`buf`, which outlive the call;
            // `pw_name` points into `buf` and is read before `buf` is dropped.
            with_buffer(|buf| unsafe {
                let mut pwd: libc::passwd = std::mem::zeroed();
                let mut out = std::ptr::null_mut();
                let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out);
                (
                    rc,
                    (!out.is_null())
                        .then(|| CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned()),
                )
            })
        })
    }

    pub(super) fn group_name(gid: u32) -> String {
        cached(&GROUPS, gid, |gid| {
            // SAFETY: as in `user_name`.
            with_buffer(|buf| unsafe {
                let mut grp: libc::group = std::mem::zeroed();
                let mut out = std::ptr::null_mut();
                let rc = libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut out);
                (
                    rc,
                    (!out.is_null())
                        .then(|| CStr::from_ptr(grp.gr_name).to_string_lossy().into_owned()),
                )
            })
        })
    }

    pub(super) fn resolve_user(name: &str) -> Result<u32> {
        if let Ok(id) = name.parse() {
            return Ok(id);
        }
        let cname = CString::new(name).map_err(|_| Error::NotFound(format!("user {name}")))?;
        // SAFETY: as in `user_name`; only `pw_uid` is read.
        with_buffer(|buf| unsafe {
            let mut pwd: libc::passwd = std::mem::zeroed();
            let mut out = std::ptr::null_mut();
            let rc = libc::getpwnam_r(
                cname.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut out,
            );
            (rc, (!out.is_null()).then_some(pwd.pw_uid))
        })
        .ok_or_else(|| Error::NotFound(format!("user {name}")))
    }

    pub(super) fn resolve_group(name: &str) -> Result<u32> {
        if let Ok(id) = name.parse() {
            return Ok(id);
        }
        let cname = CString::new(name).map_err(|_| Error::NotFound(format!("group {name}")))?;
        // SAFETY: as in `user_name`; only `gr_gid` is read.
        with_buffer(|buf| unsafe {
            let mut grp: libc::group = std::mem::zeroed();
            let mut out = std::ptr::null_mut();
            let rc = libc::getgrnam_r(
                cname.as_ptr(),
                &mut grp,
                buf.as_mut_ptr(),
                buf.len(),
                &mut out,
            );
            (rc, (!out.is_null()).then_some(grp.gr_gid))
        })
        .ok_or_else(|| Error::NotFound(format!("group {name}")))
    }

    /// Run a `get*_r` call, growing the scratch buffer on `ERANGE`.
    fn with_buffer<T>(
        mut call: impl FnMut(&mut [libc::c_char]) -> (libc::c_int, Option<T>),
    ) -> Option<T> {
        let mut buf = vec![0 as libc::c_char; 1024];
        loop {
            let (rc, value) = call(&mut buf);
            if rc == libc::ERANGE && buf.len() < 1 << 20 {
                buf.resize(buf.len() * 2, 0);
                continue;
            }
            return value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn create_list_rename_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let root = s(tmp.path());
        let dir = lpath::join(&root, "sub");
        let file = lpath::join(&dir, "a.txt");
        mkdir(&dir).unwrap();
        assert!(matches!(mkdir(&dir), Err(Error::AlreadyExists(_))));
        create_file(&file).unwrap();
        assert!(matches!(create_file(&file), Err(Error::AlreadyExists(_))));
        fs::write(lpath::join(&dir, ".hidden"), b"x").unwrap();

        let listing = list(&dir).unwrap();
        assert_eq!(listing.path, canonical(&dir).unwrap());
        assert_eq!(listing.parent, Some(canonical(&root).unwrap()));
        let mut names: Vec<_> = listing
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.hidden))
            .collect();
        names.sort();
        assert_eq!(names, [(".hidden", true), ("a.txt", false)]);
        let a = listing.entries.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(a.kind, FileKind::File);
        assert_eq!(a.size, 0);
        assert!(a.modified.is_some());

        let renamed = lpath::join(&dir, "b.txt");
        rename(&file, &renamed).unwrap();
        create_file(&file).unwrap();
        assert!(matches!(
            rename(&file, &renamed),
            Err(Error::AlreadyExists(_))
        ));
        assert!(matches!(
            stat(&lpath::join(&dir, "nope")),
            Err(Error::NotFound(_))
        ));

        delete(&dir).unwrap();
        assert!(lstat(&dir).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_reported_and_never_followed_on_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let root = s(tmp.path());
        let keep = lpath::join(&root, "keep");
        fs::create_dir(&keep).unwrap();
        fs::write(lpath::join(&keep, "precious"), b"x").unwrap();
        let work = lpath::join(&root, "work");
        fs::create_dir(&work).unwrap();
        std::os::unix::fs::symlink(&keep, lpath::join(&work, "link")).unwrap();
        std::os::unix::fs::symlink("missing", lpath::join(&work, "dangling")).unwrap();

        let mut entries = list(&work).unwrap().entries;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(entries[0].name, "dangling");
        assert_eq!(entries[0].kind, FileKind::Symlink);
        assert_eq!(entries[0].link_target.as_deref(), Some("missing"));
        assert_eq!(entries[0].target_kind, None);
        assert_eq!(entries[1].target_kind, Some(FileKind::Dir));
        assert!(entries[1].owner.is_some());

        delete(&work).unwrap();
        assert!(Path::new(&lpath::join(&keep, "precious")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn chmod_recursive_and_times() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = s(tmp.path());
        let dir = lpath::join(&root, "d");
        fs::create_dir_all(lpath::join(&dir, "e")).unwrap();
        let file = lpath::join(&dir, "e/f");
        fs::write(&file, b"hello world").unwrap();
        chmod(&dir, 0o750, true).unwrap();
        let mode = |p: &str| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&dir), 0o750);
        assert_eq!(mode(&file), 0o750);
        chmod(&file, 0o600, false).unwrap();
        assert_eq!(mode(&file), 0o600);

        set_times_and_mode(&file, Some(1_000_000_000), Some(0o640)).unwrap();
        let entry = stat(&file).unwrap();
        assert_eq!(entry.modified, Some(1_000_000_000));
        assert_eq!(entry.permissions, Some(0o640));
        assert_eq!(read_head(&file, 5).unwrap(), b"hello");

        // Our own ids/names always resolve.
        let uid = fs::metadata(&file)
            .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
            .unwrap();
        chown(&file, Some(&uid.to_string()), None, false).unwrap();
        let name = ids::user_name(uid);
        assert_eq!(ids::resolve_user(&name).unwrap(), uid);
    }

    /// `""` lists the drives, and a drive root leads up to it.
    #[cfg(windows)]
    #[test]
    fn drive_list_and_drive_roots() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let drives = list("").unwrap();
        assert_eq!((drives.path.as_str(), drives.parent), ("", None));
        let entry = drives.entries.iter().find(|e| e.name == system).unwrap();
        assert_eq!(entry.path, format!("{system}\\"));
        assert_eq!(entry.kind, FileKind::Dir);

        let root = list(&entry.path).unwrap();
        assert_eq!(root.path, entry.path);
        assert_eq!(root.parent.as_deref(), Some(""));
        let windows = list(&format!("{system}\\Windows")).unwrap();
        assert_eq!(windows.parent.as_deref(), Some(&*entry.path));
    }
}
