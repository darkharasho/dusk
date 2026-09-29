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

    // An MSI that installs a service needs elevation, and there is no way to
    // ask for it from an already-running unelevated process except by
    // launching a new one with the runas verb. -Wait keeps this call
    // meaningful; without it the command returns before the install starts.
    let command = format!(
        "Start-Process msiexec.exe -ArgumentList '/i',{},'/qb' -Verb RunAs -Wait",
        powershell_quote(&msi.to_string_lossy())
    );
    check(
        service::run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &command],
        )
        .await
        .map_err(|e| ApplyError::Failed(e.to_string()))?,
        "the installer did not finish",
    )?;
    Ok(())
}

/// Single-quote for PowerShell, where the escape for a single quote is to
/// double it.
fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Let Sunshine through the Windows firewall.
///
/// Rules are bound to the program rather than to a list of ports: Sunshine
/// listens on several TCP and UDP ranges that have changed between releases,
/// and a program rule stays correct when they do. It is also narrower —
/// opening a port range admits anything that binds it.
pub async fn open_firewall(program: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "windows") {
        return Err(ApplyError::Unsupported);
    }

    let mut command = String::from("Start-Process netsh -Verb RunAs -Wait -ArgumentList ");
    let args = [
        ("Sunshine (inbound TCP)", "TCP"),
        ("Sunshine (inbound UDP)", "UDP"),
    ]
    .iter()
    .map(|(name, proto)| {
        format!(
            "'advfirewall','firewall','add','rule','name={}','dir=in','action=allow','program={}','protocol={}','enable=yes'",
            name,
            program.to_string_lossy(),
            proto
        )
    })
    .collect::<Vec<_>>();

    for arg in args {
        command.clear();
        command.push_str(&format!(
            "Start-Process netsh -Verb RunAs -Wait -ArgumentList {arg}"
        ));
        check(
            service::run(
                "powershell",
                &["-NoProfile", "-NonInteractive", "-Command", &command],
            )
            .await
            .map_err(|e| ApplyError::Failed(e.to_string()))?,
            "could not add a firewall rule",
        )?;
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

    #[test]
    fn powershell_quoting_doubles_the_quote() {
        assert_eq!(powershell_quote(r"C:\x\a.msi"), r"'C:\x\a.msi'");
        assert_eq!(powershell_quote("C:\\it's\\a.msi"), "'C:\\it''s\\a.msi'");
    }

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
