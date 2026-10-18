//! Small helpers for the JSON files Patchbay keeps in its config directory.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::Result;

/// Replace `path` with `body` (plus a trailing newline) via a temp file in
/// the same directory and a rename, so readers never see a partial file.
/// Creates the parent directory if needed.
pub(crate) fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data.json".into());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(body)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

/// Read a file, treating a missing file as `None`.
pub(crate) fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(crate::Error::ReadFile {
            path: path.to_owned(),
            source,
        }),
    }
}
