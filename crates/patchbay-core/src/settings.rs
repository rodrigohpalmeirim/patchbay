//! Backend-relevant app settings ([`AppSettings`]), stored as a
//! schema-versioned JSON file (`<config_dir>/patchbay/settings.json`) that is
//! rewritten atomically on every change. Missing or unknown fields fall back
//! to defaults, so older and newer files load fine.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::AppSettings;
use crate::persist;
use crate::{Error, Result};

const SCHEMA_VERSION: u32 = 1;

/// Allowed range for [`AppSettings::max_concurrent_transfers`].
pub const MAX_CONCURRENT_TRANSFERS: std::ops::RangeInclusive<u8> = 1..=16;

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    #[serde(default)]
    settings: AppSettings,
}

/// Owner of the settings. Not internally synchronized; wrap in a lock when
/// shared.
pub struct SettingsStore {
    path: PathBuf,
    settings: AppSettings,
}

impl SettingsStore {
    /// `<config_dir>/patchbay/settings.json`.
    pub fn default_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| Error::Other("cannot determine the configuration directory".into()))?;
        Ok(config_dir.join("patchbay").join("settings.json"))
    }

    /// Load from [`default_path`](Self::default_path).
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    /// Defaults, saved to `path` on the next [`set`](Self::set) (e.g. to
    /// replace an unreadable file).
    pub fn with_defaults(path: PathBuf) -> Self {
        Self {
            path,
            settings: AppSettings::default(),
        }
    }

    /// Explicit location. A missing file means defaults.
    pub fn load(path: PathBuf) -> Result<Self> {
        let settings = read(&path)?;
        Ok(Self { path, settings })
    }

    pub fn get(&self) -> AppSettings {
        self.settings.clone()
    }

    /// Normalize (clamp numbers into range), persist and return the stored
    /// settings.
    pub fn set(&mut self, settings: AppSettings) -> Result<AppSettings> {
        let settings = normalize(settings);
        let body = serde_json::to_vec_pretty(&SettingsFile {
            version: SCHEMA_VERSION,
            settings: settings.clone(),
        })?;
        persist::write_atomic(&self.path, &body)?;
        self.settings = settings.clone();
        Ok(settings)
    }
}

fn normalize(mut s: AppSettings) -> AppSettings {
    s.max_concurrent_transfers = s.max_concurrent_transfers.clamp(
        *MAX_CONCURRENT_TRANSFERS.start(),
        *MAX_CONCURRENT_TRANSFERS.end(),
    );
    s
}

fn read(path: &Path) -> Result<AppSettings> {
    let Some(text) = persist::read_optional(path)? else {
        return Ok(AppSettings::default());
    };
    let file: SettingsFile = serde_json::from_str(&text)?;
    if file.version > SCHEMA_VERSION {
        return Err(Error::Other(format!(
            "{} was written by a newer version of Patchbay (schema {}, supported {})",
            path.display(),
            file.version,
            SCHEMA_VERSION
        )));
    }
    Ok(normalize(file.settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_persist_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patchbay/settings.json");
        let mut store = SettingsStore::load(path.clone()).unwrap();
        assert_eq!(store.get(), AppSettings::default());
        assert!(!path.exists());

        let saved = store
            .set(AppSettings {
                close_to_tray: false,
                launch_at_login: true,
                start_minimized: false,
                max_concurrent_transfers: 0,
                tailscale: false,
            })
            .unwrap();
        assert_eq!(saved.max_concurrent_transfers, 1);
        assert!(!saved.close_to_tray && saved.launch_at_login);
        assert_eq!(SettingsStore::load(path.clone()).unwrap().get(), saved);
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["settings"]["launchAtLogin"], true);

        let max = store
            .set(AppSettings {
                max_concurrent_transfers: 200,
                ..saved
            })
            .unwrap();
        assert_eq!(max.max_concurrent_transfers, 16);
    }

    #[test]
    fn partial_newer_and_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"version": 1, "settings": {"closeToTray": false, "futureOption": 1}}"#,
        )
        .unwrap();
        let s = SettingsStore::load(path.clone()).unwrap().get();
        assert_eq!(
            s,
            AppSettings {
                close_to_tray: false,
                ..AppSettings::default()
            }
        );
        fs::write(&path, r#"{"version": 2}"#).unwrap();
        assert!(SettingsStore::load(path.clone()).is_err());
        fs::write(&path, "{").unwrap();
        assert!(SettingsStore::load(path).is_err());
    }
}
