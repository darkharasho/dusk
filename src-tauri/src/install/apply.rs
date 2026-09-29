//! Running an install once the download has been verified.
//!
//! Three platforms, three completely different acts: mount a disk image and
//! copy a bundle out of it, hand an MSI to `msiexec` with elevation, or drop
//! an executable somewhere on PATH. Nothing here is reached unless the
//! download matched its checksum — see `download.rs`.

use std::path::{Path, PathBuf};

use super::release::InstallKind;
use crate::host::service::{self, Run};

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("{0}")]
    Failed(String),
    #[error("not supported on this platform")]
    Unsupported,
}

fn check(run: Run, what: &str) -> Result<Run, ApplyError> {
    if run.ok() {
        Ok(run)
    } else {
        Err(ApplyError::Failed(format!("{what}: {}", run.message())))
    }
}

pub async fn install(path: &Path, kind: InstallKind) -> Result<(), ApplyError> {
    match kind {
        InstallKind::MacosDmg => macos_dmg(path).await,
        InstallKind::WindowsMsi => windows_msi(path).await,
        InstallKind::LinuxAppImage => linux_appimage(path).await,
    }
}

// -------------------------------------------------------------------- macOS

/// Mount the image, copy the bundle out, unmount.
///
/// The unmount runs whatever happens. A disk image left attached after a
/// failed install is both a confusing artefact and a reason the next attempt
/// fails, so it is not conditional on success.
async fn macos_dmg(dmg: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "macos") {
        return Err(ApplyError::Unsupported);
    }

    let mount = attach(dmg).await?;
    let result = copy_bundle_from(&mount).await;
    let _ = service::run("hdiutil", &["detach", &mount.to_string_lossy(), "-quiet"]).await;
    result
}

/// Attach the image and return where it landed.
///
/// `-plist` rather than scraping the human-readable output: that output is
/// tab-aligned columns which differ between macOS versions, and picking the
/// wrong field here means copying from the wrong path.
async fn attach(dmg: &Path) -> Result<PathBuf, ApplyError> {
    let run = check(
        service::run(
            "hdiutil",
            &[
                "attach",
                &dmg.to_string_lossy(),
                "-nobrowse",
                "-noautoopen",
                "-readonly",
                "-plist",
            ],
        )
        .await
        .map_err(|e| ApplyError::Failed(e.to_string()))?,
        "could not open the disk image",
    )?;

    mount_point_from_plist(&run.stdout)
        .ok_or_else(|| ApplyError::Failed("the disk image mounted nowhere Dusk could find".into()))
}

#[cfg(target_os = "macos")]
fn mount_point_from_plist(stdout: &str) -> Option<PathBuf> {
    let value: plist::Value = plist::from_bytes(stdout.as_bytes()).ok()?;
    let entities = value.as_dictionary()?.get("system-entities")?.as_array()?;
    // Only one entity carries a mount point; the others are the raw slices.
    entities
        .iter()
        .filter_map(|e| e.as_dictionary()?.get("mount-point")?.as_string())
        .map(PathBuf::from)
        .next()
}

#[cfg(not(target_os = "macos"))]
fn mount_point_from_plist(_stdout: &str) -> Option<PathBuf> {
    None
}

async fn copy_bundle_from(mount: &Path) -> Result<(), ApplyError> {
    let bundle = find_app_bundle(mount)
        .ok_or_else(|| ApplyError::Failed("no application was found in the disk image".into()))?;
    let target = PathBuf::from("/Applications").join(
        bundle
            .file_name()
            .ok_or_else(|| ApplyError::Failed("the application had no name".into()))?,
    );

    // ditto rather than cp: it preserves the resource forks and extended
    // attributes an app bundle's signature depends on.
    let copy = service::run(
        "ditto",
        &[&bundle.to_string_lossy(), &target.to_string_lossy()],
    )
    .await
    .map_err(|e| ApplyError::Failed(e.to_string()))?;

    if copy.ok() {
        return Ok(());
    }

    // /Applications is not writable by every account. Ask for rights the way
    // macOS expects, which shows the system's own prompt rather than asking
    // for a password inside Dusk.
    let script = format!(
        "do shell script \"/usr/bin/ditto {} {}\" with administrator privileges",
        shell_quote(&bundle.to_string_lossy()),
        shell_quote(&target.to_string_lossy()),
    );
    check(
        service::run("osascript", &["-e", &script])
            .await
            .map_err(|e| ApplyError::Failed(e.to_string()))?,
        "could not copy Sunshine into Applications",
    )?;
    Ok(())
}

fn find_app_bundle(mount: &Path) -> Option<PathBuf> {
    std::fs::read_dir(mount)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "app"))
}

