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

/// Is this store the one inside the Flatpak sandbox?
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_flatpak_store(path: &std::path::Path) -> bool {
    path.components()
        .any(|part| part.as_os_str() == FLATPAK_ID)
}

/// Is this client binary the Flatpak one?
///
/// The exported wrapper is named for the application id, and everything
/// `flatpak` exports lives under a directory of that name, so either is
/// enough. A `DUSK_MOONLIGHT_BIN` pointing at a hand-written launcher that
/// calls `flatpak run` is the one case this cannot see; it reads as native
/// and the native store is then the wrong guess.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_flatpak_client(binary: &std::path::Path) -> bool {
    binary.file_name().is_some_and(|name| name == FLATPAK_ID)
        || binary.components().any(|part| part.as_os_str() == "flatpak")
}

/// The stores that belong to `client`, best first.
///
/// A Flatpak Moonlight and a native one keep *separate* identities, and the
/// host binds a pairing to the client certificate. Reading one client's
/// store while launching the other therefore does not merely show a stale
/// list: Dusk reports a machine as paired, offers to stream, and the client
/// it actually runs is a stranger the host refuses. Narrowing to the store
/// the chosen client writes keeps the grid honest about the client it has.
///
/// With no client there is nothing to match, so every store is fair game —
/// a grid built from whichever store answers beats an empty one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stores_for(
    client: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
    config_home: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let all = ini_paths(home, config_home);
    let Some(client) = client else {
        return all;
    };

    let sandboxed = is_flatpak_client(client);
    all.into_iter()
        .filter(|path| is_flatpak_store(path) == sandboxed)
        .collect()
}

/// [`stores_for`] against the real environment.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_stores_for(client: Option<&Moonlight>) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    stores_for(
        client.map(|client| client.binary()),
        home.as_deref(),
        config_home.as_deref(),
    )
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

    /// The bug this guards: Dusk read the Flatpak's paired host list while
    /// launching the native client, so the grid offered a Stream button for
    /// a machine that client had never paired with, and moonlight-qt refused
    /// it with "has not been paired".
    #[test]
    fn a_native_client_reads_only_the_native_store() {
        let home = PathBuf::from("/home/someone");
        let client = PathBuf::from("/usr/bin/moonlight");

        let found = stores_for(Some(&client), Some(&home), None);

        assert_eq!(found, vec![home.join(".config").join(INI_RELATIVE)]);
    }

    #[test]
    fn a_flatpak_client_reads_only_the_sandboxed_store() {
        let home = PathBuf::from("/home/someone");
        let client =
            PathBuf::from("/var/lib/flatpak/exports/bin/com.moonlight_stream.Moonlight");

        let found = stores_for(Some(&client), Some(&home), None);

        assert_eq!(found.len(), 1);
        assert!(is_flatpak_store(&found[0]), "{found:?}");
    }

    /// Dusk's own fork lives beside the app, nowhere near a Flatpak, and a
    /// `DUSK_MOONLIGHT_BIN` pointing at a build tree is the same shape.
    #[test]
    fn dusks_forked_client_counts_as_native() {
        let client = PathBuf::from("/home/someone/GitHub/moonlight-qt/app/moonlight");
        assert!(!is_flatpak_client(&client));
    }

    /// With no client installed there is nothing to match against, and a
    /// grid built from whichever store answers beats an empty one.
    #[test]
    fn no_client_leaves_every_store_in_play() {
        let home = PathBuf::from("/home/someone");
        assert_eq!(
            stores_for(None, Some(&home), None),
            ini_paths(Some(&home), None)
        );
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
