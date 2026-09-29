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

use std::time::Duration;

use serde::Deserialize;

use super::credentials::Credentials;

/// Sunshine's web UI and config API.
pub const DEFAULT_PORT: u16 = 47990;

const TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Dusk does not have this machine's Sunshine sign-in yet")]
    NoCredentials,
    #[error("Sunshine rejected that username or password")]
    Unauthorized,
    #[error("could not reach Sunshine on this machine: {0}")]
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
    pub async fn submit_pin(
        &self,
        credentials: &Credentials,
        pin: &str,
        device_name: &str,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "pin": pin, "name": device_name });

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
                "Sunshine did not accept that PIN. It may have expired.".into()
            })));
        }
        Ok(())
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
                "Sunshine answered {}",
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
    fn an_unexpected_body_does_not_panic() {
        let reply: ApiReply = serde_json::from_str("{}").expect("parses");
        assert!(reply.error.is_none());
        assert!(truthy(reply.status.as_ref()));
    }
}
