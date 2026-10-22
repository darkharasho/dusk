//! Sunshine's local configuration API.
//!
//! This is the undocumented one. Unlike GameStream's `serverinfo`, nothing
//! depends on it staying still, so every call here is written to degrade
//! rather than to assume — and the UI treats a failure as "Dusk could not
//! read the host" rather than as a fact about the host.
//!
//! Measured against a live Sunshine rather than assumed:
//!
//! - It serves HTTPS on `47990` with a self-signed certificate.
//! - Auth is HTTP Basic and failures are a real `401`, with a JSON body, so
//!   unlike GameStream the HTTP status can be trusted here.
//! - `POST /api/pin` validates `Content-Type` **before** auth: send it
//!   without `application/json` and you get `400 Content type mismatch`
//!   whether or not your credentials are right. Anything probing this
//!   endpoint has to set the header or it will misread a content-type
//!   complaint as a working, unauthenticated endpoint.
//! - A 2026 host holds each pairing request under an id, lists them at
//!   `GET /api/pin`, and **refuses a POST that does not name one**. An
//!   older host has no such list. This is the one call here that has
//!   actually changed shape under us, so it reads the host's answer to
//!   decide which contract it is talking to rather than assuming either.

use std::time::Duration;

use serde::Deserialize;

use super::credentials::Credentials;

/// Sunshine's web UI and config API.
pub const DEFAULT_PORT: u16 = 47990;

const TIMEOUT: Duration = Duration::from_secs(8);

/// Fields `GET /api/config` reports that are *not* settings.
///
/// They have to be stripped before a save, because the endpoint replaces the
/// configuration with whatever it is handed and these would be written into
/// `sunshine.conf` as if they were settings. The list is version-sensitive:
/// a future Sunshine adding a new read-only field would have it written back
/// until this list catches up. Tested against the shape a live host returns.
const METADATA_KEYS: &[&str] = &["platform", "version", "status", "restart_supported"];

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Hosting is not set up on this machine yet. Set it up in Settings.")]
    NoCredentials,
    #[error("Hosting no longer accepts the sign-in Dusk gave it. Set it up again in Settings.")]
    Unauthorized,
    #[error("could not reach the hosting service on this machine: {0}")]
    Unreachable(String),
    #[error("{0}")]
    Failed(String),
}

pub struct SunshineApi {
    client: reqwest::Client,
    port: u16,
}

#[derive(Debug, Deserialize)]
struct ApiReply {
    #[serde(default)]
    status: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

/// A pairing request the host is holding, as `GET /api/pin` lists it.
///
/// The id is minted per request and is not stable across attempts — the same
/// client asking twice is two different ids — so it has to be read
/// immediately before it is used rather than remembered.
#[derive(Debug, Deserialize)]
struct Pending {
    id: String,
    #[serde(default)]
    address: String,
}

#[derive(Debug, Deserialize)]
struct PendingReply {
    pairings: Option<Vec<Pending>>,
}

/// Which pairing requests the host is holding, or `None` if this host does
/// not name them — a Sunshine from before the endpoint existed.
fn pending_from(status: reqwest::StatusCode, body: &str) -> Option<Vec<Pending>> {
    if !status.is_success() {
        return None;
    }
    serde_json::from_str::<PendingReply>(body)
        .ok()
        .and_then(|reply| reply.pairings)
}

/// What a PIN submission sends. `pairing_id` is left out rather than sent
/// empty: an older Sunshine has no field for it.
fn pin_body(pin: &str, device_name: &str, pairing_id: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({ "pin": pin, "name": device_name });
    if let (Some(id), Some(map)) = (pairing_id, body.as_object_mut()) {
        map.insert("pairing_id".into(), id.into());
    }
    body
}

/// The requests worth offering the PIN to, nearest guess first.
///
/// `from` is the address the asking machine reached this one on, which the
/// host reports for each waiting request. It is a hint and not a filter: the
/// two are chosen independently — a machine with several addresses may
/// appear under one here and another there — so when nothing matches, every
/// waiting request is tried. The PIN is what actually decides; a request
/// that is not the right one answers `status: false` and costs one call.
fn candidates(pairings: &[Pending], from: Option<&str>) -> Vec<String> {
    let matching: Vec<String> = from
        .map(|from| {
            pairings
                .iter()
                .filter(|p| p.address == from)
                .map(|p| p.id.clone())
                .collect()
        })
        .unwrap_or_default();
    if !matching.is_empty() {
        return matching;
    }
    pairings.iter().map(|p| p.id.clone()).collect()
}

impl SunshineApi {
    /// `danger_accept_invalid_certs` is unavoidable: Sunshine generates its
    /// own certificate and there is no authority to check it against. The
    /// exposure is bounded by only ever talking to loopback — a certificate
    /// cannot be spoofed by anything that is not already running as this
    /// user on this machine.
    pub fn new(port: u16) -> Result<Self, ApiError> {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .connect_timeout(Duration::from_millis(1500))
            .build()
            .map_err(|e| ApiError::Failed(e.to_string()))?;
        Ok(Self { client, port })
    }

    fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{path}", self.port)
    }

    /// Hand a pairing PIN to the local host.
    ///
    /// This is the half of pairing that is Sunshine's web page today. Doing
    /// it here is what makes hosting and connecting stop feeling like two
    /// programs.
    ///
    /// Sunshine grew a second step here. A host from 2026 holds each pairing
    /// request under an id, lists them at `GET /api/pin`, and **refuses a
    /// POST that does not name one** — `400 pairing_id must contain exactly
    /// 32 hexadecimal characters`, whatever the PIN. So the id is read first
    /// and the PIN offered against it. Measured on 2026.914.233613; an older
    /// host does not list them and is posted to as before, which is why
    /// `pending_pairings` answers `None` rather than failing.
    ///
    /// `from` is the address the machine asking to pair was reached from,
    /// used to pick between several waiting requests. `None` when nobody
    /// knows, as when the PIN was typed in by hand.
    pub async fn submit_pin(
        &self,
        credentials: &Credentials,
        pin: &str,
        device_name: &str,
        from: Option<&str>,
    ) -> Result<(), ApiError> {
        let Some(pairings) = self.pending_pairings(credentials).await? else {
            return self.post_pin(credentials, pin, device_name, None).await;
        };

        // Nobody is waiting yet. The client is a Qt application that takes
        // its time, so this is the ordinary case on the first attempts and
        // the caller retries; see `peer::enter_pin`.
        if pairings.is_empty() {
            return Err(ApiError::Failed(
                "No device is waiting to pair with this machine yet.".into(),
            ));
        }

        let mut last = None;
        for id in candidates(&pairings, from) {
            match self
                .post_pin(credentials, pin, device_name, Some(&id))
                .await
            {
                Ok(()) => return Ok(()),
                Err(ApiError::Unauthorized) => return Err(ApiError::Unauthorized),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or(ApiError::Failed(
            "That PIN was not accepted. It may have expired.".into(),
        )))
    }

    /// The pairing requests the host is holding. `None` means this host does
    /// not name them, so a PIN can be posted without one.
    async fn pending_pairings(
        &self,
        credentials: &Credentials,
    ) -> Result<Option<Vec<Pending>>, ApiError> {
        let response = self
            .client
            .get(self.url("/api/pin"))
            .basic_auth(&credentials.username, Some(&credentials.password))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        let text = response.text().await.unwrap_or_default();
        Ok(pending_from(status, &text))
    }

    async fn post_pin(
        &self,
        credentials: &Credentials,
        pin: &str,
        device_name: &str,
        pairing_id: Option<&str>,
    ) -> Result<(), ApiError> {
        let body = pin_body(pin, device_name, pairing_id);

        let response = self
            .client
            .post(self.url("/api/pin"))
            .basic_auth(&credentials.username, Some(&credentials.password))
            // Load-bearing: without it Sunshine answers 400 before it ever
            // looks at the credentials.
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }

        let reply: ApiReply = serde_json::from_str(&text).unwrap_or(ApiReply {
            status: None,
            error: None,
        });

        // Sunshine answers 200 with `status: false` when the PIN is wrong or
        // no client is waiting, so the HTTP status alone is not the verdict.
        if !status.is_success() || !truthy(reply.status.as_ref()) {
            return Err(ApiError::Failed(reply.error.unwrap_or_else(|| {
                "That PIN was not accepted. It may have expired.".into()
            })));
        }
        Ok(())
    }

