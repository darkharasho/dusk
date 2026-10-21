//! Signing in, so your own machines trust each other without being asked.
//!
//! Pairing between two Dusks already skips the PIN, but the machine being
//! paired with still asks whoever is sitting there to press Allow. When both
//! machines are signed in to the same GitHub account, that question has
//! already been answered: they are both yours. So the connecting machine
//! signs the pairing request with its own key, and the other machine checks
//! that signature against the account's machine list and pairs without
//! putting anything on screen.
//!
//! Nothing here replaces the Allow prompt for anyone else. A request with no
//! signature, a signature from another account, or one that does not check
//! out falls through to the prompt exactly as before.
//!
//! There is no Dusk server in this. GitHub is the only party, and all it
//! holds is a secret gist of machine names and public keys — see
//! [`github`].

pub mod github;
pub mod keys;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};

use crate::state::AppState;
use github::{Entry, Poll, User};
use keys::MachineKey;

pub const ACCOUNT_EVENT: &str = "dusk://account";

/// A machine not on the cached list is looked up again, but not more often
/// than this, so a stream of bogus requests cannot become a stream of calls
/// to GitHub.
const LIST_REFRESH: Duration = Duration::from_secs(10);

/// A machine on the account, as the grid needs it: who it is, what it is
/// called, and where it said it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMachine {
    pub id: String,
    pub name: String,
    pub addresses: Vec<String>,
}

/// What a signed pairing request carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub account: u64,
    pub machine: String,
    pub signature: String,
}

/// Everything kept in the keystore, as one entry so signing in costs one
/// keystore prompt, not three.
#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    token: String,
    user: User,
    machine: String,
    key: String,
    list: Option<String>,
    /// Both default, so an entry written before Dusk kept these still
    /// loads. Such a sign-in simply cannot be renewed, and behaves as it
    /// always did until the person signs in once more.
    #[serde(default)]
    refresh: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
}

#[derive(Clone)]
struct Account {
    tokens: github::Tokens,
    user: User,
    key: MachineKey,
    list: Option<String>,
}

impl Account {
    fn to_stored(&self) -> Stored {
        Stored {
            token: self.tokens.access.clone(),
            user: self.user.clone(),
            machine: self.key.id.clone(),
            key: self.key.pkcs8_hex(),
            list: self.list.clone(),
            refresh: self.tokens.refresh.clone(),
            expires_at: self.tokens.expires_at,
        }
    }

    fn from_stored(s: Stored) -> Result<Self, String> {
        Ok(Self {
            key: MachineKey::from_parts(s.machine, &s.key)?,
            tokens: github::Tokens {
                access: s.token,
                refresh: s.refresh,
                expires_at: s.expires_at,
            },
            user: s.user,
            list: s.list,
        })
    }
}

#[derive(Default)]
struct Cached {
    read: bool,
    account: Option<Account>,
    persistent: bool,
}

#[derive(Default)]
struct MachineList {
    entries: HashMap<String, Entry>,
    fetched: Option<Instant>,
}

#[derive(Default)]
pub struct AccountState {
    cached: RwLock<Cached>,
    list: RwLock<MachineList>,
    /// The sign-in being waited on, if any, so a second press replaces it.
    signing_in: Mutex<Option<tokio::task::AbortHandle>>,
}

/// What the settings screen shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    /// False when this build has no GitHub client ID, so signing in cannot
    /// work at all.
    pub available: bool,
    pub user: Option<User>,
    pub machines: Vec<MachineView>,
    pub persistent: bool,
    /// Set when GitHub has stopped accepting the stored sign-in. The window
    /// shows this rather than letting the only symptom be other machines
    /// quietly ceasing to be yours.
    pub expired: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineView {
    pub name: String,
    pub this_machine: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInCode {
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: u64,
}

/// How a sign-in waited on in the background ended.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEvent {
    pub error: Option<String>,
}

impl AccountState {
    async fn load(&self) -> Option<Account> {
        {
            let cached = self.cached.read().await;
            if cached.read {
                return cached.account.clone();
            }
        }
        let mut cached = self.cached.write().await;
        if cached.read {
            return cached.account.clone();
        }
        match blocking(read_keystore).await {
            Ok(stored) => {
                cached.account = stored.and_then(|s| match Account::from_stored(s) {
                    Ok(account) => Some(account),
                    Err(err) => {
                        eprintln!("dusk: the stored sign-in is unusable ({err}); signed out");
                        None
                    }
                });
                cached.read = true;
                cached.persistent = true;
                cached.account.clone()
            }
            Err(err) => {
                eprintln!("dusk: could not read the sign-in from the keystore ({err})");
                None
            }
        }
    }

