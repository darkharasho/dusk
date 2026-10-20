//! Running an install once the download has been verified.
//!
//! Three platforms, three completely different acts: mount a disk image and
//! copy a bundle out of it, hand an MSI to `msiexec` with elevation, or drop
//! an executable somewhere on PATH. Nothing here is reached unless the
//! download matched its checksum — see `download.rs`.

use std::path::{Path, PathBuf};

use super::embedded;
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
    let bundle = result?;
    register_launch_agent(&embedded::program(&bundle))
}

/// Turn on the hosting helper inside this Dusk (see `embedded`): write a
/// launch agent that runs it. Nothing is copied anywhere.
pub async fn install_embedded(helper: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "macos") {
        return Err(ApplyError::Unsupported);
    }
    let exe = std::env::current_exe().map_err(|e| ApplyError::Failed(e.to_string()))?;
    if let Some(why) = embedded::unsettled(&exe) {
        return Err(ApplyError::Failed(why.into()));
    }

    let plist = agent_path()?;
    // An agent from an earlier copy of Dusk that has since moved is Dusk's
    // own and points at nothing; anyone else's is left alone, as below.
    let stale = plist.exists()
        && embedded::agent_program(&plist).is_some_and(|p| embedded::is_dusk_helper(&p));
    if stale {
        // Unloaded first, so the next start bootstraps from the new plist
        // rather than kickstarting the job launchd still has for the old one.
        let domain = service::gui_domain_or_default();
        let _ = service::run(
            "launchctl",
            &["bootout", &format!("{domain}/{MACOS_LABEL}")],
        )
        .await;
        std::fs::remove_file(&plist).map_err(|e| ApplyError::Failed(e.to_string()))?;
    }
    register_launch_agent(&embedded::program(helper))
}

fn agent_path() -> Result<PathBuf, ApplyError> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| ApplyError::Failed("Dusk could not find your home directory".into()))?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{MACOS_LABEL}.plist")))
}

/// Give Sunshine a launch agent, so it runs in the background from sign-in
/// and the host backend has a job to start and stop.
///
/// The disk image carries only the app; Sunshine's own docs leave the agent
/// to the person. An agent that already exists is left alone — it may be
/// one they wrote, and overwriting it would discard whatever they put in it.
fn register_launch_agent(program: &Path) -> Result<(), ApplyError> {
    let plist = agent_path()?;
    if plist.exists() {
        return Ok(());
    }
    if let Some(dir) = plist.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ApplyError::Failed(e.to_string()))?;
    }
    std::fs::write(&plist, launch_agent(program)).map_err(|e| ApplyError::Failed(e.to_string()))
}

/// The label the host backend looks for.
const MACOS_LABEL: &str = "dev.lizardbyte.sunshine";

fn launch_agent(program: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{MACOS_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
        xml_escape(&program.to_string_lossy())
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

async fn copy_bundle_from(mount: &Path) -> Result<PathBuf, ApplyError> {
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
        return Ok(target);
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
        "could not copy the hosting service into Applications",
    )?;
    Ok(target)
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

/// Let Sunshine, and Dusk itself, through the Windows firewall.
///
/// Rules are bound to the program rather than to a list of ports: Sunshine
/// listens on several TCP and UDP ranges that have changed between releases,
/// and a program rule stays correct when they do. It is also narrower —
/// opening a port range admits anything that binds it.
///
/// Dusk needs its own rule because a connecting Dusk asks this one things
/// directly — its screen size, a PIN-free pairing — on [`crate::peer::PORT`].
/// Without it those questions time out, and a stream set to match the
/// host's screen quietly falls back to 1080p.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub async fn open_firewall(program: &Path, dusk: &Path) -> Result<(), ApplyError> {
    if !cfg!(target_os = "windows") {
        return Err(ApplyError::Unsupported);
    }

    // Every rule in one elevated script, so it costs one prompt rather than
    // one per rule.
    let script = firewall_script(&program.to_string_lossy(), &dusk.to_string_lossy());
    let run = service::run_elevated(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &service::encode_powershell(&script),
        ],
        service::ELEVATED_TIMEOUT,
    )
    .await
    .map_err(|e| ApplyError::Failed(e.to_string()))?;

    if run.ok() {
        Ok(())
    } else {
        Err(ApplyError::Failed(
            "Could not add the firewall rules for hosting.".into(),
        ))
    }
}