    /// Read the host's whole configuration.
    ///
    /// Returns exactly what Sunshine reports, including the metadata fields
    /// below — stripping happens at save time, not here, because the UI
    /// wants the version and platform.
    pub async fn get_config(
        &self,
        credentials: &Credentials,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ApiError> {
        let response = self
            .client
            .get(self.url("/api/config"))
            .basic_auth(&credentials.username, Some(&credentials.password))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(ApiError::Failed(format!(
                "The hosting service answered {}",
                response.status()
            )));
        }

        match response.json().await {
            Ok(serde_json::Value::Object(map)) => Ok(map),
            Ok(_) => Err(ApiError::Failed(
                "The host's configuration was not in the expected shape.".into(),
            )),
            Err(e) => Err(ApiError::Failed(e.to_string())),
        }
    }

    /// Write changed settings.
    ///
    /// `POST /api/config` **replaces** the configuration with whatever is
    /// sent, so this reads the current config and merges into it rather than
    /// posting the changes alone — posting a partial object would silently
    /// erase every setting not included. There is a small race if something
    /// else writes between the read and the write; Sunshine's own web UI has
    /// the same one, and the alternative is a patch endpoint that does not
    /// exist.
    pub async fn save_config(
        &self,
        credentials: &Credentials,
        changes: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ApiError> {
        let mut config = self.get_config(credentials).await?;
        for (key, value) in changes {
            config.insert(key, value);
        }
        for key in METADATA_KEYS {
            config.remove(*key);
        }

        let response = self
            .client
            .post(self.url("/api/config"))
            .basic_auth(&credentials.username, Some(&credentials.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&serde_json::Value::Object(config))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let reply: ApiReply = serde_json::from_str(&text).unwrap_or(ApiReply {
            status: None,
            error: None,
        });

        if !status.is_success() || !truthy(reply.status.as_ref()) {
            return Err(ApiError::Failed(reply.error.unwrap_or_else(|| {
                "The host did not accept those settings.".into()
            })));
        }
        Ok(())
    }

    /// Restart Sunshine so changed settings take effect.
    ///
    /// The connection usually dies mid-request because the process goes away
    /// while answering, so a transport error here is the expected outcome
    /// rather than a failure.
    pub async fn restart(&self, credentials: &Credentials) -> Result<(), ApiError> {
        match self.restart_checked(credentials).await {
            Err(ApiError::Unauthorized) => Err(ApiError::Unauthorized),
            _ => Ok(()),
        }
    }

    /// Restart Sunshine, and say so only if it took the request: a 2xx, or
    /// the connection dying after the request went out. Refused, an error
    /// status, no answer at all, or nothing listening are all `Err`. Taking
    /// the request is still not proof it restarted; see
    /// `cert_pairing::reconcile` for how that is confirmed.
    pub async fn restart_checked(&self, credentials: &Credentials) -> Result<(), ApiError> {
        post_restart(
            &self.client,
            &self.url("/api/restart"),
            credentials,
            TIMEOUT,
        )
        .await
    }

    /// Check a username and password without changing anything.
    ///
    /// `GET /api/config` is the cheapest authenticated read there is, and it
    /// carries the version, which is worth having for the host card.
    pub async fn verify(&self, credentials: &Credentials) -> Result<Option<String>, ApiError> {
        let response = self
            .client
            .get(self.url("/api/config"))
            .basic_auth(&credentials.username, Some(&credentials.password))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(ApiError::Failed(format!(
                "The hosting service answered {}",
                response.status()
            )));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ApiError::Failed(e.to_string()))?;

        Ok(body
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string))
    }

    /// Turn Sunshine's own tray icon off, so Dusk's is the only one.
    ///
    /// Two icons for one thing is the exact seam Dusk exists to hide, and
    /// Sunshine's menu bar item is the most visible place the illusion
    /// breaks. `system_tray` covers its desktop notifications too, which is
    /// the right call for the same reason: a notification from an app the
    /// person never opened is a notification from the wrong app.
    ///
    /// Returns whether anything actually changed, because the change only
    /// takes effect on restart and restarting Sunshine drops whatever is
    /// streaming. A caller that restarts unconditionally would end a session
    /// to apply a setting that was already applied.
    pub async fn set_tray(
        &self,
        credentials: &Credentials,
        enabled: bool,
    ) -> Result<bool, ApiError> {
        let wanted = if enabled { "enabled" } else { "disabled" };

        let config = self.get_config(credentials).await?;
        if tray_is(&config, wanted) {
            return Ok(false);
        }

        let mut changes = serde_json::Map::new();
        changes.insert(TRAY_KEY.to_string(), serde_json::json!(wanted));
        self.save_config(credentials, changes).await?;
        Ok(true)
    }
}

