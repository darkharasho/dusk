//! Unlocking the Secret Service on a machine nobody logged in to by hand.
//!
//! On Linux the keystore is a *locked* safe that the session opens with the
//! password you typed at the login screen. A machine set to log in
//! automatically never types one, so the safe stays shut: Dusk cannot write
//! its secrets and cannot read them back, and the machine quietly stops being
//! one your others recognise.
//!
//! This opens it for the rest of the boot, given the password, and changes
//! nothing else — in particular it does **not** remove the keyring's
//! password, which would leave it unencrypted on disk for good.
//!
//! # Why not `gnome-keyring-daemon --unlock`
//!
//! Because it does not work, and the way it fails is destructive. Measured on
//! a real autologin machine: the keyring stayed locked, and `--replace` tore
//! down the session's own daemon and took `GNOME_KEYRING_CONTROL` and
//! `SSH_AUTH_SOCK` with it — breaking the running desktop's keyring and ssh
//! agent. Nothing Dusk offers may do that to someone's session.
//!
//! So this talks to the daemon that is already there, over the interface
//! GNOME provides for exactly this and named with admirable honesty:
//! `org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface`. It is
//! GNOME-specific, which is the right scope — it is GNOME's autologin that
//! creates the problem.

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const SERVICE: &str = "org.freedesktop.secrets";
const PATH: &str = "/org/freedesktop/secrets";
const LOGIN: &str = "/org/freedesktop/secrets/collection/login";
const GUILT: &str = "org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface";

/// Unlock the login keyring with `password`, leaving its password in place.
///
/// The password arrives from the caller's stdin and is never an argument to
/// anything: a command line is readable by anything that can run `ps`.
pub fn login_keyring(password: &str) -> Result<(), String> {
    let conn = Connection::session()
        .map_err(|e| format!("no session bus to reach the keyring on ({e})"))?;

    // A "plain" session sends the password unencrypted over the bus. That is
    // a unix socket owned by this user, which is the same trust boundary the
    // secrets themselves live behind, so negotiating a shared key would buy
    // nothing here.
    let opened = conn
        .call_method(
            Some(SERVICE),
            PATH,
            Some("org.freedesktop.Secret.Service"),
            "OpenSession",
            &("plain", Value::new("")),
        )
        .map_err(|e| format!("the keyring would not open a session ({e})"))?;
    let (_output, session): (OwnedValue, OwnedObjectPath) = opened
        .body()
        .deserialize()
        .map_err(|e| format!("the keyring's answer was not understood ({e})"))?;

    let collection = ObjectPath::try_from(LOGIN).map_err(|e| e.to_string())?;
    // (oayays): the session it is encrypted for, the parameters that would
    // describe that encryption, the bytes, and what they are.
    let secret = (
        session.as_ref(),
        Vec::<u8>::new(),
        password.as_bytes().to_vec(),
        "text/plain",
    );

    conn.call_method(
        Some(SERVICE),
        PATH,
        Some(GUILT),
        "UnlockWithMasterPassword",
        &(collection, secret),
    )
    .map_err(|e| match e.to_string() {
        // What a wrong password comes back as. Worth naming, because the raw
        // D-Bus error reads like a fault in Dusk.
        s if s.contains("MatchFailed") || s.contains("denied") => {
            "that password did not unlock the keyring".to_string()
        }
        s => format!("the keyring refused to unlock ({s})"),
    })?;
    Ok(())
}

/// Whether the login keyring is locked right now.
///
/// `Ok(false)` on a machine with no Secret Service at all: there is no locked
/// keyring in the way, which is what the caller is asking about. Whether a
/// keystore exists to write to is a separate question, and `save` answers it.
pub fn login_keyring_locked() -> Result<bool, String> {
    let Ok(conn) = Connection::session() else {
        return Ok(false);
    };
    let reply = match conn.call_method(
        Some(SERVICE),
        LOGIN,
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.freedesktop.Secret.Collection", "Locked"),
    ) {
        Ok(reply) => reply,
        Err(_) => return Ok(false),
    };
    let locked: OwnedValue = reply
        .body()
        .deserialize()
        .map_err(|e| format!("the keyring's answer was not understood ({e})"))?;
    bool::try_from(locked).map_err(|e| e.to_string())
}