/// One `netsh` rule per protocol for hosting and one for Dusk, stopping at
/// the first that fails.
fn firewall_script(program: &str, dusk: &str) -> String {
    // Dismissing the prompt Windows shows the first time Dusk listens leaves
    // block rules behind for it, and a block rule beats any allow rule. They
    // go first; finding none is not a failure, so its exit code is ignored.
    let mut script = format!(
        "& netsh.exe advfirewall firewall delete rule name=all {} | Out-Null\n",
        service::powershell_quote(&format!("program={dusk}")),
    );
    for (name, path, protocol) in [
        ("Dusk hosting (inbound TCP)", program, "TCP"),
        ("Dusk hosting (inbound UDP)", program, "UDP"),
        ("Dusk (inbound TCP)", dusk, "TCP"),
    ] {
        script.push_str(&format!(
            "& netsh.exe advfirewall firewall add rule {} dir=in action=allow {} protocol={protocol} enable=yes\n\
             if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}\n",
            service::powershell_quote(&format!("name={name}")),
            service::powershell_quote(&format!("program={path}")),
        ));
    }
    script.push_str("exit 0");
    script
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
    let dir = PathBuf::from(&home).join(".local/bin");
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

    register_user_unit(&PathBuf::from(&home), &target).await
}

/// Give the AppImage a systemd user unit, so it runs in the background from
/// sign-in and the host backend has a service to start and stop.
///
/// An AppImage registers nothing on its own; without this, Dusk installs
/// Sunshine and then reports it as not installed, because what the backend
/// looks for is the unit. A unit that already exists is left alone — it may
/// be the person's own, and overwriting it would discard what is in it.
async fn register_user_unit(home: &Path, program: &Path) -> Result<(), ApplyError> {
    let dir = home.join(".config/systemd/user");
    let unit = dir.join("sunshine.service");
    if !unit.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| ApplyError::Failed(e.to_string()))?;
        std::fs::write(&unit, user_unit(program)).map_err(|e| ApplyError::Failed(e.to_string()))?;
    }

    for args in [
        &["--user", "daemon-reload"][..],
        &["--user", "enable", "sunshine"][..],
    ] {
        check(
            service::run("systemctl", args)
                .await
                .map_err(|e| ApplyError::Failed(e.to_string()))?,
            "could not register hosting with systemd",
        )?;
    }
    Ok(())
}

/// Where udev reads rules from, packaged and local.
const UDEV_RULE_DIRS: [&str; 3] = [
    "/etc/udev/rules.d",
    "/usr/lib/udev/rules.d",
    "/lib/udev/rules.d",
];

/// The rule Sunshine's packages ship: hand `/dev/uinput`, which is how
/// Sunshine creates the remote machine's keyboard, mouse and controllers, to
/// whoever is signed in at the seat. An AppImage cannot install it.
const UINPUT_RULE: &str =
    "KERNEL==\"uinput\", SUBSYSTEM==\"misc\", OPTIONS+=\"static_node=uinput\", TAG+=\"uaccess\"\n\
                           KERNEL==\"uhid\", TAG+=\"uaccess\"\n";

/// Whether some rule already grants the seat `/dev/uinput`. Any file will
/// do, not only Sunshine's: distributions ship their own under other names,
/// Bazzite's among them.
pub fn linux_input_ready() -> bool {
    UDEV_RULE_DIRS.iter().any(|dir| {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .any(|text| grants_uinput(&text))
        })
    })
}

fn grants_uinput(rules: &str) -> bool {
    rules
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .any(|line| line.contains("KERNEL==\"uinput\"") && line.contains("uaccess"))
}

/// Install the uinput rule and apply it now. Writing under `/etc` needs
/// root, so this goes through polkit, which shows the desktop's own prompt.
pub async fn linux_allow_input() -> Result<(), ApplyError> {
    if !cfg!(target_os = "linux") {
        return Err(ApplyError::Unsupported);
    }
    let script = format!(
        "printf '%s' '{UINPUT_RULE}' > /etc/udev/rules.d/60-sunshine.rules && \
         udevadm control --reload-rules && \
         udevadm trigger --property-match=DEVNAME=/dev/uinput"
    );
    check(
        service::run_for("pkexec", &["sh", "-c", &script], service::ELEVATED_TIMEOUT)
            .await
            .map_err(|e| ApplyError::Failed(e.to_string()))?,
        "could not let hosting create input devices",
    )?;
    Ok(())
}