    async fn save(&self, account: Option<Account>) {
        let stored = account.as_ref().map(Account::to_stored);
        let written = blocking(move || match stored {
            Some(s) => write_keystore(&s),
            None => clear_keystore(),
        })
        .await;
        let mut cached = self.cached.write().await;
        cached.persistent = match written {
            Ok(()) => true,
            Err(err) => {
                eprintln!(
                    "dusk: keystore unavailable ({err}); holding the sign-in for this run only"
                );
                false
            }
        };
        cached.account = account;
        cached.read = true;
    }

    /// This machine's id on the account, if it is signed in.
    ///
    /// The public half of the key this names is already on the account's
    /// machine list, so this is not a secret — it is the join key another
    /// Dusk needs to tell whether this machine is one of the same person's.
    pub async fn machine_id(&self) -> Option<String> {
        self.load().await.map(|a| a.key.id.clone())
    }

    /// Tell the account where this machine answers, if that has changed.
    ///
    /// Only on change. Writing on a timer to prove liveness would be a
    /// rendezvous service with extra steps, which this project does not run
    /// — see `docs/addresses.md`.
    pub async fn publish_addresses(&self, http: &reqwest::Client) {
        let Some(account) = self.load().await else {
            return;
        };
        let Some(token) = self.access_token(http).await else {
            return;
        };
        let here = crate::reachable_addresses();
        if here.is_empty() {
            return;
        }
        // Compared against what the list already says, which is why this
        // runs after the list has been read rather than at sign-in only.
        let published = self
            .list
            .read()
            .await
            .entries
            .get(&account.key.id)
            .map(|e| e.addresses.clone())
            .unwrap_or_default();
        if published == here {
            return;
        }

        let entry = Entry {
            name: crate::peer::host_name(),
            key: account.key.public_hex(),
            addresses: here,
            updated: Some(github::now_unix()),
        };
        match github::publish(
            http,
            &token,
            account.list.as_deref(),
            &account.key.id,
            &entry,
        )
        .await
        {
            Ok(_) => {
                if let Err(err) = self.refresh_list(http).await {
                    eprintln!("dusk: could not re-read the machine list: {err}");
                }
            }
            // Not worth failing over: the machine is still reachable by
            // every other means, it is just harder to find from elsewhere.
            Err(err) => eprintln!("dusk: could not publish this machine's addresses: {err}"),
        }
    }

    /// Every machine on the account's list, as `(id, name)`.
    ///
    /// The grid's source of truth for "yours". Empty when signed out or
    /// before the list has been read, which the caller must treat as "not
    /// known yet" rather than as "you own nothing".
    pub async fn machines_on_account(&self) -> Vec<AccountMachine> {
        self.list
            .read()
            .await
            .entries
            .iter()
            .map(|(id, entry)| AccountMachine {
                id: id.clone(),
                name: entry.name.clone(),
                addresses: entry.addresses.clone(),
            })
            .collect()
    }

    /// Is this machine id on your account's list?
    ///
    /// False when signed out: with no list to check against, no machine can
    /// be claimed as yours, which is the right answer rather than a guess.
    pub async fn knows_machine(&self, machine: &str) -> bool {
        if self.load().await.is_none() {
            return false;
        }
        self.list.read().await.entries.contains_key(machine)
    }

    /// The message to show when the sign-in has lapsed, if it has.
    ///
    /// Said in full because the alternative is what this cost a day of:
    /// other machines silently ceasing to be yours, with the app insisting
    /// it was signed in the whole time.
    pub async fn expiry_notice(&self) -> Option<String> {
        let account = self.load().await?;
        account.tokens.expired().then(|| {
            "Your GitHub sign-in has expired, so this machine can no longer check which \
             machines are yours. Sign in again."
                .to_string()
        })
    }

    /// An access token GitHub will still accept, or nothing.
    ///
    /// Every use of the token goes through here. Reaching for
    /// `account.tokens.access` directly is how this breaks again: a lapsed
    /// token fails in a way that reads as "those are not your machines"
    /// rather than as "sign in again".
    async fn access_token(&self, _http: &reqwest::Client) -> Option<String> {
        let account = self.load().await?;
        if account.tokens.expired() {
            // Not renewed here, and cannot be: see `github::Tokens`. The
            // sign-in is kept rather than cleared, so the Account screen can
            // still say whose it was and offer to sign in again.
            return None;
        }
        Some(account.tokens.access)
    }

    /// Read the keystore and the machine list once, in the background.
    pub async fn prime(&self, http: &reqwest::Client) {
        if self.load().await.is_some() {
            if let Err(err) = self.refresh_list(http).await {
                eprintln!("dusk: could not read the account's machine list: {err}");
            }
        }
    }

