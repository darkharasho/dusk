//! The frontend's entire surface area.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::install;
use crate::model::{Device, Snapshot};
use crate::registry::split_address;
use crate::state::{emit_snapshot, AppState};
use crate::store::ManualEntry;
use crate::sunshine::{self, Credentials, SunshineApi};

#[tauri::command]
pub async fn get_snapshot(state: State<'_, Arc<AppState>>) -> Result<Snapshot, String> {
    Ok(state.snapshot().await)
}

#[tauri::command]
pub async fn add_manual_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    address: String,
    name: Option<String>,
) -> Result<Device, String> {
    let (host, port) = split_address(&address)?;
    let name = name.and_then(|n| {
        let trimmed = n.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });

    let id = {
        let mut registry = state.registry.write().await;
        registry.upsert_manual(&host, port, name.clone())
    };

    {
        let mut store = state.store.lock().await;
        store.upsert(ManualEntry {
            address: host,
            port,
            name,
        });
        store.save(&state.store_path)?;
    }

    // Probe the new machine straight away so its card is not stuck on
    // "Checking" until the next tick.
    state.request_refresh();

    let device = state
        .registry
        .read()
        .await
        .devices()
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| "Machine was added but could not be read back.".to_string())?;

    let inner = state.inner().clone();
    emit_snapshot(&app, &inner).await;
    Ok(device)
}

#[tauri::command]
pub async fn remove_manual_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let addresses = {
        let registry = state.registry.read().await;
        registry
            .devices()
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| (d.addresses.clone(), d.http_port))
    };

    state.registry.write().await.remove_manual(&id)?;

    if let Some((addresses, port)) = addresses {
        let mut store = state.store.lock().await;
        for address in addresses {
            // The stored port is None when the user accepted the default, so
            // clear both spellings of the same entry.
            store.remove(&address, Some(port));
            store.remove(&address, None);
        }
        store.save(&state.store_path)?;
    }

    let inner = state.inner().clone();
    emit_snapshot(&app, &inner).await;
    Ok(())
}

#[tauri::command]
pub async fn refresh_now(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.request_refresh();
    Ok(())
}

/// The address to reach a device on, and its human name for messages.
async fn target_of(state: &Arc<AppState>, id: &str) -> Result<(String, String), String> {
    let registry = state.registry.read().await;
    let device = registry
        .devices()
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| "That machine is no longer in the list.".to_string())?;

    let address = device
        .primary_address
        .or_else(|| device.addresses.first().cloned())
        .ok_or_else(|| format!("Dusk has no address for {}.", device.name))?;

    Ok((address, device.name))
}

fn moonlight(state: &Arc<AppState>) -> Result<&crate::moonlight::Moonlight, String> {
    state
        .moonlight
        .as_ref()
        .ok_or_else(|| "Moonlight is not installed, or Dusk could not find it.".to_string())
}

