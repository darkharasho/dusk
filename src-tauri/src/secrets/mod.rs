//! Where Dusk's own secrets live, and the one place that decides.
//!
//! Two things need keeping: the host's web-API password
//! ([`crate::sunshine::credentials`]) and the GitHub sign-in with this
//! machine's signing key ([`crate::account`]). Both were reaching for
//! `keyring::Entry` directly, which meant there was no single answer to
//! "where does this machine keep its secrets" — and no way to change that
//! answer for a machine that cannot use the OS keystore.
//!
//! # Why a second backend exists at all
//!
//! The OS keystore is right on every attended machine, and on Windows and
//! macOS it is right full stop. On Linux it is the Secret Service, which a
//! session has to *unlock*, normally with the password you typed to log in.
//! A machine that logs in automatically never types one, so its keyring
//! stays locked and Dusk can neither write nor read. The machine then forgets
//! its signing key on every boot, and silently stops being one your other
//! machines recognise — which is the whole point of signing in.
//!
//! # Why sealing, and not a file
//!
//! The obvious fix is a file with tight permissions. It is also the wrong
//! one, and the reason is worth stating because it is not obvious:
//!
//! | | stolen disk or backup | another process running as you |
//! |---|---|---|
//! | plain file, mode 600 | readable | readable |
//! | keystore, session unlocked | protected | readable |
//! | sealed to the TPM | protected | readable |
//!
//! A keyring protects a secret only while it is *locked*. In any logged-in
//! session it is unlocked, and anything running as you can read every entry
//! over D-Bus. So against a live attacker the keystore was never buying
//! anything a 600 file does not — but against a stolen disk it buys
//! everything, and that is the row a plain file loses.
//!
//! Sealing to the TPM keeps that row and costs no password: the blob only
//! decrypts on the machine that wrote it. So this is not a downgrade traded
//! for convenience; it is the same protection, obtained differently.
//!
//! Never silently: a machine uses this only when asked (`dusk setup
//! --store=tpm`), and if there is no TPM to seal to, that command fails
//! rather than quietly writing a secret in the clear.

#[cfg(target_os = "linux")]
pub mod unlock;

use std::path::PathBuf;

/// The keystore entry holding Sunshine's web-API password.
pub const HOST: &str = "sunshine-local-api";
/// The keystore entry holding the GitHub sign-in and this machine's key.
pub const ACCOUNT: &str = "github-account";

const SERVICE: &str = "dev.dusk.app";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// The OS keystore: Keychain, Credential Manager, Secret Service.
    Keystore,
    /// Blobs under this directory, each decryptable only by this machine.
    Sealed(PathBuf),
}

impl Backend {
    /// One line for `dusk status`, so which store is in use is never a guess.
    pub fn describe(&self) -> String {
        match self {
            Backend::Keystore => "the system keystore".to_string(),
            Backend::Sealed(dir) => format!("sealed to this machine's TPM ({})", dir.display()),
        }
    }
}

/// Which store this machine uses.
///
/// Decided by the sealed directory existing rather than by a setting. A
/// setting would have to be read identically by the window and by `dusk
/// setup`, from two different config paths, and the day they disagreed one
/// of them would write a secret somewhere the other never looks.
pub fn backend() -> Backend {
    match sealed_dir() {
        Some(dir) if dir.is_dir() => Backend::Sealed(dir),
        _ => Backend::Keystore,
    }
}

/// `$XDG_DATA_HOME/dusk/sealed`, or `~/.local/share/dusk/sealed`.
///
/// Not Tauri's app-data directory: `dusk setup` runs before Tauri exists and
/// cannot ask it where that is.
fn sealed_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(base.join("dusk/sealed"))
}

pub fn get(name: &str) -> Result<Option<String>, String> {
    match backend() {
        Backend::Keystore => from_keystore(name),
        Backend::Sealed(dir) => unseal(&dir, name),
    }
}

pub fn set(name: &str, value: &str) -> Result<(), String> {
    match backend() {
        Backend::Keystore => to_keystore(name, value),
        Backend::Sealed(dir) => seal(&dir, name, value),
    }
}

