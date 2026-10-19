//! The Moonlight client identity.
//!
//! # Why Dusk adopts rather than imposes
//!
//! The plan going into M2 was for Dusk to generate its own keypair and point
//! moonlight-qt at it through a private profile. The spike killed that:
//! moonlight-qt has no `--config` flag, and its QSettings backend is
//! CFPreferences on macOS and the registry on Windows. Neither is
//! redirectable by environment variable, so only Linux could have worked.
//!
//! What the spike also found is that the identity is stored as plain PEM
//! bytes under two keys, `certificate` and `key`. So Dusk reads them instead.
//! It never writes to moonlight-qt's store — pairing is what mints an
//! identity, and pairing is moonlight-qt's job in M2.
//!
//! The consequence, stated plainly: moonlight-qt owns the identity for now
//! and Dusk holds a copy. If someone resets moonlight-qt, Dusk loses pairing
//! state with it. Ownership flips at the moonlight-common-c swap, and because
//! Dusk already holds this keypair, that swap costs nobody a re-pair — which
//! was the whole point of settling this early.

/// A PEM certificate and its private key, as read from moonlight-qt.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

impl std::fmt::Debug for ClientIdentity {
    /// Hand-written so a stray `{:?}` cannot put a private key in a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field("cert_pem", &format_args!("{} bytes", self.cert_pem.len()))
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

impl ClientIdentity {
    fn from_parts(cert: Vec<u8>, key: Vec<u8>) -> Option<Self> {
        // A half-written store is worse than no store: it would produce a TLS
        // client that fails every handshake rather than falling back to the
        // plain-HTTP probe.
        if !starts_with_pem(&cert, b"-----BEGIN CERTIFICATE-----") {
            return None;
        }
        if !looks_like_private_key(&key) {
            return None;
        }
        Some(Self {
            cert_pem: cert,
            key_pem: key,
        })
    }
}

fn starts_with_pem(bytes: &[u8], header: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    bytes[start..].starts_with(header)
}

/// Moonlight has written both `PRIVATE KEY` and `RSA PRIVATE KEY` over its
/// lifetime depending on the OpenSSL it was built against.
fn looks_like_private_key(bytes: &[u8]) -> bool {
    starts_with_pem(bytes, b"-----BEGIN PRIVATE KEY-----")
        || starts_with_pem(bytes, b"-----BEGIN RSA PRIVATE KEY-----")
        || starts_with_pem(bytes, b"-----BEGIN EC PRIVATE KEY-----")
}

/// Read the identity `client` is using, if it has one yet.
///
/// Returns `None` rather than an error when that client has never paired:
/// that is an ordinary first-run state, not a fault.
///
/// It must be *that* client's identity. Dusk probes hosts with this
/// certificate to decide whether they are paired, and a host answers for
/// the certificate it was paired with — so borrowing a second Moonlight's
/// identity makes the grid describe a client Dusk is not going to run.
pub fn load(client: Option<&crate::moonlight::Moonlight>) -> Option<ClientIdentity> {
    platform::load(client)
}

// ------------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::path::PathBuf;

    fn store_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Preferences/com.moonlight-stream.Moonlight.plist"))
    }

    /// One store per user here, so which client was chosen does not change
    /// the answer.
    pub fn load(_client: Option<&crate::moonlight::Moonlight>) -> Option<ClientIdentity> {
        let value = plist::Value::from_file(store_path()?).ok()?;
        let dict = value.as_dictionary()?;
        let read = |k: &str| dict.get(k)?.as_data().map(<[u8]>::to_vec);
        ClientIdentity::from_parts(read("certificate")?, read("key")?)
    }
}

// ------------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    /// Only the chosen client's own store: a Flatpak Moonlight and a native
    /// one mint separate identities, and the host binds a pairing to the
    /// certificate. Reading the other one's would have Dusk probe as a
    /// client it never runs.
    ///
    /// Among that client's stores the first holding a usable identity wins —
    /// not the first that exists, so a config without one does not hide a
    /// config that has it.
    pub fn load(client: Option<&crate::moonlight::Moonlight>) -> Option<ClientIdentity> {
        crate::moonlight::linux_stores_for(client)
            .into_iter()
            .find_map(|path| {
                let text = std::fs::read_to_string(path).ok()?;
                ClientIdentity::from_parts(
                    super::ini_value(&text, "certificate")?,
                    super::ini_value(&text, "key")?,
                )
            })
    }
}

