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
pub mod sealed;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};

use crate::state::AppState;
use github::{Entry, Poll, User};
use keys::MachineKey;
use sealed::BoxKey;

pub const ACCOUNT_EVENT: &str = "dusk://account";

/// A machine not on the cached list is looked up again, but not more often
/// than this, so a stream of bogus requests cannot become a stream of calls
/// to GitHub.
const LIST_REFRESH: Duration = Duration::from_secs(10);

/// The keys this machine should sign in with: the ones it already has, or
/// new ones.
///
/// Separated from [`finish`] because the case worth testing — a keystore
/// that will not answer — is unreachable through `finish`, which would need
/// GitHub on the other end first.
async fn identity_for(
    account: &AccountState,
    user_id: u64,
) -> Result<(MachineKey, BoxKey), String> {
    // A keystore that will not answer is not grounds for minting: this
    // machine may well have an identity that simply could not be read, and
    // replacing it orphans its entry on the account and loses every sealed
    // box already addressed to it. Two machines did exactly that before this
    // check existed, each leaving a duplicate behind.
    let existing = account.stored().await.map_err(|err| {
        format!(
            "Dusk could not read whether this machine is already signed in ({err}), \
             so it stopped rather than give the machine a new identity and lose the one it has. \
             Try again once the keystore is reachable."
        )
    })?;
    match existing {
        Some(old) if carries_its_key(Some(old.user.id), user_id) => Ok((old.key, old.box_key)),
        _ => Ok((MachineKey::generate()?, BoxKey::generate()?)),
    }
}

/// Whether a sign-in keeps this machine's existing identity.
///
/// Pure, and separated from the read for one reason: the interesting case is
/// not "same user" but "the keystore would not say", and that is the one a
/// test can reach. Signing in as somebody else is the only honest reason to
/// become a different machine.
fn carries_its_key(existing_user: Option<u64>, signing_in_as: u64) -> bool {
    existing_user == Some(signing_in_as)
}

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
    /// Defaulted, so an entry written before sealing loads and simply mints
    /// a key the next time this machine publishes.
    #[serde(default)]
    box_key: Option<String>,
}

