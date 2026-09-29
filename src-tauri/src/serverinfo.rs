//! GameStream `serverinfo` client.
//!
//! This is the only host endpoint Dusk needs for the device grid, and it is
//! the stable one — Moonlight depends on it, so Sunshine cannot change it
//! freely. Over plain HTTP it answers unauthenticated with everything except
//! a meaningful `PairStatus`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::model::{Activity, PairingState, ServerDetails};

const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Sunshine's own status, carried in the `status_code` attribute of `<root>`
/// rather than in the HTTP status — an unauthorized client still gets
/// `HTTP 200` with `status_code="401"` in the body.
pub const STATUS_OK: u16 = 200;
pub const STATUS_UNAUTHORIZED: u16 = 401;

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub fields: HashMap<String, String>,
    pub rtt_ms: u32,
    /// True when the response came from the TLS port with our client
    /// certificate presented, which is the only case where `PairStatus`
    /// means anything.
    pub authenticated: bool,
    /// Sunshine's application-level status, not the HTTP one.
    pub status_code: u16,
}

impl ServerInfo {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(&key.to_ascii_lowercase()).map(String::as_str)
    }

    /// Sunshine's stable per-host identifier. Preferred as the device key so
    /// a machine seen on two addresses collapses into one card.
    pub fn unique_id(&self) -> Option<&str> {
        self.get("uniqueid").filter(|v| !v.is_empty())
    }

    pub fn hostname(&self) -> Option<&str> {
        self.get("hostname").filter(|v| !v.is_empty())
    }

    pub fn details(&self) -> ServerDetails {
        ServerDetails {
            hostname: self.hostname().map(str::to_string),
            mac: self.get("mac").map(str::to_string),
            app_version: self.get("appversion").map(str::to_string),
            local_ip: self.get("localip").map(str::to_string),
        }
    }

    pub fn pairing(&self) -> PairingState {
        if !self.authenticated {
            // Over plain HTTP `PairStatus` is always 0 regardless of the real
            // state. Reporting that as NotPaired would be a lie.
            return PairingState::Unknown;
        }
        // The host rejected our certificate outright. That is not a failed
        // probe — it is the clearest "not paired" answer available, and
        // better than anything the plain-HTTP probe can say.
        if self.status_code == STATUS_UNAUTHORIZED {
            return PairingState::NotPaired;
        }
        match self.get("pairstatus") {
            Some("1") => PairingState::Paired,
            Some(_) => PairingState::NotPaired,
            None => PairingState::Unknown,
        }
    }

    pub fn activity(&self) -> Activity {
        let busy = self
            .get("state")
            .map(|s| s.to_ascii_uppercase().ends_with("_BUSY"))
            .unwrap_or(false);
        let current = self.get("currentgame").unwrap_or("0");
        let has_game = !current.is_empty() && current != "0";

        if busy || has_game {
            Activity::Hosting {
                app_id: has_game.then(|| current.to_string()),
                app_name: None, // Resolved against the host's app list in M2.
            }
        } else {
            Activity::Idle
        }
    }
}

/// Query `serverinfo` over plain HTTP.
///
/// `address` may carry an explicit port, in which case it wins over `port`.
pub async fn query_http(
    client: &reqwest::Client,
    address: &str,
    port: u16,
) -> Result<ServerInfo, String> {
    let authority = with_port(address, port);
    query(client, &format!("http://{authority}/serverinfo?uniqueid=0&uuid=0"), false).await
}

/// Query `serverinfo` over TLS with our client certificate presented.
///
/// This is the only way `PairStatus` means anything: the host answers it
/// against the certificate it is talking to, so the plain-HTTP probe can
/// never tell paired from unpaired.
///
/// `client` must be one built by [`crate::state::tls_client`] — an ordinary
/// client has no certificate to present and would read as unpaired.
pub async fn query_https(
    client: &reqwest::Client,
    address: &str,
    port: u16,
) -> Result<ServerInfo, String> {
    let authority = with_port(address, port);
    query(client, &format!("https://{authority}/serverinfo?uniqueid=0&uuid=0"), true).await
}

