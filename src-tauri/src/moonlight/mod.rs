//! The Moonlight client side.
//!
//! M2 shells out to moonlight-qt. The streaming logic lives in
//! moonlight-common-c, so an embedded renderer can replace the subprocess
//! later without the UI changing — see `identity` for what that swap costs
//! and why it is already paid for.

use std::path::PathBuf;

pub mod cli;
pub mod hosts;
pub mod identity;

pub use cli::Moonlight;
pub use identity::ClientIdentity;

// ------------------------------------------------- the Linux settings store

/// moonlight-qt's QSettings file, relative to a config directory.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const INI_RELATIVE: &str = "Moonlight Game Streaming Project/Moonlight.conf";

/// The Flatpak application id, which is also the name of its sandbox.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const FLATPAK_ID: &str = "com.moonlight_stream.Moonlight";

/// Every place moonlight-qt's settings file may be on Linux, best first.
///
/// A Flatpak Moonlight — how most distributions ship it, and the only way to
/// install it on an immutable one such as Bazzite — writes its settings
/// *inside the sandbox*, at `~/.var/app/<id>/config`, not to `~/.config`.
/// Checking only the native path therefore finds nothing on a very ordinary
/// Linux machine, and the cost is not small: this one file holds both the
/// client identity and the remembered-host list, so missing it loses TLS
/// probing and an entire discovery source at the same time, silently.
///
/// The native path stays first so a distro package or a self-built client
/// wins over a Flatpak that may be installed beside it. Callers walk the
/// list and take the first store that answers, rather than the first that
/// exists, so an abandoned empty config does not mask a live one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ini_paths(
    home: Option<&std::path::Path>,
    config_home: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();

    match (config_home, home) {
        (Some(dir), _) => out.push(dir.join(INI_RELATIVE)),
        (None, Some(home)) => out.push(home.join(".config").join(INI_RELATIVE)),
        (None, None) => {}
    }

    if let Some(home) = home {
        out.push(
            home.join(".var/app")
                .join(FLATPAK_ID)
                .join("config")
                .join(INI_RELATIVE),
        );
    }

    out
}

/// Strip the quotes QSettings puts around an INI value.
///
/// It quotes a value whenever leaving it bare would be ambiguous, which for
/// a PEM blob means whenever base64 padding lands an `=` inside it. So one
/// key is quoted and the next is not *in the same file* — `certificate` is,
/// `key` usually is not. Left in place the quotes defeat the
/// `@ByteArray(...)` unwrap underneath, and the identity then reads as
/// absent on a machine that has a perfectly good one.
pub(crate) fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

/// [`ini_paths`] against the real environment.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_store_paths() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    ini_paths(home.as_deref(), config_home.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flatpak_store_is_looked_for_as_well_as_the_native_one() {
        let home = PathBuf::from("/home/someone");
        let found = ini_paths(Some(&home), None);

        assert_eq!(
            found,
            vec![
                home.join(".config/Moonlight Game Streaming Project/Moonlight.conf"),
                home.join(
                    ".var/app/com.moonlight_stream.Moonlight/config/Moonlight Game Streaming Project/Moonlight.conf"
                ),
            ]
        );
    }

    #[test]
    fn xdg_config_home_replaces_the_native_path_but_not_the_flatpak_one() {
        // The Flatpak sandbox is keyed off HOME, so redirecting the native
        // config directory must not move it.
        let home = PathBuf::from("/home/someone");
        let config = PathBuf::from("/elsewhere");
        let found = ini_paths(Some(&home), Some(&config));

        assert_eq!(found[0], config.join(INI_RELATIVE));
        assert_eq!(found.len(), 2);
        assert!(found[1].starts_with(home.join(".var/app")));
    }

    #[test]
    fn a_quoted_value_loses_its_quotes_and_a_bare_one_is_untouched() {
        assert_eq!(unquote("\"@ByteArray(x)\""), "@ByteArray(x)");
        assert_eq!(unquote("@ByteArray(x)"), "@ByteArray(x)");
    }

    #[test]
    fn a_lone_quote_is_not_mistaken_for_a_pair() {
        assert_eq!(unquote("\"unterminated"), "\"unterminated");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn no_home_is_an_empty_list_rather_than_a_relative_path() {
        assert!(ini_paths(None, None).is_empty());
    }
}
