//! Sunshine's state file (`sunshine_state.json`): where it keeps the list of
//! clients it has paired. Dusk writes a client's certificate straight into it
//! so your own machines pair without a PIN.
//!
//! This module is pure file handling. It only ever adds or removes
//! `named_devices` entries whose `uuid` Dusk was told about; every other
//! entry and every field it does not understand is left as it was found.

// Nothing calls this yet; the pairing tasks that follow wire it in.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// A client Sunshine should trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedDevice {
    pub name: String,
    pub cert: String,
    pub uuid: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StateFileError {
    #[error("Sunshine's state file does not exist")]
    Missing,
    #[error("could not read Sunshine's state file: {0}")]
    Unreadable(String),
    #[error("could not write Sunshine's state file: {0}")]
    Unwritable(String),
    #[error("Sunshine's state file has a shape Dusk does not recognise")]
    UnknownShape,
}

/// Sunshine's configuration directory.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramFiles")?;
        Some(PathBuf::from(base).join("Sunshine").join("config"))
    }
    #[cfg(not(windows))]
    {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty());
        let base = match xdg {
            Some(x) => PathBuf::from(x),
            None => {
                PathBuf::from(std::env::var_os("HOME").filter(|v| !v.is_empty())?).join(".config")
            }
        };
        Some(base.join("sunshine"))
    }
}

/// A path named in Sunshine's config: absolute as given, relative to the
/// config directory otherwise. An unset key (or no config at all) means the
/// default.
fn configured_path(
    config: Option<&Map<String, Value>>,
    key: &str,
    default: &str,
) -> Option<PathBuf> {
    let dir = config_dir()?;
    let set = config
        .and_then(|c| c.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    Some(dir.join(set.unwrap_or(default)))
}

pub fn state_path(config: Option<&Map<String, Value>>) -> Option<PathBuf> {
    configured_path(config, "file_state", "sunshine_state.json")
}

pub fn server_cert_path(config: Option<&Map<String, Value>>) -> Option<PathBuf> {
    configured_path(config, "cert", "credentials/cacert.pem")
}

/// A PEM reduced to a form that compares equal across line endings and hashes
/// the same everywhere: no `\r`, no trailing whitespace on any line, no
/// leading or trailing whitespace overall.
pub fn normalise_pem(pem: &str) -> String {
    pem.replace('\r', "")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn devices(state: &Value) -> Result<Option<&Vec<Value>>, StateFileError> {
    let root = state
        .get("root")
        .and_then(Value::as_object)
        .ok_or(StateFileError::UnknownShape)?;
    match root.get("named_devices") {
        None => Ok(None),
        Some(Value::Array(list)) => Ok(Some(list)),
        Some(_) => Err(StateFileError::UnknownShape),
    }
}

fn str_field<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

/// Apply Dusk's additions and removals to a parsed state file. `Ok(None)`
/// means nothing would change, so there is nothing to write.
pub fn merge(
    state: &Value,
    add: &[NamedDevice],
    remove_uuids: &[String],
) -> Result<Option<Value>, StateFileError> {
    let existing = devices(state)?;
    let mut list: Vec<Value> = existing.cloned().unwrap_or_default();
    let mut changed = false;

    let before = list.len();
    list.retain(|e| !str_field(e, "uuid").is_some_and(|u| remove_uuids.iter().any(|r| r == u)));
    changed |= list.len() != before;

    for device in add {
        let cert = normalise_pem(&device.cert);
        let present = list.iter().any(|e| {
            str_field(e, "uuid") == Some(device.uuid.as_str())
                || str_field(e, "cert").is_some_and(|c| normalise_pem(c) == cert)
        });
        if !present {
            list.push(json!({
                "name": device.name,
                "cert": device.cert,
                "uuid": device.uuid,
                "enabled": "true",
            }));
            changed = true;
        }
    }

    if !changed {
        return Ok(None);
    }
    let mut merged = state.clone();
    merged["root"]["named_devices"] = Value::Array(list);
    Ok(Some(merged))
}

pub fn read(path: &Path) -> Result<Value, StateFileError> {
    let text = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => StateFileError::Missing,
        _ => StateFileError::Unreadable(e.to_string()),
    })?;
    serde_json::from_str(&text).map_err(|e| StateFileError::Unreadable(e.to_string()))
}