async fn query(
    client: &reqwest::Client,
    url: &str,
    authenticated: bool,
) -> Result<ServerInfo, String> {
    let started = Instant::now();
    let response = client
        .get(url)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let rtt_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;

    if !response.status().is_success() {
        return Err(format!("serverinfo returned {}", response.status()));
    }

    let body = response.text().await.map_err(|e| e.to_string())?;
    let (status_code, fields) = parse(&body)?;
    Ok(ServerInfo {
        fields,
        rtt_ms,
        authenticated,
        status_code,
    })
}

/// Append the default port unless the address already names one.
///
/// Bracketed IPv6 literals (`[::1]`) keep their brackets; a bare IPv6 literal
/// is bracketed here so the URL stays parseable.
fn with_port(address: &str, port: u16) -> String {
    let trimmed = address.trim();

    if trimmed.starts_with('[') {
        return if trimmed.rfind("]:").is_some() {
            trimmed.to_string()
        } else {
            format!("{trimmed}:{port}")
        };
    }

    // More than one colon means a bare IPv6 literal, which must be bracketed
    // before a port can be attached unambiguously.
    if trimmed.matches(':').count() > 1 {
        return format!("[{trimmed}]:{port}");
    }

    if trimmed.contains(':') {
        trimmed.to_string()
    } else {
        format!("{trimmed}:{port}")
    }
}

/// Flatten the top-level children of `<root>` into a lowercase-keyed map,
/// alongside the `status_code` attribute on `<root>` itself.
///
/// The document is flat, so a tag/text scan is enough and avoids binding the
/// parser to a field list that Sunshine may extend.
fn parse(xml: &str) -> Result<(u16, HashMap<String, String>), String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut fields = HashMap::new();
    let mut status = STATUS_OK;
    let mut depth = 0usize;
    let mut current: Option<String> = None;
    let mut buf = Vec::new();
    let mut saw_root = false;

    loop {
        match reader.read_event_into(&mut buf) {
            // An unauthorized response is a self-closing <root/>, so the
            // status has to be read from Empty as well as Start.
            Ok(Event::Empty(e)) => {
                depth += 1;
                if depth == 1 {
                    saw_root = true;
                    status = root_status(&e).unwrap_or(status);
                }
                depth -= 1;
            }
            Ok(Event::Start(e)) => {
                depth += 1;
                if depth == 1 {
                    saw_root = true;
                    status = root_status(&e).unwrap_or(status);
                }
                // Depth 1 is <root>; depth 2 is the fields we want.
                if depth == 2 {
                    current = Some(
                        String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase(),
                    );
                }
            }
            Ok(Event::Text(e)) => {
                if let Some(key) = &current {
                    let text = e.unescape().map_err(|e| e.to_string())?.into_owned();
                    fields.entry(key.clone()).or_insert(text);
                }
            }
            Ok(Event::CData(e)) => {
                if let Some(key) = &current {
                    let text = String::from_utf8_lossy(&e.into_inner()).into_owned();
                    fields.entry(key.clone()).or_insert(text);
                }
            }
            Ok(Event::End(_)) => {
                if depth == 2 {
                    current = None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("malformed serverinfo XML: {e}")),
            _ => {}
        }
        buf.clear();
    }

    if !saw_root {
        return Err("serverinfo response was not a serverinfo document".into());
    }
    // A non-OK status legitimately carries no fields — that response is the
    // answer, not a malformed one.
    if fields.is_empty() && status == STATUS_OK {
        return Err("serverinfo response had no fields".into());
    }
    Ok((status, fields))
}

