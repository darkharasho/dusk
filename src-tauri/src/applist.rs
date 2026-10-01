//! The host's app list.
//!
//! Taken from GameStream's `applist` rather than `moonlight list` on purpose:
//! it is structured XML instead of CLI text, and Dusk already holds the
//! certificate the endpoint requires. It is also what resolves a running
//! app's id into a name, which is the difference between a card reading
//! "In a session" and "Streaming Desktop".
//!
//! Unlike `serverinfo`, this document is nested — `<root><App>…</App></root>`
//! — so it gets its own parser rather than reusing the flat one.

use std::time::Duration;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

use crate::serverinfo::{STATUS_OK, STATUS_UNAUTHORIZED};

const TIMEOUT: Duration = Duration::from_millis(4000);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostApp {
    pub id: String,
    pub name: String,
    pub hdr: bool,
}

/// Fetch the app list over TLS. `client` must present our client certificate.
pub async fn query(
    client: &reqwest::Client,
    address: &str,
    tls_port: u16,
) -> Result<Vec<HostApp>, String> {
    let authority = crate::serverinfo::with_port(address, tls_port);
    let url = format!("https://{authority}/applist?uniqueid=0&uuid=0");

    let response = client
        .get(&url)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| crate::serverinfo::describe(&e))?;

    if !response.status().is_success() {
        return Err(format!("applist returned {}", response.status()));
    }

    let body = response
        .text()
        .await
        .map_err(|e| crate::serverinfo::describe(&e))?;
    parse(&body)
}

fn parse(xml: &str) -> Result<Vec<HostApp>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut apps = Vec::new();
    let mut status = STATUS_OK;
    let mut current: Option<HostApp> = None;
    let mut field: Option<String> = None;
    let mut buf = Vec::new();
    let mut depth = 0usize;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) if depth == 0 => {
                status = root_status(&e).unwrap_or(status);
            }
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                if depth == 0 {
                    status = root_status(&e).unwrap_or(status);
                } else if depth == 1 && name == "app" {
                    current = Some(HostApp {
                        id: String::new(),
                        name: String::new(),
                        hdr: false,
                    });
                } else if depth == 2 {
                    field = Some(name);
                }
                depth += 1;
            }
            Ok(Event::Text(e)) => {
                let (Some(app), Some(key)) = (current.as_mut(), field.as_deref()) else {
                    buf.clear();
                    continue;
                };
                let text = e.unescape().map_err(|e| e.to_string())?.into_owned();
                match key {
                    "id" => app.id = text,
                    "apptitle" => app.name = text,
                    "ishdrsupported" => app.hdr = text.trim() == "1",
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                if depth == 2 {
                    field = None;
                } else if depth == 1 {
                    // Closing an <App>. An entry with no id is unusable —
                    // it is what `stream` would be handed — so it is dropped
                    // rather than shown as a launchable thing that is not.
                    if let Some(app) = current.take() {
                        if !app.id.is_empty() {
                            apps.push(app);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("malformed applist XML: {e}")),
            _ => {}
        }
        buf.clear();
    }

    if status == STATUS_UNAUTHORIZED {
        return Err("Not paired with this machine.".into());
    }
    if status != STATUS_OK {
        return Err(format!("applist returned status {status}"));
    }
    Ok(apps)
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
  <App>
    <IsHdrSupported>1</IsHdrSupported>
    <AppTitle>Desktop</AppTitle>
    <ID>881448767</ID>
  </App>
  <App>
    <IsHdrSupported>0</IsHdrSupported>
    <AppTitle>Steam Big Picture</AppTitle>
    <ID>1ota</ID>
  </App>
</root>"#;

    #[test]
    fn reads_every_app_with_its_id_and_name() {
        let apps = parse(SAMPLE).expect("parses");
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Desktop");
        assert_eq!(apps[0].id, "881448767");
        assert!(apps[0].hdr);
        assert_eq!(apps[1].name, "Steam Big Picture");
        assert!(!apps[1].hdr);
    }

    #[test]
    fn an_unauthorized_list_is_an_error_not_an_empty_list() {
        // An empty list would render as "this host has no apps", which is a
        // different and wrong thing to tell someone who is simply unpaired.
        let xml = r#"<root status_code="401" status_message="not authorized"/>"#;
        assert!(parse(xml).is_err());
    }

    #[test]
    fn an_entry_with_no_id_is_dropped() {
        let xml = r#"<root status_code="200"><App><AppTitle>Broken</AppTitle></App></root>"#;
        assert!(parse(xml).expect("parses").is_empty());
    }

    #[test]
    fn a_host_with_no_apps_is_an_empty_list_not_an_error() {
        assert_eq!(
            parse(r#"<root status_code="200"></root>"#).unwrap().len(),
            0
        );
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(parse("<root><App>").is_err() || parse("<root><App>").unwrap().is_empty());
        assert!(parse("not xml <<<").is_err());
    }
}

/// Against a real host, set `DUSK_TEST_HOST` to its address.
///
/// This is the regression guard for TLS session resumption: Sunshine aborts
/// a resumed handshake, and the failure only shows on the *second* request
/// through a client that has cached a session — so a single-request test
/// would pass while the app failed every few seconds. Ignored by default
/// because it needs a paired host on the network.
#[cfg(test)]
mod live_probe {
    #[tokio::test]
    #[ignore]
    async fn repeated_requests_through_one_client_all_succeed() {
        let Ok(addr) = std::env::var("DUSK_TEST_HOST") else {
            eprintln!("skipping: set DUSK_TEST_HOST to a paired host's address");
            return;
        };
        let id = crate::moonlight::identity::load().expect("a moonlight identity");
        let client = crate::state::tls_client(&id).expect("a TLS client");

        for attempt in 1..=8 {
            let apps = super::query(&client, &addr, 47984)
                .await
                .unwrap_or_else(|e| panic!("attempt {attempt} failed: {e}"));
            assert!(!apps.is_empty(), "attempt {attempt} returned no apps");
        }
    }
}
