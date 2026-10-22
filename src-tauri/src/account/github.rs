//! The GitHub side of signing in: the device flow, and the machine list.
//!
//! # Signing in without a server
//!
//! GitHub's device flow needs only a public client ID — no secret, no
//! redirect, nothing Dusk has to host. Dusk asks for a short code, the
//! person types it at github.com/login/device, and Dusk polls until GitHub
//! hands over a token.
//!
//! # The machine list
//!
//! Every signed-in machine writes its name and public key to one secret gist
//! on the account, one file per machine so two machines writing at once
//! never overwrite each other. Another machine on the same account reads it
//! to check a pairing request's signature. Only public keys are in it, and
//! only the account can change it.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const API: &str = "https://api.github.com";
pub const VERIFICATION_URL: &str = "https://github.com/login/device";

/// Gists are the only thing Dusk touches.
const SCOPE: &str = "gist";
/// How Dusk finds its gist among the account's others.
const GIST_DESCRIPTION: &str = "Dusk machines";
const FILE_PREFIX: &str = "dusk-machine-";
pub(crate) const TIMEOUT: Duration = Duration::from_secs(15);

/// The OAuth app's client ID. Public by design — the device flow has no
/// secret — so it is baked in at build time, with an environment override
/// for development.
pub fn client_id() -> Option<String> {
    std::env::var("DUSK_GITHUB_CLIENT_ID")
        .ok()
        .or_else(|| option_env!("DUSK_GITHUB_CLIENT_ID").map(String::from))
        .filter(|id| !id.trim().is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: u64,
    pub login: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// One machine on the account, as its file in the gist says.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub key: String,
    /// Where this machine answers, for the machines that cannot find it.
    ///
    /// mDNS does not cross a VPN, so from another network the account knows
    /// which machines are yours and nothing knows where they are. Publishing
    /// them here leans on an overlay network the person already runs rather
    /// than on a rendezvous service this project will not operate — see
    /// `docs/addresses.md`.
    ///
    /// Defaulted so an entry written before this still loads; such a machine
    /// is simply one with no published addresses.
    /// Only ever read, never written, since v0.3.21: entries published
    /// before sealing existed put the addresses here in the clear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// This machine's public key for receiving sealed messages, hex. Absent
    /// on a machine that has not signed in since sealing arrived; such a
    /// machine cannot be sent addresses and is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_key: Option<String>,
    /// The addresses, sealed to every machine on the account that published
    /// a box key. See [`crate::account::sealed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<crate::account::sealed::Envelope>,
    /// Unix seconds, so a reader can prefer the freshest of two answers.
    /// Written when the addresses change, never on a timer: a heartbeat
    /// would be a rendezvous service with extra steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    SlowDown,
    Token(Tokens),
    Failed(String),
}

/// What GitHub hands back, which is not only an access token.
///
/// # Why there is no refresh here
///
/// An OAuth app can be set to expire user tokens, and when it is, the device
/// flow returns a refresh token beside the access token. The obvious move is
/// to keep it and renew. GitHub will not allow it:
///
/// ```text
/// POST /login/oauth/access_token  grant_type=refresh_token, client_id=…
/// {"error": "incorrect_client_credentials",
///  "error_description": "The client_id and/or client_secret passed are incorrect."}
/// ```
///
/// The device flow is secret-less on the way *in*, but the refresh grant is
/// a confidential-client operation and wants the client secret. Dusk has
/// none to give: one shipped in the binary is readable by anyone who runs
/// `strings` on it, and holding it anywhere else means running a server,
/// which this project deliberately does not do.
///
/// So the fix is on GitHub's side — the app must be set *not* to expire its
/// tokens — and Dusk's job is to notice when one has and say so. That is
/// what `expires_at` is for. Before it existed the only symptom was other
/// machines quietly ceasing to be yours eight hours after signing in: an
/// expired token cannot read the account's machine list, and every pairing
/// proof then looks like a stranger's.
///
/// `refresh` is kept only so the stored shape need not change if GitHub ever
/// permits this from a public client. Nothing reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    /// Unix seconds. Absent when the token does not expire, which is how the
    /// app is meant to be configured.
    pub expires_at: Option<u64>,
}