fn root_status(e: &quick_xml::events::BytesStart<'_>) -> Option<u16> {
    let attr = e
        .attributes()
        .flatten()
        .find(|a| a.key.as_ref().eq_ignore_ascii_case(b"status_code"))?;
    String::from_utf8_lossy(&attr.value).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root status_code="200">
  <hostname>WORKSHOP</hostname>
  <appversion>7.1.431.-1</appversion>
  <uniqueid>5c0e2c0e-0000-4000-8000-000000000001</uniqueid>
  <mac>00:11:22:33:44:55</mac>
  <LocalIP>192.168.1.40</LocalIP>
  <PairStatus>1</PairStatus>
  <currentgame>0</currentgame>
  <state>SUNSHINE_SERVER_FREE</state>
</root>"#;

    /// Exactly what a local Sunshine returns to an unpaired client on the TLS
    /// port: HTTP 200, a self-closing root, and the real status in the body.
    const UNAUTHORIZED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root status_code="401" query="" status_message="The client is not authorized. Certificate verification failed."/>"#;

    fn info(xml: &str, authenticated: bool) -> ServerInfo {
        let (status_code, fields) = parse(xml).expect("parses");
        ServerInfo {
            fields,
            rtt_ms: 3,
            authenticated,
            status_code,
        }
    }

    #[test]
    fn reads_flat_fields_case_insensitively() {
        let i = info(SAMPLE, false);
        assert_eq!(i.hostname(), Some("WORKSHOP"));
        assert_eq!(i.details().local_ip.as_deref(), Some("192.168.1.40"));
        assert_eq!(
            i.unique_id(),
            Some("5c0e2c0e-0000-4000-8000-000000000001")
        );
    }

    #[test]
    fn pairing_is_unknown_without_a_client_certificate() {
        // PairStatus says 1, but over plain HTTP that value is not meaningful.
        assert_eq!(info(SAMPLE, false).pairing(), PairingState::Unknown);
        assert_eq!(info(SAMPLE, true).pairing(), PairingState::Paired);
    }

    #[test]
    fn busy_state_reads_as_hosting() {
        let xml = SAMPLE.replace("SUNSHINE_SERVER_FREE", "SUNSHINE_SERVER_BUSY");
        assert!(matches!(info(&xml, false).activity(), Activity::Hosting { .. }));
        assert_eq!(info(SAMPLE, false).activity(), Activity::Idle);
    }

    #[test]
    fn a_running_app_reads_as_hosting_even_when_state_says_free() {
        let xml = SAMPLE.replace("<currentgame>0</currentgame>", "<currentgame>881448767</currentgame>");
        match info(&xml, false).activity() {
            Activity::Hosting { app_id, .. } => {
                assert_eq!(app_id.as_deref(), Some("881448767"));
            }
            other => panic!("expected hosting, got {other:?}"),
        }
    }

    #[test]
    fn ports_are_only_added_when_absent() {
        assert_eq!(with_port("192.168.1.40", 47989), "192.168.1.40:47989");
        assert_eq!(with_port("192.168.1.40:1234", 47989), "192.168.1.40:1234");
        assert_eq!(with_port("workshop.local", 47989), "workshop.local:47989");
        assert_eq!(with_port("[::1]:1234", 47989), "[::1]:1234");
        assert_eq!(with_port("[::1]", 47989), "[::1]:47989");
        assert_eq!(with_port("fe80::1", 47989), "[fe80::1]:47989");
    }

    #[test]
    fn malformed_bodies_are_errors_not_empty_devices() {
        assert!(parse("not xml at all").is_err());
        assert!(parse("<root></root>").is_err());
    }

    #[test]
    fn an_unauthorized_body_parses_despite_carrying_no_fields() {
        // Sunshine answers HTTP 200 here, so the only signal is in the body.
        let (status, fields) = parse(UNAUTHORIZED).expect("parses");
        assert_eq!(status, STATUS_UNAUTHORIZED);
        assert!(fields.is_empty());
    }

    #[test]
    fn a_rejected_certificate_means_not_paired_not_unknown() {
        // The host told us plainly. Falling back to Unknown would throw away
        // the best answer we are ever going to get.
        assert_eq!(info(UNAUTHORIZED, true).pairing(), PairingState::NotPaired);
    }

    #[test]
    fn an_unauthenticated_probe_stays_unknown_even_on_a_401() {
        // Without our certificate the 401 says nothing about pairing.
        assert_eq!(info(UNAUTHORIZED, false).pairing(), PairingState::Unknown);
    }

    #[test]
    fn a_missing_status_code_attribute_defaults_to_ok() {
        let (status, _) = parse(SAMPLE.replace(" status_code=\"200\"", "").as_str())
            .expect("parses");
        assert_eq!(status, STATUS_OK);
    }
}
