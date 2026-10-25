//! Saved windows: each window's position and size, whether it shows the
//! sidebar, and its tabs, so all of them come back on the next launch.
//! Stored as a schema-versioned JSON file
//! (`<config_dir>/patchbay/windows.json`), rewritten atomically.
//!
//! The tabs are whatever the frontend saved (`ui/lib/stores/session.svelte.ts`)
//! and are kept as opaque JSON here; the frontend validates them on load.
//!
//! Schema 1 kept the sidebar in the tabs (`session.sidebarHidden`), so it was
//! lost with "restore tabs" off; it is moved to the window on load.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::persist;
use crate::{Error, Result};

const SCHEMA_VERSION: u32 = 2;

/// Saved windows beyond this many are dropped.
pub const MAX_WINDOWS: usize = 16;

/// A window's place on screen, in physical pixels: the outer position and
/// the inner size it had when last not maximized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedWindow {
    #[serde(default)]
    pub geometry: Option<WindowGeometry>,
    /// The hosts sidebar is hidden (restored whether or not tabs are).
    #[serde(default)]
    pub sidebar_hidden: bool,
    /// The window's tabs as saved by the frontend.
    #[serde(default)]
    pub session: Option<serde_json::Value>,
}

/// All saved windows, in the order they were opened (the first one is
/// restored into the window that exists at launch).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layouts {
    /// The frontend's "restore tabs" preference: when off, only the first
    /// window comes back, empty.
    #[serde(default = "yes")]
    pub restore_tabs: bool,
    #[serde(default)]
    pub windows: Vec<SavedWindow>,
}

impl Default for Layouts {
    fn default() -> Self {
        Self {
            restore_tabs: true,
            windows: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct LayoutsFile {
    version: u32,
    #[serde(flatten)]
    layouts: Layouts,
}

impl Layouts {
    /// `<config_dir>/patchbay/windows.json`.
    pub fn default_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| Error::Other("cannot determine the configuration directory".into()))?;
        Ok(config_dir.join("patchbay").join("windows.json"))
    }

    /// `None` if nothing was saved yet (first launch, or a version from
    /// before windows were saved here).
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let Some(text) = persist::read_optional(path)? else {
            return Ok(None);
        };
        let file: LayoutsFile = serde_json::from_str(&text)?;
        if file.version > SCHEMA_VERSION {
            return Err(Error::Other(format!(
                "{} was written by a newer version of Patchbay (schema {}, supported {})",
                path.display(),
                file.version,
                SCHEMA_VERSION
            )));
        }
        let mut layouts = file.layouts;
        layouts.windows.truncate(MAX_WINDOWS);
        if file.version < 2 {
            for w in &mut layouts.windows {
                w.sidebar_hidden = w
                    .session
                    .as_ref()
                    .and_then(|s| s.get("sidebarHidden"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            }
        }
        Ok(Some(layouts))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_vec_pretty(&LayoutsFile {
            version: SCHEMA_VERSION,
            layouts: self.clone(),
        })?;
        persist::write_atomic(path, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("patchbay/windows.json");
        assert_eq!(Layouts::load(&path).unwrap(), None);

        let layouts = Layouts {
            restore_tabs: false,
            windows: vec![
                SavedWindow {
                    geometry: Some(WindowGeometry {
                        x: -1200,
                        y: 40,
                        width: 1200,
                        height: 720,
                        maximized: true,
                    }),
                    sidebar_hidden: true,
                    session: Some(serde_json::json!({"version": 2, "tabs": []})),
                },
                SavedWindow::default(),
            ],
        };
        layouts.save(&path).unwrap();
        assert_eq!(Layouts::load(&path).unwrap(), Some(layouts));
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["restoreTabs"], false);
        assert_eq!(json["windows"][0]["geometry"]["maximized"], true);
        assert_eq!(json["windows"][0]["sidebarHidden"], true);
        assert_eq!(json["windows"][1]["sidebarHidden"], false);
        assert_eq!(json["windows"][0]["session"]["version"], 2);
    }

    #[test]
    fn partial_newer_and_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("windows.json");
        fs::write(
            &path,
            r#"{"version": 2, "windows": [{"geometry": {"x": 1, "y": 2, "width": 3, "height": 4}}, {"future": true}]}"#,
        )
        .unwrap();
        let layouts = Layouts::load(&path).unwrap().unwrap();
        assert!(layouts.restore_tabs);
        assert_eq!(layouts.windows.len(), 2);
        assert!(!layouts.windows[0].geometry.unwrap().maximized);
        assert_eq!(layouts.windows[1], SavedWindow::default());

        let many = format!(r#"{{"version": 2, "windows": [{}{{}}]}}"#, "{},".repeat(40));
        fs::write(&path, many).unwrap();
        assert_eq!(
            Layouts::load(&path).unwrap().unwrap().windows.len(),
            MAX_WINDOWS
        );

        fs::write(&path, r#"{"version": 3}"#).unwrap();
        assert!(Layouts::load(&path).is_err());
        fs::write(&path, "{").unwrap();
        assert!(Layouts::load(&path).is_err());
    }

    #[test]
    fn sidebar_moves_out_of_schema_1_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("windows.json");
        fs::write(
            &path,
            r#"{"version": 1, "restoreTabs": false, "windows": [
                {"session": {"version": 2, "tabs": [], "sidebarHidden": true}},
                {"session": {"version": 2, "tabs": [], "sidebarHidden": false}},
                {"session": {"version": 2, "tabs": []}},
                {}
            ]}"#,
        )
        .unwrap();
        let hidden: Vec<bool> = Layouts::load(&path)
            .unwrap()
            .unwrap()
            .windows
            .iter()
            .map(|w| w.sidebar_hidden)
            .collect();
        assert_eq!(hidden, [true, false, false, false]);

        // Schema 2 keeps it with the window only.
        fs::write(
            &path,
            r#"{"version": 2, "windows": [{"session": {"sidebarHidden": true}}]}"#,
        )
        .unwrap();
        assert!(!Layouts::load(&path).unwrap().unwrap().windows[0].sidebar_hidden);
    }
}
