//! The Sunshine installer that ships inside Dusk.
//!
//! A release build carries one pinned Sunshine installer for its platform,
//! staged by `scripts/stage-sunshine.sh` beside a manifest naming its version
//! and checksum. Installing from it needs no network and installs exactly
//! the Sunshine the settings screen was tested against, which a "latest"
//! download cannot promise. Without one — a development build, or a platform
//! that does not bundle — Dusk falls back to downloading the release, as it
//! always did.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::download::hex;
use super::release::{self, Asset, InstallKind};

/// Where the staged installer lives inside the resource directory.
pub const DIR: &str = "sunshine";
const MANIFEST: &str = "bundled.json";

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    asset: String,
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct Bundled {
    pub version: String,
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub kind: InstallKind,
}

/// The bundled installer, if this build has one for this machine.
///
/// An installer for another platform or CPU is treated as absent, not as an
/// error: the release machinery staging the wrong file should cost a
/// download, not an install that cannot run.
pub fn find(resources: Option<&Path>) -> Option<Bundled> {
    find_for(resources?, std::env::consts::OS, std::env::consts::ARCH)
}

fn find_for(resources: &Path, os: &str, arch: &str) -> Option<Bundled> {
    let dir = resources.join(DIR);
    let raw = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let manifest: Manifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("dusk: the bundled Sunshine manifest is unreadable ({err}); will download");
            return None;
        }
    };

    let path = dir.join(&manifest.asset);
    let size = std::fs::metadata(&path).ok()?.len();
    let asset = Asset {
        name: manifest.asset,
        url: String::new(),
        size,
        sha256: Some(manifest.sha256.to_ascii_lowercase()),
    };
    let choice = release::select(std::slice::from_ref(&asset), os, arch)?;

    Some(Bundled {
        version: manifest.version,
        path,
        size,
        sha256: asset.sha256.unwrap_or_default(),
        kind: choice.kind,
    })
}

/// Check the installer is the one the release staged before running it with
/// elevated rights. The resource directory is writable by whoever installed
/// Dusk, which on Linux and a per-user Windows install is the person — so
/// this is the same guard the download path has, for the same reason.
pub async fn verify(bundled: &Bundled) -> Result<(), String> {
    let path = bundled.path.clone();
    let digest = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
        Ok(hex(&hasher.finalize()))
    })
    .await
    .map_err(|e| e.to_string())??;

    if digest == bundled.sha256 {
        Ok(())
    } else {
        Err("The hosting installer inside Dusk does not match its checksum, so Dusk will not run it. Reinstalling Dusk should fix this.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(asset: &str, body: &[u8], sha256: &str) -> tempdir::Dir {
        let dir = tempdir::Dir::new();
        let sun = dir.path().join(DIR);
        std::fs::create_dir_all(&sun).unwrap();
        std::fs::write(sun.join(asset), body).unwrap();
        std::fs::write(
            sun.join(MANIFEST),
            format!(r#"{{"version":"2026.914.233613","asset":"{asset}","sha256":"{sha256}"}}"#),
        )
        .unwrap();
        dir
    }

    // sha256("abc")
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn a_staged_installer_for_this_platform_is_found() {
        let dir = stage("Sunshine-Windows-AMD64-installer.msi", b"abc", ABC);
        let b = find_for(dir.path(), "windows", "x86_64").expect("found");
        assert_eq!(b.kind, InstallKind::WindowsMsi);
        assert_eq!(b.version, "2026.914.233613");
        assert_eq!(b.size, 3);
    }

    #[test]
    fn an_installer_for_another_platform_is_ignored() {
        let dir = stage("Sunshine-Windows-AMD64-installer.msi", b"abc", ABC);
        assert!(find_for(dir.path(), "linux", "x86_64").is_none());
        assert!(find_for(dir.path(), "windows", "aarch64").is_none());
    }

    #[test]
    fn nothing_staged_means_nothing_found() {
        let dir = tempdir::Dir::new();
        assert!(find_for(dir.path(), "linux", "x86_64").is_none());
    }

    #[tokio::test]
    async fn the_checksum_is_checked_before_anything_runs() {
        let dir = stage("Sunshine_2026.914.233613_x86_64.AppImage", b"abc", ABC);
        let good = find_for(dir.path(), "linux", "x86_64").unwrap();
        assert!(verify(&good).await.is_ok());

        let tampered = stage("Sunshine_2026.914.233613_x86_64.AppImage", b"abd", ABC);
        let bad = find_for(tampered.path(), "linux", "x86_64").unwrap();
        assert!(verify(&bad).await.is_err());
    }

    /// A directory removed when the test ends, without a crate for it.
    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let mut bytes = [0u8; 8];
                getrandom::getrandom(&mut bytes).unwrap();
                let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                let path = std::env::temp_dir().join(format!("dusk-bundled-{name}"));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