    async fn refresh_list(&self, http: &reqwest::Client) -> Result<(), String> {
        let Some(account) = self.load().await else {
            return Ok(());
        };
        // This is the call that quietly stopped working when a token aged
        // out, and with it the account's whole purpose: an unreadable list
        // makes every other machine look like somebody else's.
        let token = match self.access_token(http).await {
            Some(token) => token,
            None => {
                // Said out loud rather than returned as an empty list: an
                // empty machine list is indistinguishable from "none of
                // these are yours", which is the confusion to avoid.
                return Err(self
                    .expiry_notice()
                    .await
                    .unwrap_or_else(|| "Dusk is not signed in to GitHub.".to_string()));
            }
        };
        let list = match &account.list {
            Some(id) => Some(id.clone()),
            None => github::find_list(http, &token).await?,
        };
        let entries = match list {
            Some(id) => github::read_list(http, &token, &id).await?,
            None => HashMap::new(),
        };
        let mut cached = self.list.write().await;
        cached.entries = entries;
        cached.fetched = Some(Instant::now());
        Ok(())
    }

    pub async fn view(&self) -> AccountView {
        let account = self.load().await;
        let own = account.as_ref().map(|a| a.key.id.clone());
        let mut machines: Vec<MachineView> = self
            .list
            .read()
            .await
            .entries
            .iter()
            .map(|(id, entry)| MachineView {
                name: entry.name.clone(),
                this_machine: Some(id) == own.as_ref(),
            })
            .collect();
        machines.sort_by(|a, b| {
            b.this_machine
                .cmp(&a.this_machine)
                .then(a.name.cmp(&b.name))
        });
        let expired = account.as_ref().filter(|a| a.tokens.expired()).map(|_| {
            "Your GitHub sign-in has expired, so this machine can no longer check which \
                 machines are yours. Sign in again."
                .to_string()
        });
        AccountView {
            available: github::client_id().is_some(),
            user: account.map(|a| a.user),
            machines,
            persistent: self.cached.read().await.persistent,
            expired,
        }
    }

    /// Sign this pairing request, if this machine is signed in.
    pub async fn proof(&self, pin: &str) -> Option<Proof> {
        let account = self.load().await?;
        Some(Proof {
            account: account.user.id,
            machine: account.key.id.clone(),
            signature: account.key.sign_pair(pin),
        })
    }

    /// Whether `proof` shows the request comes from another machine on this
    /// machine's own account.
    pub async fn vouches(&self, http: &reqwest::Client, proof: &Proof, pin: &str) -> bool {
        let Some(account) = self.load().await else {
            return false;
        };
        if account.user.id != proof.account {
            return false;
        }
        let known = self.list.read().await.entries.contains_key(&proof.machine);
        let stale = self
            .list
            .read()
            .await
            .fetched
            .map_or(true, |at| at.elapsed() > LIST_REFRESH);
        if !known && stale {
            if let Err(err) = self.refresh_list(http).await {
                eprintln!("dusk: could not read the account's machine list: {err}");
            }
        }
        vouch(account.user.id, &self.list.read().await.entries, proof, pin)
    }
}

/// The check itself, with nothing to fetch.
fn vouch(own_account: u64, list: &HashMap<String, Entry>, proof: &Proof, pin: &str) -> bool {
    proof.account == own_account
        && list
            .get(&proof.machine)
            .is_some_and(|entry| keys::verify_pair(&entry.key, pin, &proof.signature))
}

/// Start signing in. Returns the code to show; the rest happens in the
/// background and ends with an [`ACCOUNT_EVENT`].
pub async fn begin(app: AppHandle, state: Arc<AppState>) -> Result<SignInCode, String> {
    let client_id =
        github::client_id().ok_or("This build of Dusk was made without GitHub sign-in.")?;
    let code = github::start(&state.http, &client_id).await?;
    let shown = SignInCode {
        user_code: code.user_code.clone(),
        verification_url: github::VERIFICATION_URL.into(),
        expires_in: code.expires_in,
    };

    let worker = state.clone();
    let task = tauri::async_runtime::spawn(async move {
        let result = wait_for_token(&worker.http, &client_id, &code).await;
        let result = match result {
            Ok(token) => finish(&worker.http, &worker.account, token)
                .await
                .map(|_| ()),
            Err(err) => Err(err),
        };
        let _ = app.emit(
            ACCOUNT_EVENT,
            AccountEvent {
                error: result.err(),
            },
        );
    });
    if let Some(previous) = state
        .account
        .signing_in
        .lock()
        .await
        .replace(task.inner().abort_handle())
    {
        previous.abort();
    }
    Ok(shown)
}

pub async fn cancel(state: &AppState) {
    if let Some(task) = state.account.signing_in.lock().await.take() {
        task.abort();
    }
}

