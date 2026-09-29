//! Finding the right Sunshine build to install.
//!
//! Dusk does not bundle Sunshine — it fetches the official release at first
//! run. That keeps Dusk out of GPL-3.0 conveying obligations, lets Sunshine
//! ship security fixes without a Dusk release, and means one code path
//! instead of a bundled copy per platform.
//!
//! # What verification does and does not buy
//!
//! The project publishes no checksum or signature files. GitHub's API does
//! report a per-asset `digest`, and Dusk checks downloads against it, but
//! that digest is computed by the same service that serves the file: it
//! proves the bytes arrived intact, not that the release is authentic. A
//! compromised GitHub account would produce a matching digest. Real
//! authenticity needs a signature from the project, which does not exist
//! today — so this is integrity only, and the UI should not imply more.

use serde::Deserialize;

const LATEST_RELEASE: &str =
    "https://api.github.com/repos/LizardByte/Sunshine/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex sha256, when GitHub reported one.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    /// `sha256:<hex>`; absent on older releases.
    #[serde(default)]
    digest: Option<String>,
}

pub async fn fetch_latest(client: &reqwest::Client) -> Result<Release, String> {
    let raw: RawRelease = client
        .get(LATEST_RELEASE)
        // GitHub rejects requests with no user agent.
        .header(reqwest::header::USER_AGENT, "dusk")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("could not reach GitHub: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GitHub answered {e}"))?
        .json()
        .await
        .map_err(|e| format!("could not read the release list: {e}"))?;

    Ok(Release {
        version: raw.tag_name.trim_start_matches('v').to_string(),
        assets: raw
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
                sha256: a.digest.and_then(|d| {
                    d.strip_prefix("sha256:").map(|h| h.to_ascii_lowercase())
                }),
            })
            .collect(),
    })
}

/// How a platform's asset is installed once downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// macOS: mount, copy the bundle out, unmount.
    MacosDmg,
    /// Windows: hand to msiexec.
    WindowsMsi,
    /// Linux: a single self-contained executable.
    LinuxAppImage,
}

#[derive(Debug, Clone)]
pub struct Choice {
    pub asset: Asset,
    pub kind: InstallKind,
}

/// Pick the asset for a platform and CPU.
///
/// The release carries 40-odd assets — every Fedora, Ubuntu and openSUSE
/// variant — so matching is by required tokens rather than by guessing a
/// filename, which would break the first time a version string moves.
///
/// Linux deliberately takes the AppImage rather than a distro package.
/// Choosing between `ubuntu24.04_amd64.deb` and `fc45.x86_64.rpm` means
/// knowing the distro *and* its release, and getting it wrong installs a
/// package that will not run. Where a distro already packages Sunshine, its
/// own package manager is the better answer and Dusk should defer to it.
pub fn select(assets: &[Asset], os: &str, arch: &str) -> Option<Choice> {
    let (kind, required, forbidden): (InstallKind, Vec<&str>, Vec<&str>) = match os {
        "macos" => (
            InstallKind::MacosDmg,
            vec![".dmg", "macos", macos_arch(arch)?],
            vec![],
        ),
        "windows" => (
            InstallKind::WindowsMsi,
            vec!["installer.msi", "windows", windows_arch(arch)?],
            // The lite zip and the debug symbols carry the same tokens.
            vec!["debuginfo", "lite"],
        ),
        "linux" => (
            InstallKind::LinuxAppImage,
            vec![".appimage", linux_arch(arch)?],
            vec!["debug"],
        ),
        _ => return None,
    };

    assets
        .iter()
        .find(|asset| {
            let name = asset.name.to_ascii_lowercase();
            required.iter().all(|token| name.contains(token))
                && !forbidden.iter().any(|token| name.contains(token))
        })
        .map(|asset| Choice {
            asset: asset.clone(),
            kind,
        })
}

fn macos_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("arm64"),
        "x86_64" => Some("x86_64"),
        _ => None,
    }
}

/// Windows assets are labelled AMD64/ARM64, not by the Rust target names.
fn windows_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

