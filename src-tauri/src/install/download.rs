//! Fetching a release asset and checking it arrived intact.
//!
//! The hash is computed as the bytes stream past rather than by reading the
//! file back: a 40MB DMG should not be read twice, and nothing should ever
//! touch a file that has not been checked.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::release::Asset;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("the download failed: {0}")]
    Transport(String),
    #[error("could not write the download: {0}")]
    Disk(String),
    #[error("GitHub published no checksum for {0}, so Dusk will not install it")]
    NoChecksum(String),
    #[error("the download did not match its checksum and has been discarded")]
    Corrupt,
}

/// Progress, for a UI that would otherwise show nothing for a minute.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub received: u64,
    pub total: u64,
}

/// Download `asset` into `dir`, verify it, and return the path.
///
/// Refuses outright when the release carries no digest. An unverifiable
/// installer is not better than no installer: this runs with elevated
/// rights a moment later.
pub async fn fetch(
    client: &reqwest::Client,
    asset: &Asset,
    dir: &Path,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, DownloadError> {
    let Some(expected) = asset.sha256.as_deref() else {
        return Err(DownloadError::NoChecksum(asset.name.clone()));
    };

    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| DownloadError::Disk(e.to_string()))?;

    // Written under a partial name so an interrupted run can never leave
    // something that looks like a finished, verified download.
    let final_path = dir.join(&asset.name);
    let partial = dir.join(format!("{}.partial", asset.name));

    let response = client
        .get(&asset.url)
        .header(reqwest::header::USER_AGENT, "dusk")
        .send()
        .await
        .map_err(|e| DownloadError::Transport(e.to_string()))?
        .error_for_status()
        .map_err(|e| DownloadError::Transport(e.to_string()))?;

    let total = response.content_length().unwrap_or(asset.size);
    let mut file = tokio::fs::File::create(&partial)
        .await
        .map_err(|e| DownloadError::Disk(e.to_string()))?;

    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DownloadError::Transport(e.to_string()))?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        file.write_all(&chunk)
            .await
            .map_err(|e| DownloadError::Disk(e.to_string()))?;
        on_progress(Progress { received, total });
    }

    file.flush()
        .await
        .map_err(|e| DownloadError::Disk(e.to_string()))?;
    drop(file);

    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        // Remove it rather than leave a bad installer on disk for someone
        // to find and run by hand later.
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(DownloadError::Corrupt);
    }

    tokio::fs::rename(&partial, &final_path)
        .await
        .map_err(|e| DownloadError::Disk(e.to_string()))?;
    Ok(final_path)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn the_known_sha256_of_abc_round_trips() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex(&h.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Exercises the whole path against a real release: fetch the asset
    /// list, take the smallest asset, download it and verify the digest.
    ///
    /// Ignored by default because it needs the network. Run it with
    /// `cargo test -- --ignored` when touching this module — the parts that
    /// matter here (streaming hash, digest comparison) cannot be proven by
    /// a unit test alone.
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn a_real_asset_downloads_and_verifies() {
        let client = reqwest::Client::new();
        let release = super::super::release::fetch_latest(&client)
            .await
            .expect("release list");

        let smallest = release
            .assets
            .iter()
            .filter(|a| a.sha256.is_some())
            .min_by_key(|a| a.size)
            .expect("an asset with a digest");

        let dir = std::env::temp_dir().join("dusk-download-test");
        let path = fetch(&client, smallest, &dir, |_| {})
            .await
            .expect("downloads and verifies");

        let written = std::fs::metadata(&path).expect("exists").len();
        assert_eq!(written, smallest.size, "size must match the release");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same download with a deliberately wrong digest must be rejected
    /// and must leave nothing behind.
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn a_mismatched_checksum_is_rejected_and_the_file_removed() {
        let client = reqwest::Client::new();
        let release = super::super::release::fetch_latest(&client)
            .await
            .expect("release list");

        let mut tampered = release
            .assets
            .iter()
            .filter(|a| a.sha256.is_some())
            .min_by_key(|a| a.size)
            .expect("an asset")
            .clone();
        tampered.sha256 = Some("00".repeat(32));

        let dir = std::env::temp_dir().join("dusk-download-bad");
        let result = fetch(&client, &tampered, &dir, |_| {}).await;

        assert!(matches!(result, Err(DownloadError::Corrupt)));
        assert!(
            !dir.join(&tampered.name).exists(),
            "a failed download must not be left on disk"
        );
        assert!(
            !dir.join(format!("{}.partial", tampered.name)).exists(),
            "the partial file must be cleaned up too"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_asset_with_no_checksum_is_refused_before_anything_is_written() {
        let dir = std::env::temp_dir().join("dusk-test-nochecksum");
        let asset = Asset {
            name: "x.dmg".into(),
            url: "https://example.invalid/x.dmg".into(),
            size: 1,
            sha256: None,
        };

        let result = fetch(&reqwest::Client::new(), &asset, &dir, |_| {}).await;
        assert!(matches!(result, Err(DownloadError::NoChecksum(_))));
        // Nothing should have been created on the way to refusing.
        assert!(!dir.join("x.dmg").exists());
    }
}