/// Wait for the person to enter the code on github.com.
///
/// Takes the HTTP client rather than the whole application state so that
/// `dusk signin` can wait for the same code on a machine with no window —
/// which is the whole point of a device flow.
pub(crate) async fn wait_for_token(
    http: &reqwest::Client,
    client_id: &str,
    code: &github::DeviceCode,
) -> Result<github::Tokens, String> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(5);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match github::poll(http, client_id, &code.device_code).await {
            Poll::Pending => {}
            // GitHub's rule: back off by five seconds each time it asks.
            Poll::SlowDown => interval += 5,
            Poll::Token(tokens) => return Ok(tokens),
            Poll::Failed(err) => return Err(err),
        }
    }
    Err("The code expired before it was entered. Try again.".into())
}

/// Record the sign-in and publish this machine's public key to the account.
///
/// Returns who was signed in, which is the one thing a caller wants to say
/// out loud afterwards.
pub(crate) async fn finish(
    http: &reqwest::Client,
    account: &AccountState,
    tokens: github::Tokens,
) -> Result<User, String> {
    let token = tokens.access.clone();
    let user = github::user(http, &token).await?;
    // Keep the machine's key across signing in again as the same person, so
    // its entry in the list is replaced rather than joined by a stale one.
    let key = match account.load().await {
        Some(old) if old.user.id == user.id => old.key,
        _ => MachineKey::generate()?,
    };
    let list = github::find_list(http, &token).await?;
    let entry = Entry {
        name: crate::peer::host_name(),
        key: key.public_hex(),
        addresses: crate::reachable_addresses(),
        updated: Some(github::now_unix()),
    };
    let list = github::publish(http, &token, list.as_deref(), &key.id, &entry).await?;
    account
        .save(Some(Account {
            tokens,
            user: user.clone(),
            key,
            list: Some(list),
        }))
        .await;
    account.refresh_list(http).await?;
    Ok(user)
}

/// Forget the sign-in here, and take this machine off the account's list
/// so the others stop trusting it.
pub async fn sign_out(state: &AppState) -> Result<(), String> {
    cancel(state).await;
    let token = state.account.access_token(&state.http).await;
    if let (Some(account), Some(token)) = (state.account.load().await, token) {
        if let Some(list) = &account.list {
            // Best effort: a machine signing out offline should still sign
            // out. Its entry stays until it is removed from GitHub by hand.
            if let Err(err) = github::unpublish(&state.http, &token, list, &account.key.id).await {
                eprintln!("dusk: could not take this machine off the account's list: {err}");
            }
        }
    }
    state.account.save(None).await;
    *state.account.list.write().await = MachineList::default();
    Ok(())
}

async fn blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|err| Err(format!("keystore call did not finish: {err}")))
}

fn read_keystore() -> Result<Option<Stored>, String> {
    match crate::secrets::get(crate::secrets::ACCOUNT)? {
        Some(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

fn write_keystore(stored: &Stored) -> Result<(), String> {
    let raw = serde_json::to_string(stored).map_err(|e| e.to_string())?;
    crate::secrets::set(crate::secrets::ACCOUNT, &raw)
}

fn clear_keystore() -> Result<(), String> {
    crate::secrets::delete(crate::secrets::ACCOUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(key: &MachineKey) -> HashMap<String, Entry> {
        HashMap::from([(
            key.id.clone(),
            Entry {
                name: "Desk".into(),
                key: key.public_hex(),
                addresses: Vec::new(),
                updated: None,
            },
        )])
    }

    fn proof(key: &MachineKey, account: u64, pin: &str) -> Proof {
        Proof {
            account,
            machine: key.id.clone(),
            signature: key.sign_pair(pin),
        }
    }

    #[test]
    fn a_listed_machine_on_the_same_account_is_vouched_for() {
        let key = MachineKey::generate().unwrap();
        assert!(vouch(7, &listed(&key), &proof(&key, 7, "1234"), "1234"));
    }

    #[test]
    fn another_account_is_not_vouched_for_even_with_a_good_signature() {
        let key = MachineKey::generate().unwrap();
        assert!(!vouch(7, &listed(&key), &proof(&key, 8, "1234"), "1234"));
    }

    #[test]
    fn an_unlisted_machine_or_a_replayed_pin_is_not_vouched_for() {
        let key = MachineKey::generate().unwrap();
        let stranger = MachineKey::generate().unwrap();
        assert!(!vouch(
            7,
            &listed(&key),
            &proof(&stranger, 7, "1234"),
            "1234"
        ));
        // A signature lifted from one pairing does not cover another PIN.
        assert!(!vouch(7, &listed(&key), &proof(&key, 7, "1234"), "9999"));
    }
}