/// Write the whole file to a sibling temp file, then rename it into place, so
/// Sunshine never reads half a file.
pub fn write_atomic(path: &Path, state: &Value) -> Result<(), StateFileError> {
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| StateFileError::Unwritable(e.to_string()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".dusk-tmp");
    let tmp = PathBuf::from(tmp);
    let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(StateFileError::Unwritable(e.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
  "username": "sunny",
  "salt": "abc",
  "password": "hash",
  "root": {
    "future": 1,
    "uniqueid": "HOST-ID",
    "named_devices": [
      {"name": "Phone", "cert": "-----BEGIN CERTIFICATE-----\nFOREIGN\n-----END CERTIFICATE-----\n", "uuid": "U1", "enabled": "true"}
    ]
  }
}"#;

    fn state() -> Value {
        serde_json::from_str(FIXTURE).unwrap()
    }

    fn dev(uuid: &str, body: &str) -> NamedDevice {
        NamedDevice {
            name: format!("dev-{uuid}"),
            cert: format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n"),
            uuid: uuid.to_string(),
        }
    }

    fn named(v: &Value) -> &Vec<Value> {
        v["root"]["named_devices"].as_array().unwrap()
    }

    #[test]
    fn a_device_is_added_with_enabled_as_the_string_true() {
        let d = dev("D1", "MINE");
        let out = merge(&state(), std::slice::from_ref(&d), &[])
            .unwrap()
            .unwrap();
        assert_eq!(
            named(&out).last().unwrap(),
            &json!({"name": d.name, "cert": d.cert, "uuid": d.uuid, "enabled": "true"})
        );
    }

    #[test]
    fn everything_dusk_does_not_own_is_kept() {
        let before = state();
        let out = merge(&before, &[dev("D1", "MINE")], &[]).unwrap().unwrap();
        for key in ["username", "salt", "password"] {
            assert_eq!(out[key], before[key]);
        }
        assert_eq!(out["root"]["uniqueid"], before["root"]["uniqueid"]);
        assert_eq!(out["root"]["future"], json!(1));
        assert_eq!(named(&out)[0], named(&before)[0]);
    }

    #[test]
    fn adding_what_is_already_there_changes_nothing() {
        let d = dev("D1", "MINE");
        let with = merge(&state(), std::slice::from_ref(&d), &[])
            .unwrap()
            .unwrap();
        assert!(merge(&with, &[d], &[]).unwrap().is_none());
    }

    #[test]
    fn a_certificate_already_paired_is_not_added_twice() {
        let s = state();
        let cert = "-----BEGIN CERTIFICATE-----\r\nFOREIGN\r\n-----END CERTIFICATE-----\r\n";
        let d = NamedDevice {
            name: "x".into(),
            cert: cert.into(),
            uuid: "U2".into(),
        };
        assert!(merge(&s, std::slice::from_ref(&d), &[]).unwrap().is_none());
        assert!(merge(&s, &[], &["U2".to_string()]).unwrap().is_none());
    }

    #[test]
    fn only_listed_uuids_are_removed() {
        let d = dev("D1", "MINE");
        let with = merge(&state(), std::slice::from_ref(&d), &[])
            .unwrap()
            .unwrap();
        let out = merge(&with, &[], &[d.uuid]).unwrap().unwrap();
        assert_eq!(named(&out).len(), 1);
        assert_eq!(named(&out)[0]["uuid"], "U1");
    }

    #[test]
    fn a_state_with_no_devices_yet_gets_the_list() {
        let mut s = state();
        s["root"].as_object_mut().unwrap().remove("named_devices");
        let d = dev("D1", "MINE");
        let out = merge(&s, std::slice::from_ref(&d), &[]).unwrap().unwrap();
        assert_eq!(named(&out).len(), 1);
        assert_eq!(named(&out)[0]["uuid"], "D1");
    }

    #[test]
    fn an_unrecognised_shape_is_refused_not_rewritten() {
        let mut s = state();
        s["root"]["named_devices"] = json!({});
        assert!(matches!(
            merge(&s, &[dev("D1", "M")], &[]),
            Err(StateFileError::UnknownShape)
        ));
        let no_root = json!({"username": "x"});
        assert!(matches!(
            merge(&no_root, &[dev("D1", "M")], &[]),
            Err(StateFileError::UnknownShape)
        ));
    }

    #[test]
    fn file_state_moves_the_file_relative_to_the_config_dir() {
        let Some(dir) = config_dir() else { return };
        let cfg = json!({"file_state": "other.json"});
        let p = state_path(cfg.as_object()).unwrap();
        assert_eq!(p, dir.join("other.json"));
        assert!(p.ends_with("sunshine/other.json") || p.ends_with("Sunshine/config/other.json"));

        let abs = std::env::temp_dir().join("abs-state.json");
        let cfg = json!({"file_state": abs.to_str().unwrap()});
        assert_eq!(state_path(cfg.as_object()).unwrap(), abs);

        assert_eq!(state_path(None).unwrap(), dir.join("sunshine_state.json"));
        assert_eq!(
            server_cert_path(None).unwrap(),
            dir.join("credentials/cacert.pem")
        );
    }

    #[test]
    fn a_write_lands_whole_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("dusk-state-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sunshine_state.json");
        write_atomic(&path, &state()).unwrap();
        assert_eq!(read(&path).unwrap(), state());
        assert!(!dir.join("sunshine_state.json.dusk-tmp").exists());
        assert!(matches!(
            read(&dir.join("nope.json")),
            Err(StateFileError::Missing)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pem_normalising_ignores_line_endings_and_trailing_space() {
        assert_eq!(normalise_pem("A \r\nB\r\n\r\n"), "A\nB");
    }
}
