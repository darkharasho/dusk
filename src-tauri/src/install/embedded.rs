//! The hosting helper a macOS Dusk carries inside itself.
//!
//! On Linux and Windows a release ships Sunshine's installer and runs it. On
//! macOS it ships Sunshine itself, merged universal and renamed Dusk Host, at
//! `Dusk.app/Contents/Helpers/Dusk Host.app`, signed and notarized with Dusk
//! (`scripts/stage-host-macos.sh`). Installing hosting is then only writing a
//! launch agent that points at it: no download, no copy into /Applications,
//! no administrator prompt.
//!
//! The agent names a path inside Dusk, so it is only written from a Dusk
//! that is somewhere it will stay. A copy run straight from the disk image,
//! or one macOS has translocated, lives at a path that is gone the next time
//! anyone looks.

use std::path::{Path, PathBuf};

/// The helper's directory name inside `Contents/Helpers`.
pub const HELPER: &str = "Dusk Host.app";

/// The helper inside the running Dusk, if this build carries one.
pub fn find() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    helper_beside(&exe).filter(|p| p.is_dir())
}

/// `<x>.app/Contents/MacOS/<exe>` → `<x>.app/Contents/Helpers/Dusk Host.app`.
fn helper_beside(exe: &Path) -> Option<PathBuf> {
    let contents = exe.parent()?.parent()?;
    Some(contents.join("Helpers").join(HELPER))
}

/// Whether `program` is the helper of some Dusk — this one or a copy that has
/// since moved. A launch agent pointing at one is Dusk's to rewrite; any
/// other agent was written by someone else and is left alone.
pub fn is_dusk_helper(program: &Path) -> bool {
    program
        .ancestors()
        .any(|p| p.file_name().is_some_and(|n| n == HELPER))
        && program.to_string_lossy().contains("/Contents/Helpers/")
}

/// Why a launch agent must not point into this Dusk right now, if it must
/// not.
pub fn unsettled(exe: &Path) -> Option<&'static str> {
    let s = exe.to_string_lossy();
    if s.contains("/AppTranslocation/") || s.starts_with("/Volumes/") {
        Some("Move Dusk into your Applications folder and open it from there, then turn hosting on. Hosting runs from inside Dusk, so it needs Dusk to stay put.")
    } else {
        None
    }
}

/// The `.app` a program inside a bundle belongs to.
pub fn bundle_of(program: &Path) -> Option<PathBuf> {
    program
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// The executable inside a Sunshine bundle. Read from Info.plist, because the
/// helper keeps upstream's executable name under a new bundle name, and a
/// guessed case is wrong on a case-sensitive volume.
pub fn program(bundle: &Path) -> PathBuf {
    let name = info_string(bundle, "CFBundleExecutable").unwrap_or_else(|| "sunshine".into());
    bundle.join("Contents/MacOS").join(name)
}

/// The bundle's own version, for showing before anyone signs in.
pub fn version(bundle: &Path) -> Option<String> {
    info_string(bundle, "CFBundleShortVersionString")
}

#[cfg(target_os = "macos")]
fn info_string(bundle: &Path, key: &str) -> Option<String> {
    let value = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    value
        .as_dictionary()?
        .get(key)?
        .as_string()
        .map(str::to_string)
}

#[cfg(not(target_os = "macos"))]
fn info_string(_bundle: &Path, _key: &str) -> Option<String> {
    None
}

/// The program a launch agent plist runs: its first `ProgramArguments`.
#[cfg(target_os = "macos")]
pub fn agent_program(plist_path: &Path) -> Option<PathBuf> {
    let value = plist::Value::from_file(plist_path).ok()?;
    let dict = value.as_dictionary()?;
    dict.get("ProgramArguments")
        .and_then(|a| a.as_array()?.first()?.as_string())
        .or_else(|| dict.get("Program")?.as_string())
        .map(PathBuf::from)
}

#[cfg(not(target_os = "macos"))]
pub fn agent_program(_plist_path: &Path) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_sits_in_helpers_beside_the_executable() {
        assert_eq!(
            helper_beside(Path::new("/Applications/Dusk.app/Contents/MacOS/dusk")).unwrap(),
            PathBuf::from("/Applications/Dusk.app/Contents/Helpers/Dusk Host.app")
        );
    }

    #[test]
    fn only_an_agent_into_a_dusk_helper_is_dusks_to_rewrite() {
        assert!(is_dusk_helper(Path::new(
            "/Applications/Dusk.app/Contents/Helpers/Dusk Host.app/Contents/MacOS/Sunshine"
        )));
        assert!(is_dusk_helper(Path::new(
            "/Users/me/Downloads/Dusk.app/Contents/Helpers/Dusk Host.app/Contents/MacOS/Sunshine"
        )));
        assert!(!is_dusk_helper(Path::new(
            "/Applications/Sunshine.app/Contents/MacOS/sunshine"
        )));
        assert!(!is_dusk_helper(Path::new("/opt/homebrew/bin/sunshine")));
    }

    #[test]
    fn a_disk_image_or_translocated_copy_is_not_somewhere_to_point_an_agent() {
        assert!(unsettled(Path::new("/Volumes/Dusk/Dusk.app/Contents/MacOS/dusk")).is_some());
        assert!(unsettled(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/Dusk.app/Contents/MacOS/dusk"
        ))
        .is_some());
        assert!(unsettled(Path::new("/Applications/Dusk.app/Contents/MacOS/dusk")).is_none());
        assert!(unsettled(Path::new(
            "/Users/me/Applications/Dusk.app/Contents/MacOS/dusk"
        ))
        .is_none());
    }

    #[test]
    fn a_program_belongs_to_its_nearest_bundle() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Dusk.app/Contents/Helpers/Dusk Host.app/Contents/MacOS/Sunshine"
            ))
            .unwrap(),
            PathBuf::from("/Applications/Dusk.app/Contents/Helpers/Dusk Host.app")
        );
        assert!(bundle_of(Path::new("/opt/homebrew/bin/sunshine")).is_none());
    }
}