async fn post_restart(
    client: &reqwest::Client,
    url: &str,
    credentials: &Credentials,
    timeout: Duration,
) -> Result<(), ApiError> {
    let sent = client
        .post(url)
        .basic_auth(&credentials.username, Some(&credentials.password))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({}))
        .timeout(timeout)
        .send()
        .await;

    match sent {
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            Err(ApiError::Unauthorized)
        }
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(ApiError::Failed(format!(
            "The hosting service answered {}",
            response.status()
        ))),
        // Never reached it, or it never answered: nothing says it restarted.
        Err(e) if e.is_connect() || e.is_timeout() || e.is_builder() => {
            Err(ApiError::Unreachable(e.to_string()))
        }
        // The request went out and the connection died under the answer,
        // which is the process going away while it replies.
        Err(_) => Ok(()),
    }
}

/// Sunshine's own key for its tray icon and desktop notifications.
///
/// Its web UI writes the strings "enabled" and "disabled" rather than a JSON
/// bool, and `sunshine.conf` is read back the same way.
pub const TRAY_KEY: &str = "system_tray";

/// Whether the tray setting already reads as `wanted`.
///
/// An absent key means Sunshine is on its default, which is enabled — so a
/// missing key must not be read as "already whatever we are asking for", or
/// the very first attempt to hide the icon decides there is nothing to do and
/// the icon stays up.
fn tray_is(config: &serde_json::Map<String, serde_json::Value>, wanted: &str) -> bool {
    config
        .get(TRAY_KEY)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("enabled")
        .eq_ignore_ascii_case(wanted)
}