pub fn delete(name: &str) -> Result<(), String> {
    match backend() {
        Backend::Keystore => {
            let entry = keyring::Entry::new(SERVICE, name).map_err(|e| e.to_string())?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(err.to_string()),
            }
        }
        Backend::Sealed(dir) => match std::fs::remove_file(blob(&dir, name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    }
}

// ----------------------------------------------------------------- keystore

fn from_keystore(name: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(SERVICE, name).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(raw) => Ok(Some(raw)),
        // The one error that is a real answer: nothing has been stored.
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

fn to_keystore(name: &str, value: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, name).map_err(|e| e.to_string())?;
    entry.set_password(value).map_err(|e| e.to_string())
}

// -------------------------------------------------------------------- tpm

fn blob(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.cred"))
}

/// Whether this machine can seal at all, as systemd itself reports it.
///
/// Also catches the case that bit during development: a machine with a
/// perfectly good TPM whose user cannot open `/dev/tpmrm0`, because sealing
/// needs membership of the `tss` group.
#[cfg(target_os = "linux")]
pub fn can_seal() -> Result<(), String> {
    let out = std::process::Command::new("systemd-creds")
        .arg("has-tpm2")
        .output()
        .map_err(|e| format!("systemd-creds is not available on this machine ({e})"))?;
    if !out.status.success() {
        return Err("this machine reports no TPM 2.0 to seal secrets to".to_string());
    }
    // Reporting a TPM is not the same as being allowed to use it.
    let probe = std::process::Command::new("systemd-creds")
        .args(["encrypt", "--with-key=tpm2", "--name=dusk-probe", "-", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().map(|mut s| s.write_all(b"probe"));
            child.wait_with_output()
        })
        .map_err(|e| e.to_string())?;
    if probe.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&probe.stderr);
    if why.contains("Permission denied") {
        return Err(
            "this machine has a TPM but your user cannot reach it. Add yourself to the `tss` \
             group (sudo usermod -aG tss $USER) and log in again."
                .to_string(),
        );
    }
    Err(format!("this machine cannot seal secrets: {}", why.trim()))
}

#[cfg(not(target_os = "linux"))]
pub fn can_seal() -> Result<(), String> {
    Err(
        "Sealing to a TPM is a Linux answer to a Linux problem; this platform's keystore \
         needs no unlocking."
            .to_string(),
    )
}

/// Start using the sealed store, moving anything already kept into it.
///
/// Refuses before it creates anything if the machine cannot seal, so a
/// failure here leaves the keystore exactly as it was.
pub fn enable_sealed() -> Result<PathBuf, String> {
    can_seal()?;
    let dir = sealed_dir().ok_or_else(|| "Dusk could not find your home directory".to_string())?;

    // Carried over before the directory exists, because creating it is what
    // switches `backend()` over: read through the old store, write through
    // the new one.
    let carried: Vec<(&str, Option<String>)> = [HOST, ACCOUNT]
        .iter()
        .map(|name| (*name, from_keystore(name).unwrap_or(None)))
        .collect();

    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    restrict(&dir, 0o700)?;

    for (name, value) in carried {
        if let Some(value) = value {
            seal(&dir, name, &value)?;
        }
    }
    Ok(dir)
}

fn seal(dir: &std::path::Path, name: &str, value: &str) -> Result<(), String> {
    use std::io::Write;
    let path = blob(dir, name);
    let tmp = path.with_extension("cred.partial");

    // The secret goes in on stdin, never as an argument: a command line is
    // readable by anything that can run `ps`.
    let mut child = std::process::Command::new("systemd-creds")
        .args(["encrypt", "--with-key=tpm2", &format!("--name={name}"), "-"])
        .arg(&tmp)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run systemd-creds ({e})"))?;
    child
        .stdin
        .take()
        .ok_or("systemd-creds would not take the secret")?
        .write_all(value.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "could not seal the secret: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    restrict(&tmp, 0o600)?;
    // Renamed into place only once it is whole, so an interrupted write
    // leaves the previous secret readable rather than a truncated one.
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn unseal(dir: &std::path::Path, name: &str) -> Result<Option<String>, String> {
    let path = blob(dir, name);
    if !path.is_file() {
        return Ok(None);
    }
    let out = std::process::Command::new("systemd-creds")
        .args(["decrypt", &format!("--name={name}")])
        .arg(&path)
        .arg("-")
        .output()
        .map_err(|e| format!("could not run systemd-creds ({e})"))?;
    if !out.status.success() {
        return Err(format!(
            "could not unseal the secret: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

#[cfg(unix)]
fn restrict(path: &std::path::Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_with_no_sealed_directory_uses_the_keystore() {
        // The default must be the OS keystore on every platform: sealing is
        // something a person turns on, never something Dusk decides.
        assert_eq!(
            match sealed_dir() {
                Some(dir) if dir.is_dir() => Backend::Sealed(dir),
                _ => Backend::Keystore,
            },
            backend()
        );
    }

    // Windows has no $HOME and nothing to seal to; there it is the keystore.
    #[cfg(not(windows))]
    #[test]
    fn the_sealed_directory_sits_under_the_data_home() {
        let dir = sealed_dir().expect("a home directory");
        assert!(dir.ends_with("dusk/sealed"), "{}", dir.display());
    }

    #[test]
    fn a_blob_is_named_for_its_entry() {
        let dir = PathBuf::from("/tmp/x");
        assert_eq!(
            blob(&dir, HOST),
            PathBuf::from("/tmp/x/sunshine-local-api.cred")
        );
        assert_ne!(blob(&dir, HOST), blob(&dir, ACCOUNT));
    }

    #[test]
    fn each_backend_says_which_it_is() {
        assert!(Backend::Keystore.describe().contains("keystore"));
        assert!(Backend::Sealed(PathBuf::from("/tmp/x"))
            .describe()
            .contains("TPM"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn sealing_is_refused_where_it_is_not_the_answer() {
        assert!(can_seal().is_err());
    }
}
