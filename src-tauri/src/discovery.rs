//! mDNS discovery of Sunshine hosts.
//!
//! Sunshine advertises the GameStream service type, the same one Moonlight
//! browses for, so this finds exactly the machines Moonlight would find. It
//! will not find a machine across a VPN — multicast rarely survives the hop —
//! which is why the manual address book is a peer of this, not a fallback.

use std::sync::Arc;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tauri::AppHandle;

use crate::state::{emit_snapshot, AppState};

pub const SERVICE_TYPE: &str = "_nvstream._tcp.local.";

/// How long the window says it is looking. Browsing never stops, so
/// "looking" means the stretch in which a machine on the network would have
/// answered: the first queries go out within a second or two, and a host
/// answers at once.
const SWEEP: Duration = Duration::from_secs(6);

/// Show the window looking for a while, if there is anything to look with.
pub fn sweep(app: &AppHandle, state: &Arc<AppState>) {
    if !state.is_browsing() {
        return;
    }
    let ticket = state.begin_sweep();
    let (app, state) = (app.clone(), state.clone());
    tauri::async_runtime::spawn(async move {
        emit_snapshot(&app, &state).await;
        tokio::time::sleep(SWEEP).await;
        if state.end_sweep(ticket) {
            emit_snapshot(&app, &state).await;
        }
    });
}

pub fn spawn(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let daemon = match ServiceDaemon::new() {
            Ok(daemon) => daemon,
            Err(err) => {
                eprintln!("dusk: mDNS unavailable, manual entries only: {err}");
                emit_snapshot(&app, &state).await;
                return;
            }
        };

        let receiver = match daemon.browse(SERVICE_TYPE) {
            Ok(receiver) => receiver,
            Err(err) => {
                eprintln!("dusk: could not browse {SERVICE_TYPE}: {err}");
                emit_snapshot(&app, &state).await;
                return;
            }
        };

        state.set_browsing(true);
        sweep(&app, &state);

        while let Ok(event) = receiver.recv_async().await {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let addresses: Vec<String> = info
                        .get_addresses()
                        .iter()
                        .map(|ip| ip.to_string())
                        .collect();
                    if addresses.is_empty() {
                        continue;
                    }

                    let name = instance_name(info.get_fullname());
                    {
                        let mut registry = state.registry.write().await;
                        registry.upsert_mdns(&name, &addresses, info.get_port());
                    }
                    // A newly seen machine should be probed now rather than at
                    // the next tick, so its card lights up immediately.
                    state.request_refresh();
                }
                ServiceEvent::SearchStopped(_) => break,
                _ => {}
            }
        }

        state.set_browsing(false);
        emit_snapshot(&app, &state).await;
    });
}

/// Pull the instance label out of `NAME._nvstream._tcp.local.`.
///
/// Escaped dots (`\.`) are legal inside an instance label, so the split has to
/// respect the escape rather than taking the first dot.
fn instance_name(fullname: &str) -> String {
    let mut label = String::new();
    let mut chars = fullname.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    label.push(escaped);
                }
            }
            '.' => break,
            _ => label.push(c),
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_labels_come_off_the_front() {
        assert_eq!(instance_name("WORKSHOP._nvstream._tcp.local."), "WORKSHOP");
    }

    #[test]
    fn escaped_dots_stay_part_of_the_label() {
        assert_eq!(
            instance_name("Ana\\.s PC._nvstream._tcp.local."),
            "Ana.s PC"
        );
    }
}
