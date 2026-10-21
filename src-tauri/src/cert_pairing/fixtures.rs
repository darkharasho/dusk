//! What the host-side tests share: a throwaway config directory and a host
//! whose answers are a script.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use super::reconcile::{Files, Host};
use crate::account::Membership;
use crate::store::AccountPairing;
use crate::sunshine::state_file;

pub const STATE: &str = r#"{
  "username": "u",
  "root": {
    "uniqueid": "HOST-ID",
    "named_devices": [
      {"name": "Phone", "cert": "PHONE", "uuid": "U1", "enabled": "true"}
    ]
  }
}"#;
pub const SERVER_CERT: &str = "-----BEGIN CERTIFICATE-----\nSERVER\n-----END CERTIFICATE-----\n";
pub const CLIENT_CERT: &str = "-----BEGIN CERTIFICATE-----\nCLIENT\n-----END CERTIFICATE-----\n";

/// A host config directory of its own, removed when the test ends.
pub struct Dir(pub PathBuf);

impl Dir {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dusk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sunshine_state.json"), STATE).unwrap();
        std::fs::write(dir.join("cacert.pem"), SERVER_CERT).unwrap();
        Self(dir)
    }
    pub fn files(&self) -> Files {
        Files {
            state: self.state(),
            server_cert: self.0.join("cacert.pem"),
        }
    }
    pub fn state(&self) -> PathBuf {
        self.0.join("sunshine_state.json")
    }
    /// Where Dusk's own settings go.
    pub fn store(&self) -> PathBuf {
        self.0.join("devices.json")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn uuids(path: &Path) -> Vec<String> {
    state_file::read(path).unwrap()["root"]["named_devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["uuid"].as_str().unwrap().to_string())
        .collect()
}

pub fn pairing(machine: &str, uuid: &str) -> AccountPairing {
    AccountPairing {
        machine: machine.into(),
        name: format!("{machine}'s desk"),
        cert: format!("-----BEGIN CERTIFICATE-----\n{machine}\n-----END CERTIFICATE-----\n"),
        uuid: uuid.into(),
    }
}

pub fn on_account(machines: &[&str]) -> Membership {
    Membership::Machines(
        machines
            .iter()
            .map(|m| m.to_string())
            .collect::<HashSet<_>>(),
    )
}

/// A host whose activity is a script, and whose restarts are counted along
/// with what the state file held at the moment of each.
#[derive(Default)]
pub struct Fake {
    pub script: Mutex<VecDeque<Option<bool>>>,
    pub polls: AtomicUsize,
    pub restarts: Mutex<Vec<(usize, Vec<String>)>>,
    pub file: Option<PathBuf>,
    /// Run on each poll, to stand in for the host rewriting the file.
    pub on_poll: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Fake {
    pub fn new(script: &[Option<bool>], file: &Path) -> Self {
        Self {
            script: Mutex::new(script.iter().copied().collect()),
            file: Some(file.to_path_buf()),
            ..Default::default()
        }
    }
    pub fn restarts(&self) -> Vec<(usize, Vec<String>)> {
        self.restarts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Host for Fake {
    async fn hosting(&self) -> Option<bool> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if let Some(f) = &self.on_poll {
            f();
        }
        let mut script = self.script.lock().unwrap();
        // The last answer holds once the script runs out.
        if script.len() > 1 {
            script.pop_front().unwrap()
        } else {
            script.front().copied().flatten()
        }
    }
    async fn restart(&self) -> Result<(), String> {
        let held = self.file.as_deref().map(uuids).unwrap_or_default();
        self.restarts
            .lock()
            .unwrap()
            .push((self.polls.load(Ordering::SeqCst), held));
        Ok(())
    }
}
