//! Dusk's own output, kept on disk where the window system throws it away.
//!
//! Every account Dusk gives of itself — the client it launched, a clipboard
//! connection ending, the host's focused window taking the pointer — is an
//! `eprintln!`. On Linux that lands in the journal, and on 2026-11-11 that
//! journal is what explained a failure nobody saw happen: the host's side of
//! a stream that never started was all there, stamped, an hour later.
//!
//! The same failure on macOS was unreadable. An app launched from Finder has
//! a stderr that goes nowhere at all, so the only record of what Dusk did was
//! whatever was still on screen — and the thing on screen was a stale frame
//! of a window behind three others. Two incidents the same day were diagnosed
//! from the *client's* log and the *host's* journal, with Dusk's own account
//! of both missing.
//!
//! So on macOS, stderr is pointed at a file beside the stream logs.
//!
//! # Only macOS, and only when nobody is watching
//!
//! Not Linux: the journal already has this, which is the whole reason the
//! Linux side was diagnosable, and redirecting would take it away. Not
//! Windows: a release build there has no console, so this would be worth
//! doing, but nothing has gone missing there yet and a file that nobody has
//! needed is a file nobody will prune.
//!
//! And not when stderr is a terminal: someone running Dusk from a shell to
//! watch it is the one case where the output is already where it belongs.

#![cfg(target_os = "macos")]

use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// Past this, the log is rotated to `.old` and started again. Two files of
/// this size is the whole budget: enough to hold a long session and the one
/// before it, small enough that nobody has to think about it.
const ROTATE_AT: u64 = 4 * 1024 * 1024;

/// Where Dusk's own log lives: beside the stream logs, which is where
/// somebody looking for one would look for the other.
pub fn path() -> Option<PathBuf> {
    crate::moonlight::stream_log::dir().map(|streams| {
        streams
            .parent()
            .map(|base| base.to_path_buf())
            .unwrap_or(streams)
            .join("dusk.log")
    })
}

/// Point stderr and stdout at the log file for the rest of the process.
///
/// Called once, early, from the path that opens a window — never from the
/// `dusk status` and `dusk setup` paths, whose output is for whoever typed
/// them and may well be piped somewhere.
///
/// Failure is silent and harmless: a Dusk that cannot open its log runs
/// exactly as it did before this existed.
pub fn to_file() {
    if std::io::stderr().is_terminal() {
        return;
    }

    let Some(path) = path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }

    // Rotated before opening rather than while writing: the only moment the
    // size is worth checking is the only moment nothing is writing yet.
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= ROTATE_AT) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }

    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    // `dup2` rather than a logging crate: what has to be captured is every
    // `eprintln!` already in the tree, including the ones in libraries and
    // the panic handler's, and the only thing they agree on is the file
    // descriptor. After this, fd 1 and 2 are copies of the file's — the
    // original is dropped at the end of the function, which closes nothing
    // that matters.
    //
    // SAFETY: both targets are the standard descriptors, which exist for the
    // life of the process, and `file` is open until the end of this scope.
    unsafe {
        libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
        libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO);
    }

    // A marker, so a log holding several runs can be cut into them. The
    // version is here because the first question asked of any of these is
    // which Dusk wrote it, and the stamp is seconds since the epoch for the
    // same reason the stream logs use them: `journalctl --since @<seconds>`
    // lines the two machines up without anybody doing arithmetic.
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    eprintln!(
        "\ndusk: --- {} starting, @{at} ---",
        env!("CARGO_PKG_VERSION")
    );
}