fn linux_arch(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real asset names from a Sunshine release, trimmed to the ones
    /// selection has to choose between. Factual filenames, not copied code.
    fn assets() -> Vec<Asset> {
        [
            "flathub.tar.gz",
            "sunshine-2026.914.233613-1-x86_64.pkg.tar.zst",
            "Sunshine-2026.914.233613-1.fc45.x86_64.rpm",
            "Sunshine-2026.914.233613-1.suse.tw.aarch64.rpm",
            "Sunshine-macOS-arm64.dmg",
            "Sunshine-macOS-x86_64.dmg",
            "Sunshine-Windows-AMD64-debuginfo.7z",
            "Sunshine-Windows-AMD64-installer.msi",
            "Sunshine-Windows-AMD64-lite.zip",
            "Sunshine-Windows-ARM64-installer.msi",
            "sunshine_2026.914.233613-1+ubuntu24.04_amd64.deb",
            "Sunshine_2026.914.233613_aarch64.AppImage",
            "Sunshine_2026.914.233613_x86_64.AppImage",
            "sunshine_x86_64.flatpak",
        ]
        .into_iter()
        .map(|name| Asset {
            name: name.to_string(),
            url: format!("https://example.invalid/{name}"),
            size: 1,
            sha256: Some("ab".repeat(32)),
        })
        .collect()
    }

    fn pick(os: &str, arch: &str) -> String {
        select(&assets(), os, arch).expect("an asset").asset.name
    }

    #[test]
    fn macos_picks_the_dmg_for_its_own_cpu() {
        assert_eq!(pick("macos", "aarch64"), "Sunshine-macOS-arm64.dmg");
        assert_eq!(pick("macos", "x86_64"), "Sunshine-macOS-x86_64.dmg");
    }

    #[test]
    fn windows_picks_the_installer_not_the_lite_zip_or_symbols() {
        // All three carry "windows" and "amd64"; only one is installable.
        assert_eq!(
            pick("windows", "x86_64"),
            "Sunshine-Windows-AMD64-installer.msi"
        );
        assert_eq!(
            pick("windows", "aarch64"),
            "Sunshine-Windows-ARM64-installer.msi"
        );
    }

    #[test]
    fn linux_takes_the_appimage_rather_than_guessing_a_distro() {
        // Choosing between the Ubuntu deb and the Fedora rpm needs the
        // distro and its release; the AppImage needs neither.
        assert_eq!(
            pick("linux", "x86_64"),
            "Sunshine_2026.914.233613_x86_64.AppImage"
        );
        assert_eq!(
            pick("linux", "aarch64"),
            "Sunshine_2026.914.233613_aarch64.AppImage"
        );
    }

    #[test]
    fn the_install_kind_travels_with_the_choice() {
        assert_eq!(
            select(&assets(), "macos", "aarch64").unwrap().kind,
            InstallKind::MacosDmg
        );
        assert_eq!(
            select(&assets(), "windows", "x86_64").unwrap().kind,
            InstallKind::WindowsMsi
        );
    }

    #[test]
    fn an_unknown_platform_or_cpu_selects_nothing() {
        assert!(select(&assets(), "freebsd", "x86_64").is_none());
        assert!(select(&assets(), "macos", "riscv64").is_none());
    }

    #[test]
    fn a_release_missing_our_asset_selects_nothing_rather_than_the_wrong_one() {
        let only_linux: Vec<Asset> = assets()
            .into_iter()
            .filter(|a| a.name.contains("AppImage"))
            .collect();
        assert!(select(&only_linux, "macos", "aarch64").is_none());
    }

    #[test]
    fn digests_are_unwrapped_from_the_sha256_prefix() {
        let raw: RawRelease = serde_json::from_str(
            r#"{"tag_name":"v2026.914.233613","assets":[
                {"name":"Sunshine-macOS-arm64.dmg",
                 "browser_download_url":"https://example.invalid/a.dmg",
                 "size":41,"digest":"sha256:AABB"}]}"#,
        )
        .expect("parses");

        let release = Release {
            version: raw.tag_name.trim_start_matches('v').to_string(),
            assets: raw
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                    sha256: a
                        .digest
                        .and_then(|d| d.strip_prefix("sha256:").map(|h| h.to_ascii_lowercase())),
                })
                .collect(),
        };

        assert_eq!(release.version, "2026.914.233613");
        assert_eq!(release.assets[0].sha256.as_deref(), Some("aabb"));
    }

    #[test]
    fn an_asset_with_no_digest_is_carried_as_none_not_as_empty() {
        // The difference matters: None means "cannot verify", and the
        // download path must refuse rather than compare against "".
        let raw: RawAsset = serde_json::from_str(
            r#"{"name":"x","browser_download_url":"https://example.invalid/x","size":1}"#,
        )
        .expect("parses");
        assert!(raw.digest.is_none());
    }
}