#[derive(Clone)]
struct Account {
    tokens: github::Tokens,
    user: User,
    key: MachineKey,
    /// For receiving sealed addresses. Minted alongside the signing key, and
    /// kept with it: losing one without the other leaves a machine that can
    /// prove who it is but cannot be told anything.
    box_key: sealed::BoxKey,
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
            box_key: Some(self.box_key.to_hex()),
        }
    }

    fn from_stored(s: Stored) -> Result<Self, String> {
        Ok(Self {
            key: MachineKey::from_parts(s.machine, &s.key)?,
            // A machine signed in before sealing has none; minting one here
            // costs nothing and is published the next time it publishes.
            box_key: s
                .box_key
                .as_deref()
                .and_then(sealed::BoxKey::from_hex)
                .map_or_else(sealed::BoxKey::generate, Ok)?,
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

/// How the sign-in is read.
///
/// Injectable for the reason `credentials.rs` gives: the interesting case is
/// a keystore that *fails*, and the only way to reach it in a test is to
/// provide one. It is also what stops a test touching the real keystore and
/// deleting the developer's own sign-in.
type Reader = Arc<dyn Fn() -> Result<Option<Stored>, String> + Send + Sync>;
type Writer = Arc<dyn Fn(Option<&Stored>) -> Result<(), String> + Send + Sync>;

pub struct AccountState {
    cached: RwLock<Cached>,
    list: RwLock<MachineList>,
    /// The sign-in being waited on, if any, so a second press replaces it.
    signing_in: Mutex<Option<tokio::task::AbortHandle>>,
    reader: Reader,
    writer: Writer,
}

impl Default for AccountState {
    fn default() -> Self {
        Self {
            cached: RwLock::default(),
            list: RwLock::default(),
            signing_in: Mutex::default(),
            reader: Arc::new(read_keystore),
            writer: Arc::new(|stored| match stored {
                Some(s) => write_keystore(s),
                None => clear_keystore(),
            }),
        }
    }
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
    /// Best-effort: `None` covers both "nothing stored" and "could not tell".
    ///
    /// Fine for anything that degrades — a grid that shows signed-out, a
    /// pairing that falls back to a PIN. **Not** fine for anything that
    /// would act on the absence; see [`AccountState::stored`].
    async fn load(&self) -> Option<Account> {
        self.stored().await.ok().flatten()
    }

    /// The same read, keeping the distinction the caller may need.
    ///
    /// `Ok(None)` is a settled "this machine has no sign-in". `Err` is "the
    /// keystore would not answer", which is not the same thing and must not
    /// be treated as one: minting a new identity on a failed read orphans
    /// this machine's entry on the account and, since sealing, makes every
    /// box already addressed to it unreadable.
    async fn stored(&self) -> Result<Option<Account>, String> {
        {
            let cached = self.cached.read().await;
            if cached.read {
                return Ok(cached.account.clone());
            }
        }
        let mut cached = self.cached.write().await;
        if cached.read {
            return Ok(cached.account.clone());
        }
        let reader = self.reader.clone();
        match blocking(move || reader()).await {
            Ok(stored) => {
                // A sign-in from before boxes existed has no key to receive
                // them with. One is minted on load — but minting without
                // keeping it is worse than not having one: the machine
                // publishes a different public key every launch and can
                // never open a box addressed to any of them. So a minted key
                // is written back here, once, before anything uses it.
                let minted = stored.as_ref().is_some_and(|s| s.box_key.is_none());
                cached.account = stored.and_then(|s| match Account::from_stored(s) {
                    Ok(account) => Some(account),
                    Err(err) => {
                        eprintln!("dusk: the stored sign-in is unusable ({err}); signed out");
                        None
                    }
                });
                cached.read = true;
                cached.persistent = true;
                if minted {
                    if let Some(account) = cached.account.as_ref().map(Account::to_stored) {
                        let writer = self.writer.clone();
                        if let Err(err) = blocking(move || writer(Some(&account))).await {
                            eprintln!(
                                "dusk: could not keep this machine's box key ({err}); \
                                       addresses from other machines will stay unreadable"
                            );
                        }
                    }
                }
                Ok(cached.account.clone())
            }
            // Deliberately not cached: a failed read is not an answer, or
            // one dismissed keychain prompt would strand the whole run.
            Err(err) => {
                eprintln!("dusk: could not read the sign-in from the keystore ({err})");
                Err(err)
            }
        }
    }

    async fn save(&self, account: Option<Account>) {
        let stored = account.as_ref().map(Account::to_stored);
        let writer = self.writer.clone();
        let written = blocking(move || writer(stored.as_ref())).await;
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

        // Everyone on the account that published a key to receive with,
        // ourselves included: this machine reads its own entry back like any
        // other, and a machine that cannot receive is skipped rather than
        // holding up the rest.
        let recipients: Vec<(String, String)> = {
            let list = self.list.read().await;
            let mut r: Vec<(String, String)> = list
                .entries
                .iter()
                .filter_map(|(id, e)| e.box_key.clone().map(|k| (id.clone(), k)))
                .collect();
            if !r.iter().any(|(id, _)| id == &account.key.id) {
                r.push((account.key.id.clone(), account.box_key.public_hex()));
            }
            r
        };

        // Republished when the addresses change, or when someone new joined
        // the account and has no box of ours yet. Never on a timer: that
        // would be a heartbeat, and a heartbeat is a rendezvous service with
        // extra steps.
        let (published, addressed): (Vec<String>, usize) = {
            let list = self.list.read().await;
            match list.entries.get(&account.key.id) {
                Some(e) => (
                    e.sealed
                        .as_ref()
                        .and_then(|env| sealed::open(env, &account.box_key, &account.key.id))
                        .and_then(|raw| serde_json::from_slice::<Vec<String>>(&raw).ok())
                        .unwrap_or_default(),
                    e.sealed.as_ref().map_or(0, |env| env.to.len()),
                ),
                None => (Vec::new(), 0),
            }
        };
        if published == here && addressed == recipients.len() {
            return;
        }

        let payload = match serde_json::to_vec(&here) {
            Ok(payload) => payload,
            Err(err) => {
                eprintln!("dusk: could not prepare this machine's addresses: {err}");
                return;
            }
        };
        let envelope = match sealed::seal(&payload, &recipients) {
            Ok(envelope) => envelope,
            Err(err) => {
                eprintln!("dusk: could not seal this machine's addresses: {err}");
                return;
            }
        };

        let entry = Entry {
            name: crate::peer::host_name(),
            key: account.key.public_hex(),
            // Never written in the clear again; only read, for entries
            // published before sealing existed.
            addresses: Vec::new(),
            box_key: Some(account.box_key.public_hex()),
            sealed: Some(envelope),
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

    /// What a machine said about where it answers, read back.
    ///
    /// Sealed first; the plaintext field is only ever read, for entries
    /// published before sealing existed. Signed out, or with no box for this
    /// machine, there is nothing to read and the grid falls back to whatever
    /// discovery found — which is what it did before any of this.
    fn addresses_from(&self, entry: &Entry, account: Option<&Account>) -> Vec<String> {
        if let Some(account) = account {
            if let Some(envelope) = &entry.sealed {
                if let Some(raw) = sealed::open(envelope, &account.box_key, &account.key.id) {
                    if let Ok(addresses) = serde_json::from_slice::<Vec<String>>(&raw) {
                        return addresses;
                    }
                }
            }
        }
        entry.addresses.clone()
    }

    /// Every machine on the account's list, as `(id, name)`.
    ///
    /// The grid's source of truth for "yours". Empty when signed out or
    /// before the list has been read, which the caller must treat as "not
    /// known yet" rather than as "you own nothing".
    pub async fn machines_on_account(&self) -> Vec<AccountMachine> {
        let account = self.load().await;
        self.list
            .read()
            .await
            .entries
            .iter()
            .map(|(id, entry)| AccountMachine {
                id: id.clone(),
                name: entry.name.clone(),
                addresses: self.addresses_from(entry, account.as_ref()),
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

    /// Forget what was read, so the next read goes to the keystore again.
    ///
    /// The window reads the sign-in once and keeps it for the run, which is
    /// right: a keystore read can put a password prompt on screen. But it
    /// means a sign-in that happens *elsewhere* — `dusk signin` over SSH, or
    /// the other half of this app — is invisible to a window already open,
    /// which kept using a token GitHub had stopped accepting and refused
    /// every one of its own machines as "not the same account". Observed on
    /// a real machine, and the only clue was a line in a log nobody reads.
    async fn reread(&self) {
        let mut cached = self.cached.write().await;
        cached.read = false;
        cached.account = None;
    }

    async fn refresh_list(&self, http: &reqwest::Client) -> Result<(), String> {
        match self.fetch_list(http).await {
            // Not a retry for its own sake: the token being refused is
            // exactly the case where the keystore may already hold a better
            // one. Once only, so a genuinely dead sign-in still ends as an
            // error rather than a loop.
            Err(err) if github::is_rejected(&err) => {
                self.reread().await;
                self.fetch_list(http).await
            }
            other => other,
        }
    }

    async fn fetch_list(&self, http: &reqwest::Client) -> Result<(), String> {
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
    let (key, box_key) = identity_for(account, user.id).await?;

    let list = github::find_list(http, &token).await?;
    let entry = Entry {
        name: crate::peer::host_name(),
        key: key.public_hex(),
        addresses: Vec::new(),
        box_key: Some(box_key.public_hex()),
        sealed: None,
        updated: Some(github::now_unix()),
    };
    let list = github::publish(http, &token, list.as_deref(), &key.id, &entry).await?;
    account
        .save(Some(Account {
            tokens,
            user: user.clone(),
            key,
            box_key,
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
mod identity_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn user(id: u64) -> User {
        User {
            id,
            login: "someone".into(),
            avatar_url: String::new(),
        }
    }

    fn an_account(user_id: u64) -> Stored {
        Account {
            tokens: github::Tokens {
                access: "gho_x".into(),
                refresh: None,
                expires_at: None,
            },
            user: user(user_id),
            key: MachineKey::generate().unwrap(),
            box_key: BoxKey::generate().unwrap(),
            list: None,
        }
        .to_stored()
    }

    /// An `AccountState` whose keystore is whatever the test says it is.
    fn with_keystore(
        reader: impl Fn() -> Result<Option<Stored>, String> + Send + Sync + 'static,
    ) -> AccountState {
        AccountState {
            reader: Arc::new(reader),
            // Writes go nowhere: a test must never reach the real keystore,
            // which would delete the developer's own sign-in.
            writer: Arc::new(|_| Ok(())),
            ..Default::default()
        }
    }

    #[test]
    fn signing_in_again_as_yourself_keeps_this_machine() {
        assert!(carries_its_key(Some(42), 42));
    }

    #[test]
    fn signing_in_as_somebody_else_makes_a_new_machine() {
        assert!(!carries_its_key(Some(42), 7));
    }

    #[test]
    fn a_machine_with_no_sign_in_mints_one() {
        assert!(!carries_its_key(None, 42));
    }

    #[tokio::test]
    async fn a_keystore_that_will_not_answer_stops_the_sign_in() {
        // The bug, which bit twice before it was found: `load` flattens
        // "nothing stored" and "could not read" into None, and `finish` used
        // to mint on either. So one dismissed keychain prompt or a locked
        // keyring gave the machine a new identity, orphaned its entry on the
        // account, and -- since sealing -- lost every box addressed to it.
        let state = with_keystore(|| Err("keyring is locked".into()));
        let err = identity_for(&state, 42)
            .await
            .expect_err("an unreadable keystore must not mint");
        assert!(err.contains("keyring is locked"), "{err}");
        assert!(err.contains("new identity"), "it must say what it avoided");
    }

    #[tokio::test]
    async fn a_machine_that_has_never_signed_in_gets_an_identity() {
        // The other half: Ok(None) is a settled answer and minting is right.
        let state = with_keystore(|| Ok(None));
        assert!(identity_for(&state, 42).await.is_ok());
    }

    #[tokio::test]
    async fn signing_in_again_reuses_both_keys() {
        // Both, together. Carrying one without the other leaves a machine
        // that can prove who it is and cannot be told anything.
        let stored = an_account(42);
        let want = Account::from_stored(stored.clone()).unwrap();
        let state = with_keystore(move || Ok(Some(stored.clone())));

        let (key, box_key) = identity_for(&state, 42).await.unwrap();
        assert_eq!(key.id, want.key.id, "the signing key is kept");
        assert_eq!(
            box_key.public_hex(),
            want.box_key.public_hex(),
            "and the key that boxes are addressed to"
        );
    }

    #[tokio::test]
    async fn signing_in_as_a_different_account_does_not_reuse_them() {
        let stored = an_account(42);
        let had = Account::from_stored(stored.clone()).unwrap();
        let state = with_keystore(move || Ok(Some(stored.clone())));

        let (key, box_key) = identity_for(&state, 7).await.unwrap();
        assert_ne!(key.id, had.key.id);
        assert_ne!(box_key.public_hex(), had.box_key.public_hex());
    }

    #[tokio::test]
    async fn a_failed_read_is_not_remembered_as_an_answer() {
        // Otherwise one dismissed prompt strands the whole run believing
        // there is no sign-in -- the exact wording credentials.rs uses, and
        // the property that makes refusing safe rather than sticky.
        static READS: AtomicUsize = AtomicUsize::new(0);
        READS.store(0, Ordering::SeqCst);
        let state = with_keystore(|| {
            READS.fetch_add(1, Ordering::SeqCst);
            Err("still locked".into())
        });

        assert!(identity_for(&state, 42).await.is_err());
        assert!(identity_for(&state, 42).await.is_err());
        assert_eq!(
            READS.load(Ordering::SeqCst),
            2,
            "the keystore is asked again"
        );
    }

    #[tokio::test]
    async fn a_box_key_minted_on_load_is_kept() {
        // Minting without keeping is worse than not having one: the machine
        // publishes a different public key every launch and can never open
        // a box addressed to any of them. Observed on a real machine whose
        // sign-in predated sealing.
        let stored = {
            let mut s = an_account(42);
            s.box_key = None;
            s
        };
        let written: Arc<std::sync::Mutex<Option<Stored>>> = Arc::default();
        let sink = written.clone();
        let state = AccountState {
            reader: Arc::new(move || Ok(Some(stored.clone()))),
            writer: Arc::new(move |s| {
                *sink.lock().unwrap() = s.cloned();
                Ok(())
            }),
            ..Default::default()
        };

        let loaded = state.stored().await.unwrap().expect("an account");
        let kept = written
            .lock()
            .unwrap()
            .clone()
            .expect("it was written back");
        assert_eq!(
            kept.box_key.as_deref(),
            Some(loaded.box_key.to_hex()).as_deref(),
            "the key written back is the one in use"
        );
    }

    #[tokio::test]
    async fn a_stored_box_key_is_not_rewritten() {
        // Only the machine that has none needs the write; doing it every
        // load would touch the keystore on every launch for nothing.
        let stored = an_account(42);
        let written: Arc<std::sync::Mutex<bool>> = Arc::default();
        let sink = written.clone();
        let state = AccountState {
            reader: Arc::new(move || Ok(Some(stored.clone()))),
            writer: Arc::new(move |_| {
                *sink.lock().unwrap() = true;
                Ok(())
            }),
            ..Default::default()
        };
        state.stored().await.unwrap();
        assert!(!*written.lock().unwrap(), "nothing to write");
    }

    #[tokio::test]
    async fn a_rejected_token_makes_the_next_read_go_to_the_keystore() {
        // The window reads the sign-in once per run, which is right -- a
        // keystore read can put a prompt on screen. But a sign-in that
        // happens elsewhere (`dusk signin` over SSH) was then invisible to
        // an open window, which kept using a token GitHub had stopped
        // accepting and refused every one of its own machines. Observed on
        // a real machine; the only clue was a log line nobody reads.
        static READS: AtomicUsize = AtomicUsize::new(0);
        READS.store(0, Ordering::SeqCst);
        let first = an_account(42);
        let second = an_account(42);
        let state = with_keystore(move || {
            let n = READS.fetch_add(1, Ordering::SeqCst);
            Ok(Some(if n == 0 {
                first.clone()
            } else {
                second.clone()
            }))
        });

        let before = state.load().await.expect("an account").key.id;
        assert_eq!(
            state.load().await.unwrap().key.id,
            before,
            "cached for the run"
        );

        state.reread().await;
        let after = state.load().await.expect("an account").key.id;
        assert_ne!(after, before, "the keystore is consulted again");
        assert_eq!(READS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_refused_token_is_recognised_without_matching_prose() {
        assert!(github::is_rejected(github::REJECTED));
        assert!(!github::is_rejected("GitHub answered 500: no reason given"));
    }

    #[tokio::test]
    async fn load_still_flattens_for_callers_that_degrade() {
        // `load` is deliberately lossy and that is fine for a grid showing
        // signed-out or a pairing falling back to a PIN. The point is that
        // the decision above no longer uses it.
        let state = with_keystore(|| Err("locked".into()));
        assert!(state.load().await.is_none());
    }
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
                box_key: None,
                sealed: None,
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