/// Pair with a host.
///
/// Dusk sends the PIN; the person has to type the same one into the host.
/// That second half is Sunshine's web page today and becomes a native screen
/// in M3, which is where this flow stops being two apps.
#[tauri::command]
pub async fn pair_device(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    pin: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    moonlight(&inner)?
        .pair(&address, &pin)
        .await
        .map_err(|e| e.to_string())?;

    // Pairing may have just minted an identity where there was none, so the
    // TLS client has to be rebuilt before the next probe can see the change.
    inner.reload_identity();
    inner.request_refresh();
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Start streaming an app, and watch the session for the card's sake.
#[tauri::command]
pub async fn launch_app(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    app_id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    let app_name = {
        let registry = inner.registry.read().await;
        registry
            .devices()
            .into_iter()
            .find(|d| d.id == id)
            .and_then(|d| d.apps.iter().find(|a| a.id == app_id).map(|a| a.name.clone()))
            .ok_or_else(|| "That app is no longer on this machine.".to_string())?
    };

    // moonlight-qt takes the app's title, not its id.
    let child = moonlight(&inner)?
        .stream(&address, &app_name, &Default::default())
        .map_err(|e| e.to_string())?;

    // The session ending is a state change the grid has to notice, and
    // nothing else will tell us — so wait on the child and refresh when it
    // exits rather than leaving the card mid-session until the next poll.
    let handle = app.clone();
    let watched = inner.clone();
    tauri::async_runtime::spawn(async move {
        let mut child = child;
        if let Err(err) = child.wait().await {
            eprintln!("dusk: lost track of the Moonlight session: {err}");
        }
        watched.request_refresh();
        emit_snapshot(&handle, &watched).await;
    });

    inner.request_refresh();
    Ok(())
}

// ------------------------------------------------------------- host side

/// Turn hosting on for this machine.
#[tauri::command]
pub async fn start_hosting(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    inner.host.start().await.map_err(|e| e.to_string())?;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Turn hosting off for this machine.
#[tauri::command]
pub async fn stop_hosting(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    inner.host.stop().await.map_err(|e| e.to_string())?;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Store this machine's Sunshine sign-in, after checking it actually works.
///
/// Verified before saving so a typo surfaces here rather than as a confusing
/// failure the first time someone tries to accept a PIN.
#[tauri::command]
pub async fn sign_in_host(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    username: String,
    password: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let credentials = Credentials { username, password };

    SunshineApi::new(sunshine::api::DEFAULT_PORT)
        .map_err(|e| e.to_string())?
        .verify(&credentials)
        .await
        .map_err(|e| e.to_string())?;

    inner.credentials.save(credentials).await?;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

#[tauri::command]
pub async fn sign_out_host(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let inner = state.inner().clone();
    inner.credentials.forget().await;
    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Accept a pairing PIN on this machine.
///
/// This is the half of pairing that is Sunshine's web page today, and doing
/// it here is the single biggest reason Dusk stops feeling like two
/// programs.
#[tauri::command]
pub async fn accept_pin(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    pin: String,
    device_name: Option<String>,
) -> Result<(), String> {
    let inner = state.inner().clone();

    let pin = pin.trim().to_string();
    if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err("A pairing PIN is four digits.".into());
    }

    let credentials = inner
        .credentials
        .load()
        .await
        .ok_or_else(|| sunshine::ApiError::NoCredentials.to_string())?;

    let name = device_name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Dusk".to_string());

    SunshineApi::new(sunshine::api::DEFAULT_PORT)
        .map_err(|e| e.to_string())?
        .submit_pin(&credentials, &pin, &name)
        .await
        .map_err(|e| e.to_string())?;

    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// The first-run checklist for this machine.
#[tauri::command]
pub async fn get_setup(state: State<'_, Arc<AppState>>) -> Result<install::Setup, String> {
    let inner = state.inner().clone();
    let host = inner.host.state().await;
    Ok(install::steps::build(
        host.platform,
        &host.status,
        host.capabilities.support_tier,
        inner.credentials.load().await.is_some(),
    ))
}

/// Which Sunshine build this machine would get, without downloading it.
///
/// Separate from installing on purpose: it is worth being able to see the
/// version, the size and whether a checksum exists before committing to a
/// download that will be run with elevated rights.
#[tauri::command]
pub async fn preview_sunshine_download(
    state: State<'_, Arc<AppState>>,
) -> Result<DownloadPreview, String> {
    let inner = state.inner().clone();
    let release = install::release::fetch_latest(&inner.http).await?;

    let choice = install::release::select(
        &release.assets,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
    .ok_or_else(|| {
        format!(
            "Sunshine does not publish a build for {} on {}.",
            std::env::consts::ARCH,
            std::env::consts::OS
        )
    })?;

    Ok(DownloadPreview {
        version: release.version,
        asset: choice.asset.name.clone(),
        size: choice.asset.size,
        verifiable: choice.asset.sha256.is_some(),
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPreview {
    pub version: String,
    pub asset: String,
    pub size: u64,
    /// False when the release carries no checksum, in which case Dusk
    /// refuses to install rather than running an unverifiable binary.
    pub verifiable: bool,
}

/// Progress events while a release downloads.
pub const DOWNLOAD_EVENT: &str = "dusk://download";

/// Download the right Sunshine build and install it.
///
/// Both halves are real state changes on the machine, so this only ever runs
/// from an explicit press — never on a timer, and never as part of a probe.
#[tauri::command]
pub async fn install_sunshine(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let release = install::release::fetch_latest(&inner.http).await?;

    let choice = install::release::select(
        &release.assets,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
    .ok_or_else(|| {
        format!(
            "Sunshine does not publish a build for {} on {}.",
            std::env::consts::ARCH,
            std::env::consts::OS
        )
    })?;

    // Downloads land in the app's own cache directory rather than a shared
    // temp path: a verified installer briefly sitting somewhere world
    // writable is a place to swap it before it runs.
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("downloads");

    let emitter = app.clone();
    let path = install::download::fetch(&inner.http, &choice.asset, &dir, move |progress| {
        let _ = emitter.emit(DOWNLOAD_EVENT, progress);
    })
    .await
    .map_err(|e| e.to_string())?;

    let outcome = install::apply::install(&path, choice.kind)
        .await
        .map_err(|e| e.to_string());

    // The installer is a verified copy of a public release, not a secret,
    // but there is no reason to leave 40MB behind either.
    let _ = tokio::fs::remove_file(&path).await;
    outcome?;

    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Add firewall rules for Sunshine. Windows only; elevates when it runs.
#[tauri::command]
pub async fn open_firewall() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let program = std::env::var_os("ProgramFiles")
            .map(|base| std::path::PathBuf::from(base).join("Sunshine\\sunshine.exe"))
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                "Dusk could not find sunshine.exe, so it cannot add a rule for it.".to_string()
            })?;
        install::apply::open_firewall(&program)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Dusk only manages firewall rules on Windows.".into())
    }
}

/// Open the system screen where a permission Dusk cannot grant is granted.
///
/// macOS TCC settings cannot be set by any installer, so the most Dusk can
/// do is put the right pane in front of someone.
#[tauri::command]
pub async fn open_privacy_settings(pane: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let anchor = match pane.as_str() {
            "screenRecording" => "Privacy_ScreenCapture",
            "accessibility" => "Privacy_Accessibility",
            _ => return Err("Dusk does not know that settings pane.".into()),
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        let out = crate::host::service::run("open", &[&url])
            .await
            .map_err(|e| e.to_string())?;
        if !out.ok() {
            return Err(out.message());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("This only applies to macOS.".into())
    }
}

async fn host_api(
    state: &Arc<AppState>,
) -> Result<(SunshineApi, Credentials), String> {
    let credentials = state
        .credentials
        .load()
        .await
        .ok_or_else(|| sunshine::ApiError::NoCredentials.to_string())?;
    let api = SunshineApi::new(sunshine::api::DEFAULT_PORT).map_err(|e| e.to_string())?;
    Ok((api, credentials))
}

/// Read this machine's Sunshine configuration.
#[tauri::command]
pub async fn get_host_config(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let inner = state.inner().clone();
    let (api, credentials) = host_api(&inner).await?;
    api.get_config(&credentials).await.map_err(|e| e.to_string())
}

/// Write changed settings, and optionally restart so they take effect.
#[tauri::command]
pub async fn save_host_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    changes: serde_json::Map<String, serde_json::Value>,
    restart: bool,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (api, credentials) = host_api(&inner).await?;

    api.save_config(&credentials, changes)
        .await
        .map_err(|e| e.to_string())?;

    if restart {
        api.restart(&credentials).await.map_err(|e| e.to_string())?;
    }

    emit_snapshot(&app, &inner).await;
    Ok(())
}

/// Ask the host to end whatever session is running on it.
#[tauri::command]
pub async fn quit_session(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    let (address, _) = target_of(&inner, &id).await?;

    moonlight(&inner)?
        .quit(&address)
        .await
        .map_err(|e| e.to_string())?;

    inner.request_refresh();
    emit_snapshot(&app, &inner).await;
    Ok(())
}
