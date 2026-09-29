//! Persistence for the manual address book.
//!
//! Only hand-entered machines are stored. Anything found over mDNS is
//! rediscovered on launch, so writing it down would just create stale cards.

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