/// Seconds before the stated expiry at which a token is treated as done.
///
/// A token that expires during the request it was attached to fails exactly
/// like a wrong one, so the margin covers a slow round trip and a clock that
/// disagrees with GitHub's by a little.
pub const EXPIRY_MARGIN: u64 = 120;

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Tokens {
    /// Whether GitHub will still accept this.
    ///
    /// A token with no stated expiry never lapses — the configuration Dusk
    /// wants — so it is never reported as expired.
    pub fn expired(&self) -> bool {
        self.expires_at
            .is_some_and(|at| now_unix() + EXPIRY_MARGIN >= at)
    }
}

fn tokens_from(body: &Value) -> Option<Tokens> {
    let access = body["access_token"].as_str()?.to_string();
    Some(Tokens {
        access,
        refresh: body["refresh_token"].as_str().map(String::from),
        expires_at: body["expires_in"].as_u64().map(|secs| now_unix() + secs),
    })
}

fn api(
    http: &reqwest::Client,
    method: reqwest::Method,
    path: &str,
    token: &str,
) -> reqwest::RequestBuilder {
    http.request(method, format!("{API}{path}"))
        .bearer_auth(token)
        .header(reqwest::header::USER_AGENT, "dusk")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(TIMEOUT)
}

/// What GitHub says when it will not take the token any more.
///
/// Named so a caller can recognise it rather than match on prose: a stale
/// token is one of the few errors worth *acting* on, because another process
/// may have signed in since and the keystore may already hold a good one.
pub const REJECTED: &str = "GitHub no longer accepts this sign-in. Sign in again.";

/// Whether an error is GitHub refusing the token itself.
/// A request that never got an answer, said without the library's own
/// text: that names the URL and the socket error, and reaches the screen
/// as the reason a machine "is not on your account".
fn unreachable(err: reqwest::Error) -> String {
    if err.is_timeout() {
        "GitHub did not answer in time".to_string()
    } else {
        "could not reach GitHub".to_string()
    }
}

pub fn is_rejected(err: &str) -> bool {
    err == REJECTED
}

async fn json_of(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(REJECTED.into());
    }
    let body: Value = response.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let said = body["message"].as_str().unwrap_or("no reason given");
        return Err(format!("GitHub answered {status}: {said}"));
    }
    Ok(body)
}

pub async fn start(http: &reqwest::Client, client_id: &str) -> Result<DeviceCode, String> {
    let response = http
        .post(DEVICE_CODE_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "dusk")
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(unreachable)?;
    let body = json_of(response).await?;
    if let Some(err) = body["error_description"]
        .as_str()
        .or(body["error"].as_str())
    {
        return Err(format!("GitHub would not start a sign-in: {err}"));
    }
    serde_json::from_value(body).map_err(|e| e.to_string())
}