/// Sunshine has spelled `status` as both a bool and the string "true".
fn truthy(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
        // A reply with no status at all is treated as success only because
        // the HTTP status was already checked by the caller.
        None => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_accepted_in_both_spellings() {
        assert!(truthy(Some(&serde_json::json!(true))));
        assert!(truthy(Some(&serde_json::json!("true"))));
        assert!(truthy(Some(&serde_json::json!("True"))));
        assert!(!truthy(Some(&serde_json::json!(false))));
        assert!(!truthy(Some(&serde_json::json!("false"))));
    }

    #[test]
    fn an_unset_tray_key_counts_as_enabled() {
        // Sunshine omits the key until someone changes it, and its default is
        // on. Reading the absence as "already disabled" would mean the icon we
        // are trying to hide never gets hidden.
        let empty = serde_json::Map::new();
        assert!(tray_is(&empty, "enabled"));
        assert!(!tray_is(&empty, "disabled"));
    }

    #[test]
    fn the_tray_key_is_compared_as_sunshine_writes_it() {
        let config: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"system_tray":"disabled"}"#).expect("parses");
        assert!(tray_is(&config, "disabled"));
        assert!(!tray_is(&config, "enabled"));
    }

    #[test]
    fn a_missing_status_defers_to_the_http_code() {
        assert!(truthy(None));
    }

    #[test]
    fn a_failure_body_parses_into_its_message() {
        // Exactly the shape a live Sunshine returns.
        let reply: ApiReply =
            serde_json::from_str(r#"{"error":"Unauthorized","status":false,"status_code":401}"#)
                .expect("parses");
        assert_eq!(reply.error.as_deref(), Some("Unauthorized"));
        assert!(!truthy(reply.status.as_ref()));
    }

    #[test]
    fn metadata_is_stripped_before_a_save_but_settings_are_kept() {
        // Writing `version` or `platform` back would put them in
        // sunshine.conf as though someone had configured them.
        let mut config: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{"platform":"macos","version":"2026.1","status":"true",
                    "restart_supported":true,"sunshine_name":"Workshop","qp":"28"}"#,
        )
        .expect("parses");

        for key in METADATA_KEYS {
            config.remove(*key);
        }

        assert_eq!(config.len(), 2);
        assert!(config.contains_key("sunshine_name"));
        assert!(config.contains_key("qp"));
    }

    #[test]
    fn a_host_that_lists_pairings_is_told_apart_from_one_that_does_not() {
        // Exactly what 2026.914.233613 answers to `GET /api/pin`.
        let listed = pending_from(
            reqwest::StatusCode::OK,
            r#"{"pairings":[{"address":"192.168.50.34",
                "id":"e36bd7f43066d08106f41ae394dc9eaf","name":"roth"}]}"#,
        )
        .expect("a host that names its pairings");
        assert_eq!(listed[0].id, "e36bd7f43066d08106f41ae394dc9eaf");
        assert_eq!(listed[0].address, "192.168.50.34");

        // Nobody waiting is still an answer, and a different one from "this
        // host does not name them": the first is retried, the second is
        // posted to the old way.
        assert_eq!(
            pending_from(reqwest::StatusCode::OK, r#"{"pairings":[]}"#)
                .expect("an answer")
                .len(),
            0
        );

        // An older Sunshine: no such endpoint, or some other shape.
        assert!(pending_from(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":"Not Found","status_code":404}"#
        )
        .is_none());
        assert!(pending_from(reqwest::StatusCode::OK, r#"{"status":true}"#).is_none());
        assert!(pending_from(reqwest::StatusCode::OK, "not json at all").is_none());
    }

    #[test]
    fn the_address_narrows_the_pairings_but_never_rules_them_all_out() {
        let pairings = vec![
            Pending {
                id: "a".repeat(32),
                address: "192.168.50.34".into(),
            },
            Pending {
                id: "b".repeat(32),
                address: "192.168.50.146".into(),
            },
        ];

        assert_eq!(
            candidates(&pairings, Some("192.168.50.34")),
            vec!["a".repeat(32)]
        );

        // The address the peer request came from and the one the host saw
        // the client on are chosen independently — a machine with two
        // addresses can show up under either. Matching nothing must not mean
        // pairing nothing; the PIN decides instead.
        assert_eq!(candidates(&pairings, Some("10.0.0.1")).len(), 2);
        assert_eq!(candidates(&pairings, None).len(), 2);
    }

    #[test]
    fn a_pairing_id_is_sent_only_when_there_is_one() {
        // A host that lists pairings refuses a POST without an id — 400,
        // whatever the PIN — and an older one has no field for it.
        let with = pin_body("1234", "Michaels-Mini", Some(&"e".repeat(32)));
        assert_eq!(with["pairing_id"], serde_json::json!("e".repeat(32)));
        assert_eq!(with["pin"], serde_json::json!("1234"));
        assert_eq!(with["name"], serde_json::json!("Michaels-Mini"));

        let without = pin_body("1234", "Michaels-Mini", None);
        assert!(without.get("pairing_id").is_none());
    }

    /// A stand-in for the host's restart endpoint on loopback, over plain
    /// HTTP, that does `answer` with each request once it has read it.
    async fn restart_against(
        answer: Option<&'static str>,
        timeout: Duration,
    ) -> Result<(), ApiError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/api/restart", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            // Headers and the two-byte `{}` body.
            while !seen.ends_with(b"{}") {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            match answer {
                Some(reply) => socket.write_all(reply.as_bytes()).await.unwrap(),
                // Sunshine going away while answering.
                None => drop(socket),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let credentials = Credentials {
            username: "u".into(),
            password: "p".into(),
        };
        let out = post_restart(&reqwest::Client::new(), &url, &credentials, timeout).await;
        server.abort();
        out
    }

    const LONG: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn a_restart_counts_only_when_the_host_took_it() {
        let ok = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}";
        assert!(restart_against(Some(ok), LONG).await.is_ok());
        // Gone mid-reply is what a restart usually looks like.
        assert!(restart_against(None, LONG).await.is_ok());

        let refused = "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n";
        assert!(restart_against(Some(refused), LONG).await.is_err());
        let broken = "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n";
        assert!(restart_against(Some(broken), LONG).await.is_err());
        let unauthorised = "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n";
        assert!(matches!(
            restart_against(Some(unauthorised), LONG).await,
            Err(ApiError::Unauthorized)
        ));
        // Never answering is not a restart either.
        let silent = "HTTP/1.1 200 OK\r\n";
        assert!(restart_against(Some(silent), Duration::from_millis(300))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_host_that_is_not_there_was_not_restarted() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/api/restart", listener.local_addr().unwrap());
        drop(listener);
        let credentials = Credentials {
            username: "u".into(),
            password: "p".into(),
        };
        assert!(
            post_restart(&reqwest::Client::new(), &url, &credentials, LONG)
                .await
                .is_err()
        );
    }

    #[test]
    fn an_unexpected_body_does_not_panic() {
        let reply: ApiReply = serde_json::from_str("{}").expect("parses");
        assert!(reply.error.is_none());
        assert!(truthy(reply.status.as_ref()));
    }
}
