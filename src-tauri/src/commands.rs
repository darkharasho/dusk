//! The frontend's entire surface area.

use std::sync::Arc;

use tauri::{AppHandle, State};

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
