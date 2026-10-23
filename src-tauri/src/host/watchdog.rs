//! Restart a host whose pairing port has died under it.
//!
//! Sunshine serves pairing, launch, resume and quit from one HTTPS thread.
//! When a launch or resume blocks in there — seen on a Mac asked to start a
//! stream with its screen asleep — that thread never comes back: the plain
//! port keeps answering, the web port keeps answering, and every handshake
//! on the pairing port is reset. From outside the machine reads as up and
//! busy, every client sits on "checking whether this machine is paired",
//! and nothing a client can send gets through, since quitting the session
//! goes through the same dead port. It stayed that way for a day.
//!
//! Only the host can mend it, and only by restarting: Sunshine's own restart
//! request is served by the thread that is stuck. So Dusk on the host
//! watches its own pairing port and, once it has plainly died, stops and
//! starts the service.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::model::{HostStatus, DEFAULT_HTTPS_PORT, DEFAULT_HTTP_PORT};
use crate::serverinfo::{self, QueryError};
use crate::state::AppState;

/// Between looks. Each is two requests on the loopback, and the cost of
/// looking slowly is that a stuck host stays stuck that much longer.
const TICK: Duration = Duration::from_secs(30);

/// Looks in a row on which the plain port answered and the pairing port did
/// not, before the host is restarted. A host aborts the odd handshake, and
/// a tick is ninety seconds of nobody able to reach it; a stream that was
/// live would have been launched through the very port that is not
/// answering, so there is nothing to protect by waiting longer.
const STRIKES: u8 = 3;

/// Between restarts. A host that comes back wedged is not fixed by being
/// restarted every ninety seconds, and someone may be looking at it.
const COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// What one look at the host's own ports showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Both ports answered, or the plain one did not: nothing is wedged,
    /// or nothing is running to be wedged.
    Fine,
    /// The plain port answered and the pairing port completed no handshake.
    PairingPortDead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Wait,
    Restart,
}

/// The decision, kept apart from the looking so it can be tested.
#[derive(Debug, Default)]
pub struct Watchdog {
    strikes: u8,
    restarted_at: Option<Instant>,
}

impl Watchdog {
    pub fn observe(&mut self, seen: Seen, now: Instant) -> Verdict {
        match seen {
            Seen::Fine => {
                self.strikes = 0;
                Verdict::Wait
            }
            Seen::PairingPortDead => {
                self.strikes = self.strikes.saturating_add(1);
                if self.strikes < STRIKES {
                    return Verdict::Wait;
                }
                let cooling = self
                    .restarted_at
                    .is_some_and(|at| now.duration_since(at) < COOLDOWN);
                if cooling {
                    return Verdict::Wait;
                }
                self.strikes = 0;
                self.restarted_at = Some(now);
                Verdict::Restart
            }
        }
    }
}

pub fn spawn(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let Some(tls) = crate::state::loopback_tls_client() else {
            eprintln!("dusk: cannot watch hosting's pairing port: no TLS client");
            return;
        };
        let mut watchdog = Watchdog::default();
        loop {
            tokio::time::sleep(TICK).await;
            let seen = look(&state.http, &tls).await;
            if watchdog.observe(seen, Instant::now()) == Verdict::Restart {
                restart(&state).await;
            }
        }
    });
}

/// One look at both ports on the loopback.
///
/// The pairing port is asked with a certificate it has never seen, which a
/// healthy host answers with its "not authorized" document: any answer at
/// all means the thread behind it is alive. Only no answer — a reset, a
/// handshake that never completes, a timeout — counts.
async fn look(http: &reqwest::Client, tls: &reqwest::Client) -> Seen {
    if serverinfo::query_http(http, "127.0.0.1", DEFAULT_HTTP_PORT)
        .await
        .is_err()
    {
        return Seen::Fine;
    }
    match serverinfo::query_https(tls, "127.0.0.1", DEFAULT_HTTPS_PORT).await {
        Err(QueryError::Unreachable(_)) => Seen::PairingPortDead,
        Ok(_) | Err(QueryError::Answered(_)) => Seen::Fine,
    }
}

/// Stop and start the service, rather than ask the host to restart itself:
/// the thread that would serve that request is the one that is stuck.
async fn restart(state: &AppState) {
    // Something answering on the hosting ports that Dusk is not running is
    // not Dusk's to restart.
    if !matches!(
        state.host.probe().await,
        Ok(HostStatus::Installed { running: true, .. })
    ) {
        eprintln!(
            "dusk: hosting's pairing port is not answering, but hosting is not running under Dusk here, so it is left alone"
        );
        return;
    }
    eprintln!(
        "dusk: hosting answers on its plain port but its pairing port has completed no handshake for {}s; restarting hosting",
        TICK.as_secs() * u64::from(STRIKES)
    );
    if let Err(err) = state.host.stop().await {
        // Worth going on to the start: a stop that reported a failure may
        // still have taken the process down.
        eprintln!("dusk: stopping hosting to restart it: {err}");
    }
    match state.host.start().await {
        Ok(()) => {
            eprintln!("dusk: hosting restarted");
            state.request_refresh();
        }
        Err(err) => eprintln!("dusk: could not start hosting again: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_port_dead_for_three_looks_restarts_the_host_and_one_look_does_not() {
        let mut dog = Watchdog::default();
        let start = Instant::now();
        let at = |n: u64| start + TICK * n as u32;
        assert_eq!(dog.observe(Seen::PairingPortDead, at(0)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::PairingPortDead, at(1)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::PairingPortDead, at(2)), Verdict::Restart);
    }

    #[test]
    fn one_answered_handshake_starts_the_count_over() {
        let mut dog = Watchdog::default();
        let start = Instant::now();
        let at = |n: u64| start + TICK * n as u32;
        assert_eq!(dog.observe(Seen::PairingPortDead, at(0)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::PairingPortDead, at(1)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::Fine, at(2)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::PairingPortDead, at(3)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::PairingPortDead, at(4)), Verdict::Wait);
        assert_eq!(dog.observe(Seen::PairingPortDead, at(5)), Verdict::Restart);
    }

    #[test]
    fn a_host_that_comes_back_wedged_is_not_restarted_again_for_ten_minutes() {
        let mut dog = Watchdog::default();
        let start = Instant::now();
        for _ in 0..STRIKES {
            dog.observe(Seen::PairingPortDead, start);
        }
        // Still dead on every look after: the strikes add up again, but
        // the restart waits for the cooldown.
        let mut verdicts = Vec::new();
        for n in 1..=((COOLDOWN.as_secs() / TICK.as_secs()) as u32) {
            verdicts.push(dog.observe(Seen::PairingPortDead, start + TICK * n));
        }
        let restarts = verdicts.iter().filter(|v| **v == Verdict::Restart).count();
        assert_eq!(restarts, 1, "{verdicts:?}");
        assert_eq!(verdicts.last(), Some(&Verdict::Restart));
    }
}
