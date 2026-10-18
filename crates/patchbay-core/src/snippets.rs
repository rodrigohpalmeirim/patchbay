//! Saved commands ("snippets"), global or limited to some hosts.
//!
//! Stored as a schema-versioned JSON file (`<config_dir>/patchbay/snippets.json`)
//! that is rewritten atomically on every change. Ids look like
//! `snip:<uuid>`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::{HostId, Snippet, SnippetDraft};
use crate::persist;
use crate::{Error, Result};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
struct SnippetsFile {
    version: u32,
    #[serde(default)]
    snippets: Vec<Snippet>,
}

/// Owner of the snippet list. Not internally synchronized; wrap in a lock
/// when shared.
pub struct SnippetStore {
    path: PathBuf,
    snippets: Vec<Snippet>,
}

impl SnippetStore {
    /// Default location: `<config_dir>/patchbay/snippets.json`.
    pub fn load_default() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| Error::Other("cannot determine the configuration directory".into()))?;
        Self::load(config_dir.join("patchbay").join("snippets.json"))
    }

    /// Explicit location. A missing file means "no snippets".
    pub fn load(path: PathBuf) -> Result<Self> {
        let snippets = read_snippets(&path)?;
        Ok(Self { path, snippets })
    }

    /// All snippets, sorted by name (case-insensitively).
    pub fn list(&self) -> Vec<Snippet> {
        let mut list = self.snippets.clone();
        list.sort_by_cached_key(|s| (s.name.to_lowercase(), s.name.clone(), s.id.clone()));
        list
    }

    pub fn get(&self, id: &str) -> Option<Snippet> {
        self.snippets.iter().find(|s| s.id == id).cloned()
    }

    pub fn create(&mut self, draft: SnippetDraft) -> Result<Snippet> {
        let snippet = validate(format!("snip:{}", uuid::Uuid::new_v4()), draft)?;
        let mut snippets = self.snippets.clone();
        snippets.push(snippet.clone());
        self.commit(snippets)?;
        Ok(snippet)
    }

    pub fn update(&mut self, id: &str, draft: SnippetDraft) -> Result<Snippet> {
        let idx = self.index(id)?;
        let snippet = validate(id.to_owned(), draft)?;
        let mut snippets = self.snippets.clone();
        snippets[idx] = snippet.clone();
        self.commit(snippets)?;
        Ok(snippet)
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        let idx = self.index(id)?;
        let mut snippets = self.snippets.clone();
        snippets.remove(idx);
        self.commit(snippets)
    }

    fn index(&self, id: &str) -> Result<usize> {
        self.snippets
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| Error::NotFound(format!("snippet {id}")))
    }

    /// Persist first, so memory never runs ahead of the file.
    fn commit(&mut self, snippets: Vec<Snippet>) -> Result<()> {
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "version": SCHEMA_VERSION,
            "snippets": &snippets,
        }))?;
        persist::write_atomic(&self.path, &body)?;
        self.snippets = snippets;
        Ok(())
    }
}

fn validate(id: String, draft: SnippetDraft) -> Result<Snippet> {
    let name = draft.name.trim().to_owned();
    if name.is_empty() {
        return Err(Error::Other("Name must not be empty".into()));
    }
    if draft.command.trim().is_empty() {
        return Err(Error::Other("Command must not be empty".into()));
    }
    let mut host_ids: Vec<HostId> = Vec::new();
    for h in draft.host_ids {
        let h = h.trim().to_owned();
        if !h.is_empty() && !host_ids.contains(&h) {
            host_ids.push(h);
        }
    }
    Ok(Snippet {
        id,
        name,
        command: draft.command,
        host_ids,
        run: draft.run,
    })
}

fn read_snippets(path: &Path) -> Result<Vec<Snippet>> {
    let Some(text) = persist::read_optional(path)? else {
        return Ok(Vec::new());
    };
    let file: SnippetsFile = serde_json::from_str(&text)?;
    if file.version > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "{} was written by a newer version of Patchbay (schema {}, supported {})",
            path.display(),
            file.version,
            SCHEMA_VERSION
        )));
    }
    Ok(file.snippets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn draft(name: &str, command: &str) -> SnippetDraft {
        SnippetDraft {
            name: name.into(),
            command: command.into(),
            ..SnippetDraft::default()
        }
    }

    #[test]
    fn crud_persists_atomically_and_sorts_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf/patchbay/snippets.json");
        let mut store = SnippetStore::load(path.clone()).unwrap();
        assert!(store.list().is_empty());
        assert!(!path.exists(), "nothing is written until the first change");

        let tail = store
            .create(SnippetDraft {
                host_ids: vec!["cfg:a".into(), " cfg:a ".into(), "".into(), "app:b".into()],
                run: true,
                ..draft(" tail logs ", "tail -f /var/log/syslog")
            })
            .unwrap();
        assert!(tail.id.starts_with("snip:"));
        assert_eq!(tail.name, "tail logs");
        assert_eq!(tail.host_ids, ["cfg:a", "app:b"]);
        assert!(tail.run);
        store.create(draft("Disk usage", "df -h ")).unwrap();
        store
            .create(draft("apt upgrade", "sudo apt upgrade"))
            .unwrap();

        let names: Vec<_> = store.list().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["apt upgrade", "Disk usage", "tail logs"]);

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["snippets"][0]["command"], "tail -f /var/log/syslog");
        assert_eq!(json["snippets"][0]["hostIds"][1], "app:b");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);

        let mut again = SnippetStore::load(path.clone()).unwrap();
        assert_eq!(again.list(), store.list());
        assert_eq!(again.get(&tail.id).unwrap(), tail);
        assert_eq!(
            again.list()[1].command,
            "df -h ",
            "commands are kept verbatim"
        );

        let updated = again
            .update(&tail.id, draft("follow", "journalctl -f"))
            .unwrap();
        assert_eq!(updated.id, tail.id);
        assert!(updated.host_ids.is_empty());
        assert!(!updated.run);
        assert_eq!(
            SnippetStore::load(path.clone())
                .unwrap()
                .get(&tail.id)
                .unwrap(),
            updated
        );

        again.delete(&tail.id).unwrap();
        assert_eq!(SnippetStore::load(path.clone()).unwrap().list().len(), 2);
        assert!(matches!(again.delete(&tail.id), Err(Error::NotFound(_))));
        assert!(matches!(
            again.update(&tail.id, draft("x", "y")),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snippets.json");
        let mut store = SnippetStore::load(path.clone()).unwrap();
        for bad in [
            draft("", "ls"),
            draft("  ", "ls"),
            draft("n", ""),
            draft("n", " \n"),
        ] {
            assert!(store.create(bad).is_err());
        }
        assert!(!path.exists());
        let ok = store.create(draft("n", "ls")).unwrap();
        assert!(store.update(&ok.id, draft("n", " ")).is_err());
        assert_eq!(store.get(&ok.id).unwrap().command, "ls");
    }

    #[test]
    fn rejects_newer_schema_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snippets.json");
        fs::write(&path, r#"{"version": 99, "snippets": []}"#).unwrap();
        assert!(SnippetStore::load(path.clone()).is_err());
        fs::write(&path, "not json").unwrap();
        assert!(SnippetStore::load(path.clone()).is_err());
        fs::write(&path, r#"{"version": 1}"#).unwrap();
        assert!(SnippetStore::load(path).unwrap().list().is_empty());
    }
}