/// Modelled on the unit Sunshine's own packages ship: started with the
/// desktop session, because capture needs one, and restarted if it falls
/// over.
fn user_unit(program: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Sunshine, managed by Dusk\n\
         StartLimitIntervalSec=500\n\
         StartLimitBurst=5\n\
         After=xdg-desktop-autostart.target\n\
         \n\
         [Service]\n\
         ExecStart=\"{}\"\n\
         Restart=on-failure\n\
         RestartSec=5s\n\
         \n\
         [Install]\n\
         WantedBy=xdg-desktop-autostart.target\n",
        program.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_unit_runs_the_installed_appimage_and_starts_with_the_session() {
        let unit = user_unit(Path::new("/home/a b/.local/bin/sunshine"));
        assert!(
            unit.contains("ExecStart=\"/home/a b/.local/bin/sunshine\"\n"),
            "{unit}"
        );
        assert!(
            unit.contains("WantedBy=xdg-desktop-autostart.target"),
            "{unit}"
        );
        assert!(unit.contains("Restart=on-failure"), "{unit}");
    }

    #[test]
    fn a_uinput_rule_is_recognised_whoever_wrote_it() {
        assert!(grants_uinput(UINPUT_RULE));
        assert!(grants_uinput(
            "# ublue\nKERNEL==\"uinput\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"\n"
        ));
        assert!(!grants_uinput("# KERNEL==\"uinput\", TAG+=\"uaccess\"\n"));
        assert!(!grants_uinput("KERNEL==\"uinput\", MODE=\"0660\"\n"));
    }

    #[test]
    fn the_rule_survives_the_shell_it_is_written_through() {
        // Single-quoted in sh, so a single quote in it would end the string.
        assert!(!UINPUT_RULE.contains('\''));
    }

    #[test]
    fn the_launch_agent_uses_the_label_the_backend_looks_for() {
        let plist = launch_agent(Path::new(
            "/Applications/Sunshine.app/Contents/MacOS/sunshine",
        ));
        assert!(
            plist.contains("<string>dev.lizardbyte.sunshine</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/Applications/Sunshine.app/Contents/MacOS/sunshine</string>")
        );
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn every_firewall_rule_goes_in_one_script_and_a_failure_stops_it() {
        let script = firewall_script(
            r"C:\Program Files\Sunshine\sunshine.exe",
            r"C:\Program Files\Dusk\dusk.exe",
        );
        assert_eq!(script.matches("add rule").count(), 3, "{script}");
        assert!(
            script.contains(r"'program=C:\Program Files\Sunshine\sunshine.exe'"),
            "{script}"
        );
        assert!(
            script.contains("'name=Dusk hosting (inbound UDP)'"),
            "{script}"
        );
        assert_eq!(script.matches("$LASTEXITCODE").count(), 6, "{script}");
    }

    #[test]
    fn dusk_is_let_through_and_its_old_block_rules_go_first() {
        let script = firewall_script(
            r"C:\Program Files\Sunshine\sunshine.exe",
            r"C:\Program Files\Dusk\dusk.exe",
        );
        let delete = script
            .find(r"delete rule name=all 'program=C:\Program Files\Dusk\dusk.exe'")
            .expect(&script);
        let allow = script
            .find(r"'name=Dusk (inbound TCP)' dir=in action=allow 'program=C:\Program Files\Dusk\dusk.exe' protocol=TCP")
            .expect(&script);
        assert!(delete < allow, "{script}");
    }

    #[test]
    fn shell_quoting_survives_a_path_with_a_quote_in_it() {
        // The only character that can escape single-quoting is a single
        // quote, so it is the one that has to be handled.
        assert_eq!(
            shell_quote("/Volumes/Sun/Sunshine.app"),
            "'/Volumes/Sun/Sunshine.app'"
        );
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