// ----------------------------------------------------------------- Windows

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const SUBKEY: &str = r"Software\Moonlight Game Streaming Project\Moonlight";

    /// One store per user here, so which client was chosen does not change
    /// the answer.
    pub fn load(_client: Option<&crate::moonlight::Moonlight>) -> Option<ClientIdentity> {
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(SUBKEY).ok()?;
        // QSettings writes a QByteArray to the registry as REG_BINARY.
        let read = |name: &str| -> Option<Vec<u8>> {
            let value = key.get_raw_value(name).ok()?;
            Some(value.bytes)
        };
        ClientIdentity::from_parts(read("certificate")?, read("key")?)
    }
}

/// Pull one `key=value` out of a QSettings INI file.
///
/// QSettings writes a `QByteArray` as `@ByteArray(...)` with backslash
/// escapes, so the payload has to be unwrapped and unescaped before it is
/// PEM again.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ini_value(text: &str, key: &str) -> Option<Vec<u8>> {
    let line = text.lines().map(str::trim).find(|line| {
        line.split_once('=')
            .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
    })?;
    let raw = crate::moonlight::unquote(line.split_once('=')?.1.trim());

    let payload = raw
        .strip_prefix("@ByteArray(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(raw);

    Some(unescape(payload))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            // Anything else was not an escape; keep both characters.
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &[u8] = b"-----BEGIN CERTIFICATE-----\nMII\n-----END CERTIFICATE-----\n";
    const KEY: &[u8] = b"-----BEGIN PRIVATE KEY-----\nMII\n-----END PRIVATE KEY-----\n";

    #[test]
    fn a_complete_pair_is_accepted() {
        assert!(ClientIdentity::from_parts(CERT.to_vec(), KEY.to_vec()).is_some());
    }

    #[test]
    fn a_half_written_store_is_rejected_rather_than_half_used() {
        // Better no TLS client than one that fails every handshake.
        assert!(ClientIdentity::from_parts(CERT.to_vec(), b"garbage".to_vec()).is_none());
        assert!(ClientIdentity::from_parts(b"".to_vec(), KEY.to_vec()).is_none());
    }

    #[test]
    fn a_quoted_bytearray_still_reads_as_pem() {
        // What moonlight-qt writes on Linux: the certificate's base64
        // padding forces QSettings to quote it, the key's does not.
        let ini = concat!(
            "[General]\n",
            r#"certificate="@ByteArray(-----BEGIN CERTIFICATE-----\nMII==\n-----END CERTIFICATE-----\n)""#,
            "\n",
            r"key=@ByteArray(-----BEGIN PRIVATE KEY-----\nMII\n-----END PRIVATE KEY-----\n)",
            "\n",
        );

        let identity = ClientIdentity::from_parts(
            ini_value(ini, "certificate").expect("certificate"),
            ini_value(ini, "key").expect("key"),
        );
        assert!(identity.is_some(), "a quoted value must still parse as PEM");
    }

    #[test]
    fn older_rsa_and_ec_key_headers_are_accepted() {
        for header in [
            &b"-----BEGIN RSA PRIVATE KEY-----\nx\n"[..],
            &b"-----BEGIN EC PRIVATE KEY-----\nx\n"[..],
        ] {
            assert!(ClientIdentity::from_parts(CERT.to_vec(), header.to_vec()).is_some());
        }
    }

    #[test]
    fn the_debug_impl_never_prints_the_private_key() {
        let id = ClientIdentity::from_parts(CERT.to_vec(), KEY.to_vec()).unwrap();
        let rendered = format!("{id:?}");
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("PRIVATE KEY"));
    }

    #[test]
    fn qsettings_bytearrays_unescape_back_to_pem() {
        let ini = "[General]\ncertificate=@ByteArray(-----BEGIN CERTIFICATE-----\\nMII\\n)\n";
        let value = ini_value(ini, "certificate").expect("found");
        assert_eq!(value, b"-----BEGIN CERTIFICATE-----\nMII\n");
    }

    #[test]
    fn a_missing_ini_key_is_none_not_a_panic() {
        assert!(ini_value("[General]\nfps=60\n", "certificate").is_none());
    }
}