pub async fn poll(http: &reqwest::Client, client_id: &str, device_code: &str) -> Poll {
    let sent = http
        .post(TOKEN_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "dusk")
        .form(&[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .timeout(TIMEOUT)
        .send()
        .await;
    match sent {
        // A dropped poll is not a verdict; the next one may get through.
        Err(_) => Poll::Pending,
        Ok(response) => match response.json::<Value>().await {
            Ok(body) => parse_poll(&body),
            Err(_) => Poll::Pending,
        },
    }
}

fn parse_poll(body: &Value) -> Poll {
    if let Some(tokens) = tokens_from(body) {
        return Poll::Token(tokens);
    }
    match body["error"].as_str() {
        Some("authorization_pending") | None => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => {
            Poll::Failed("The code expired before it was entered. Try again.".into())
        }
        Some("access_denied") => Poll::Failed("The sign-in was cancelled on GitHub.".into()),
        Some(other) => Poll::Failed(
            body["error_description"]
                .as_str()
                .unwrap_or(other)
                .to_string(),
        ),
    }
}

pub async fn user(http: &reqwest::Client, token: &str) -> Result<User, String> {
    let body = json_of(
        api(http, reqwest::Method::GET, "/user", token)
            .send()
            .await
            .map_err(unreachable)?,
    )
    .await?;
    Ok(User {
        id: body["id"]
            .as_u64()
            .ok_or("GitHub did not say who signed in")?,
        login: body["login"].as_str().unwrap_or_default().to_string(),
        avatar_url: body["avatar_url"].as_str().unwrap_or_default().to_string(),
    })
}

/// The account's machine-list gist, if one exists.
pub async fn find_list(http: &reqwest::Client, token: &str) -> Result<Option<String>, String> {
    let body = json_of(
        api(http, reqwest::Method::GET, "/gists?per_page=100", token)
            .send()
            .await
            .map_err(unreachable)?,
    )
    .await?;
    Ok(pick_list(&body))
}

fn pick_list(gists: &Value) -> Option<String> {
    gists
        .as_array()?
        .iter()
        .find(|g| g["description"].as_str() == Some(GIST_DESCRIPTION) && g["public"] == false)
        .and_then(|g| g["id"].as_str())
        .map(String::from)
}

fn file_name(machine: &str) -> String {
    format!("{FILE_PREFIX}{machine}.json")
}

/// Write this machine's entry, creating the list if there is none. Returns
/// the gist's ID.
pub async fn publish(
    http: &reqwest::Client,
    token: &str,
    list: Option<&str>,
    machine: &str,
    entry: &Entry,
) -> Result<String, String> {
    let content = serde_json::to_string_pretty(entry).map_err(|e| e.to_string())?;
    let files = json!({ file_name(machine): { "content": content } });
    let request = match list {
        Some(id) => api(http, reqwest::Method::PATCH, &format!("/gists/{id}"), token)
            .json(&json!({ "files": files })),
        None => api(http, reqwest::Method::POST, "/gists", token).json(&json!({
            "description": GIST_DESCRIPTION,
            "public": false,
            "files": files,
        })),
    };
    let body = json_of(request.send().await.map_err(unreachable)?).await?;
    body["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "GitHub did not return the machine list".into())
}

pub async fn unpublish(
    http: &reqwest::Client,
    token: &str,
    list: &str,
    machine: &str,
) -> Result<(), String> {
    let files = json!({ file_name(machine): null });
    json_of(
        api(
            http,
            reqwest::Method::PATCH,
            &format!("/gists/{list}"),
            token,
        )
        .json(&json!({ "files": files }))
        .send()
        .await
        .map_err(unreachable)?,
    )
    .await
    .map(|_| ())
}

/// Every machine on the list, by machine ID.
pub async fn read_list(
    http: &reqwest::Client,
    token: &str,
    list: &str,
) -> Result<HashMap<String, Entry>, String> {
    let body = json_of(
        api(http, reqwest::Method::GET, &format!("/gists/{list}"), token)
            .send()
            .await
            .map_err(unreachable)?,
    )
    .await?;
    Ok(parse_list(&body))
}

fn parse_list(gist: &Value) -> HashMap<String, Entry> {
    let Some(files) = gist["files"].as_object() else {
        return HashMap::new();
    };
    files
        .iter()
        .filter_map(|(name, file)| {
            let machine = name.strip_prefix(FILE_PREFIX)?.strip_suffix(".json")?;
            let entry = serde_json::from_str(file["content"].as_str()?).ok()?;
            Some((machine.to_string(), entry))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_reads_as_pending_until_there_is_a_token() {
        assert_eq!(
            parse_poll(&json!({"error": "authorization_pending"})),
            Poll::Pending
        );
        assert_eq!(
            parse_poll(&json!({"error": "slow_down", "interval": 10})),
            Poll::SlowDown
        );
        // An app that does not expire tokens sends neither a refresh token
        // nor an expiry, and such a token is never treated as spent.
        assert_eq!(
            parse_poll(&json!({"access_token": "gho_x", "token_type": "bearer"})),
            Poll::Token(Tokens {
                access: "gho_x".into(),
                refresh: None,
                expires_at: None,
            })
        );
        assert!(matches!(
            parse_poll(&json!({"error": "expired_token"})),
            Poll::Failed(_)
        ));

        // The case that actually bit: this app expires tokens at eight
        // hours and sends a refresh token with them. Keeping only the
        // access token made every machine stop being recognised by its own
        // account overnight, with nothing said.
        let Poll::Token(t) = parse_poll(&json!({
            "access_token": "gho_x",
            "refresh_token": "ghr_y",
            "expires_in": 28800,
            "token_type": "bearer"
        })) else {
            panic!("a token answer must parse as one");
        };
        assert_eq!(t.refresh.as_deref(), Some("ghr_y"));
        let at = t.expires_at.expect("an expiry must be kept");
        assert!(at > now_unix() + 28000 && at <= now_unix() + 28800, "{at}");
        assert!(!t.expired(), "a token issued seconds ago has not lapsed");
        assert!(matches!(
            parse_poll(&json!({"error": "access_denied"})),
            Poll::Failed(_)
        ));
    }

    #[test]
    fn a_token_near_its_expiry_counts_as_lapsed() {
        // The margin exists so a token cannot lapse mid-request, which
        // fails exactly like a wrong one and is far harder to recognise.
        let nearly = Tokens {
            access: "gho_x".into(),
            refresh: Some("ghr_y".into()),
            expires_at: Some(now_unix() + EXPIRY_MARGIN / 2),
        };
        assert!(nearly.expired());

        let fresh = Tokens {
            access: "gho_x".into(),
            refresh: Some("ghr_y".into()),
            expires_at: Some(now_unix() + 3600),
        };
        assert!(!fresh.expired());
    }

    #[test]
    fn a_token_with_no_stated_expiry_never_lapses() {
        // The configuration Dusk actually wants: the OAuth app set not to
        // expire tokens, so there is no expiry and nothing to report. This
        // also covers entries written before Dusk recorded one.
        let forever = Tokens {
            access: "gho_x".into(),
            refresh: None,
            expires_at: None,
        };
        assert!(!forever.expired());

        // But a stated expiry in the past counts, refresh token or not --
        // there is no renewing it, so the only honest answer is to say so.
        let lapsed = Tokens {
            access: "gho_x".into(),
            refresh: None,
            expires_at: Some(now_unix().saturating_sub(10_000)),
        };
        assert!(lapsed.expired());
    }

    #[test]
    fn only_the_secret_gist_with_dusks_description_is_the_list() {
        let gists = json!([
            {"id": "a", "description": "notes", "public": false},
            {"id": "b", "description": "Dusk machines", "public": true},
            {"id": "c", "description": "Dusk machines", "public": false},
        ]);
        assert_eq!(pick_list(&gists), Some("c".into()));
        assert_eq!(pick_list(&json!([])), None);
    }

    #[test]
    fn the_list_is_read_from_dusks_files_only() {
        let gist = json!({"files": {
            "dusk-machine-ab12.json": {"content": "{\"name\":\"Desk\",\"key\":\"00ff\"}"},
            "dusk-machine-cd34.json": {"content": "not json"},
            "README.md": {"content": "hi"},
        }});
        let list = parse_list(&gist);
        assert_eq!(list.len(), 1);
        assert_eq!(
            list["ab12"],
            Entry {
                name: "Desk".into(),
                key: "00ff".into(),
                addresses: Vec::new(),
                box_key: None,
                sealed: None,
                updated: None,
            }
        );
    }
}