/// Quote a path for the shell line inside an AppleScript `do shell script`.
///
/// Two layers of quoting apply here — AppleScript's string and then the
/// shell's — so this wraps in single quotes and escapes any single quote in
/// the path, which is the only character that can end the quoting.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ------------------------------------------------------------------ Windows

async fn windows_msi(msi: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "windows") {
        return Err(ApplyError::Unsupported);
    }

    // An MSI that registers a service needs elevation. The install budget
    // rather than the ordinary one: an installer legitimately runs for
    // minutes, and the default would kill it partway through.
    let run = service::run_elevated(
        "msiexec.exe",
        &["/i", &msi.to_string_lossy(), "/qb"],
        service::INSTALL_TIMEOUT,
    )
    .await
    .map_err(|e| ApplyError::Failed(e.to_string()))?;

    if run.ok() {
        return Ok(());
    }
    Err(ApplyError::Failed(match run.status {
        // 1602 is the documented msiexec code for a user cancelling.
        Some(1602) => "The installation was cancelled.".to_string(),
        Some(code) => format!("The installer stopped with code {code}."),
        None => "The installer did not finish.".to_string(),
    }))
}

/// Let Sunshine through the Windows firewall.
///
/// Rules are bound to the program rather than to a list of ports: Sunshine
/// listens on several TCP and UDP ranges that have changed between releases,
/// and a program rule stays correct when they do. It is also narrower —
/// opening a port range admits anything that binds it.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub async fn open_firewall(program: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "windows") {
        return Err(ApplyError::Unsupported);
    }

    let program = program.to_string_lossy().into_owned();

    // One rule per protocol. Each elevates separately, which means two
    // prompts — unavoidable without writing a script file and running that,
    // which trades two prompts for a temporary file that runs as admin.
    for (name, protocol) in [
        ("Sunshine (inbound TCP)", "TCP"),
        ("Sunshine (inbound UDP)", "UDP"),
    ] {
        let run = service::run_elevated(
            "netsh.exe",
            &[
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={name}"),
                "dir=in",
                "action=allow",
                &format!("program={program}"),
                &format!("protocol={protocol}"),
                "enable=yes",
            ],
            service::ELEVATED_TIMEOUT,
        )
        .await
        .map_err(|e| ApplyError::Failed(e.to_string()))?;

        if !run.ok() {
            return Err(ApplyError::Failed(format!(
                "Could not add the {protocol} firewall rule."
            )));
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- Linux

/// Put the AppImage somewhere on PATH and make it runnable.
///
/// `~/.local/bin` rather than `/usr/local/bin`: it needs no elevation, and
/// a single-user desktop tool has no reason to install system-wide.
async fn linux_appimage(appimage: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "linux") {
        return Err(ApplyError::Unsupported);
    }

    let home = std::env::var_os("HOME")
        .ok_or_else(|| ApplyError::Failed("Dusk could not find your home directory".into()))?;
    let dir = PathBuf::from(home).join(".local/bin");
    std::fs::create_dir_all(&dir).map_err(|e| ApplyError::Failed(e.to_string()))?;

    let target = dir.join("sunshine");
    std::fs::copy(appimage, &target).map_err(|e| ApplyError::Failed(e.to_string()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An AppImage that is not executable is just a large file.
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| ApplyError::Failed(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_survives_a_path_with_a_quote_in_it() {
        // The only character that can escape single-quoting is a single
        // quote, so it is the one that has to be handled.
        assert_eq!(shell_quote("/Volumes/Sun/Sunshine.app"), "'/Volumes/Sun/Sunshine.app'");
        assert_eq!(shell_quote("/tmp/it's here"), r"'/tmp/it'\''s here'");
    }

    // PowerShell quoting and the elevation script are tested where they
    // live, in host::service.

    #[test]
    fn a_space_in_a_path_needs_no_special_case_because_it_is_quoted() {
        assert_eq!(
            shell_quote("/Volumes/Sunshine 1.0/Sunshine.app"),
            "'/Volumes/Sunshine 1.0/Sunshine.app'"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_mount_point_is_read_from_the_plist_not_from_columns() {
        // Shape hdiutil actually returns: several entities, one mounted.
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>system-entities</key><array>
  <dict><key>content-hint</key><string>GUID_partition_scheme</string></dict>
  <dict><key>mount-point</key><string>/Volumes/Sunshine</string></dict>
</array></dict></plist>"#;
        assert_eq!(
            mount_point_from_plist(plist),
            Some(PathBuf::from("/Volumes/Sunshine"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_image_that_mounted_nowhere_yields_none_rather_than_a_wrong_path() {
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>system-entities</key><array>
  <dict><key>content-hint</key><string>GUID_partition_scheme</string></dict>
</array></dict></plist>"#;
        assert_eq!(mount_point_from_plist(plist), None);
        assert_eq!(mount_point_from_plist("not a plist"), None);
    }
}
