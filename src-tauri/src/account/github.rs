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
const TIMEOUT: Duration = Duration::from_secs(15);

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
}

#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    SlowDown,
    Token(String),
    Failed(String),
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

async fn json_of(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("GitHub no longer accepts this sign-in. Sign in again.".into());
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
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
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
    if let Some(token) = body["access_token"].as_str() {
        return Poll::Token(token.to_string());
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
            .map_err(|e| e.to_string())?,
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
            .map_err(|e| e.to_string())?,
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
    let body = json_of(request.send().await.map_err(|e| e.to_string())?).await?;
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
        .map_err(|e| e.to_string())?,
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
            .map_err(|e| e.to_string())?,
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
        assert_eq!(
            parse_poll(&json!({"access_token": "gho_x", "token_type": "bearer"})),
            Poll::Token("gho_x".into())
        );
        assert!(matches!(
            parse_poll(&json!({"error": "expired_token"})),
            Poll::Failed(_)
        ));
        assert!(matches!(
            parse_poll(&json!({"error": "access_denied"})),
            Poll::Failed(_)
        ));
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
                key: "00ff".into()
            }
        );
    }
}
