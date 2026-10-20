//! Persistence for the manual address book and Dusk's startup settings.
//!
//! Only hand-entered machines are stored. Anything found over mDNS is
//! rediscovered on launch, so writing it down would just create stale cards.
//! The file keeps its old name, `devices.json`, so an existing address book
//! is still found.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualEntry {
    pub address: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub manual: Vec<ManualEntry>,
    pub startup: crate::startup::Prefs,
    /// Let a machine that is not on this one's GitHub account pair with it:
    /// another person's Dusk by an Allow press here, anything else by a PIN.
    /// Off by default, because both need someone at this machine, and your
    /// own machines never do.
    pub allow_other_devices: bool,
    /// How big a stream from this machine asks to be.
    pub resolution: Resolution,
}

/// How big a stream asks to be.
///
/// Matching the host is the default because a stream whose shape differs
/// from the host's screen gets bars on the host's side, baked into the
/// picture, and the pointer cannot reach into them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Resolution {
    #[default]
    MatchHost,
    MatchThisDisplay,
    Fixed {
        width: u32,
        height: u32,
    },
}

impl Resolution {
    /// Sizes a stream can actually be: encoders want even dimensions, and
    /// nothing outside these bounds is a screen.
    pub fn is_valid(&self) -> bool {
        match *self {
            Resolution::Fixed { width, height } => {
                (320..=15360).contains(&width)
                    && (240..=8640).contains(&height)
                    && width % 2 == 0
                    && height % 2 == 0
            }
            _ => true,
        }
    }
}

impl Store {
    /// Read the address book, treating a missing or corrupt file as empty.
    ///
    /// A parse failure must not stop the app from starting — the grid still
    /// works from discovery alone, and refusing to launch over a bad config
    /// file would be a worse outcome than losing the list.
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(store) => store,
            Err(err) => {
                eprintln!("dusk: ignoring unreadable address book at {path:?}: {err}");
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn upsert(&mut self, entry: ManualEntry) {
        match self
            .manual
            .iter_mut()
            .find(|e| e.address.eq_ignore_ascii_case(&entry.address) && e.port == entry.port)
        {
            Some(existing) => *existing = entry,
            None => self.manual.push(entry),
        }
    }

    pub fn remove(&mut self, address: &str, port: Option<u16>) {
        self.manual
            .retain(|e| !(e.address.eq_ignore_ascii_case(address) && e.port == port));
    }
}

pub fn store_path(config_dir: &Path) -> PathBuf {
    config_dir.join("devices.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_from_before_the_setting_keeps_other_devices_out() {
        let stored = r#"{"manual":[],"startup":{"startInBackground":true,"closeToTray":true}}"#;
        let store: Store = serde_json::from_str(stored).unwrap();
        assert!(!store.allow_other_devices);
    }

    #[test]
    fn a_file_from_before_the_setting_matches_the_host() {
        let stored = r#"{"manual":[],"allow_other_devices":true}"#;
        let store: Store = serde_json::from_str(stored).unwrap();
        assert_eq!(store.resolution, Resolution::MatchHost);
    }

    #[test]
    fn a_resolution_round_trips_in_the_shape_the_window_sends() {
        let fixed: Resolution =
            serde_json::from_str(r#"{"kind":"fixed","width":1920,"height":1080}"#).unwrap();
        assert_eq!(
            fixed,
            Resolution::Fixed {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(
            serde_json::to_string(&Resolution::MatchThisDisplay).unwrap(),
            r#"{"kind":"matchThisDisplay"}"#
        );
    }

    #[test]
    fn only_a_real_screen_size_is_a_resolution() {
        assert!(Resolution::Fixed {
            width: 2560,
            height: 1440
        }
        .is_valid());
        assert!(!Resolution::Fixed {
            width: 0,
            height: 1440
        }
        .is_valid());
        assert!(!Resolution::Fixed {
            width: 1921,
            height: 1080
        }
        .is_valid());
        assert!(Resolution::MatchHost.is_valid());
    }

    #[test]
    fn a_corrupt_file_reads_as_empty_rather_than_failing() {
        let dir = std::env::temp_dir().join("dusk-test-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("devices.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Store::load(&path).manual.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating() {
        let mut store = Store::default();
        store.upsert(ManualEntry {
            address: "192.168.1.40".into(),
            port: None,
            name: None,
        });
        store.upsert(ManualEntry {
            address: "192.168.1.40".into(),
            port: None,
            name: Some("Workshop PC".into()),
        });
        assert_eq!(store.manual.len(), 1);
        assert_eq!(store.manual[0].name.as_deref(), Some("Workshop PC"));
    }
}
