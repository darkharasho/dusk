//! A stream's own account of itself, kept on disk.
//!
//! On Linux the client logs to stderr rather than to a file, and Dusk kept
//! only the last few lines of that in memory. So when a stream took the whole
//! machine down with it, the one record of what the client was doing went
//! with the reboot. Each session is now written to the state directory as it
//! runs and synced about once a second: a hard freeze costs at most the last
//! second, where waiting on the page cache could lose half a minute — the
//! very half minute that explains the freeze.
//!
//! Every line is stamped with the wall clock, seconds since the epoch, so it
//! lines up with the system journal (`journalctl --since @<seconds>`).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many sessions' logs are kept. The newest are the ones worth having.
const KEEP: usize = 10;

/// How far behind the disk is allowed to fall.
const SYNC_EVERY: Duration = Duration::from_secs(1);

/// A session's log is cut in two parts of at most this size: the current
/// one and the one before it, which is renamed `.old`. A long session keeps
/// its end, which is where a failure is, without filling the disk.
const PART_BYTES: u64 = 8 * 1024 * 1024;

/// One session's log. Lines go to a writer thread, so a slow disk never holds
/// up draining the client's output.
pub struct StreamLog {
    lines: Option<mpsc::Sender<String>>,
    writer: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

impl StreamLog {
    /// A log for a session starting now, in [`dir`]. `None` when there is
    /// nowhere to write one, which is said but does not stop the stream.
    pub fn create() -> Option<Self> {
        let Some(dir) = dir() else {
            eprintln!("dusk: no home directory, so this stream is not logged");
            return None;
        };
        match Self::create_in(&dir, PART_BYTES) {
            Ok(log) => {
                eprintln!("dusk: this stream's log is {:?}", log.path);
                Some(log)
            }
            Err(err) => {
                eprintln!("dusk: could not start a log for this stream in {dir:?}: {err}");
                None
            }
        }
    }

    fn create_in(dir: &Path, part_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("stream-{}.log", epoch().as_millis()));
        let file = File::create(&path)?;
        prune(dir, KEEP);

        let (lines, received) = mpsc::channel();
        let target = path.clone();
        let writer = std::thread::Builder::new()
            .name("dusk-stream-log".into())
            .spawn(move || write(received, file, &target, part_bytes))?;
        Ok(Self {
            lines: Some(lines),
            writer: Some(writer),
            path,
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn create_in_for_tests(dir: &Path) -> Self {
        Self::create_in(dir, PART_BYTES).unwrap()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Add a line, stamped with when it was said rather than when it lands.
    pub fn line(&self, line: &str) {
        let at = epoch();
        if let Some(lines) = &self.lines {
            let _ = lines.send(format!(
                "{}.{:03} {line}\n",
                at.as_secs(),
                at.subsec_millis()
            ));
        }
    }

    /// Close the log and wait until it is on disk.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn finish(mut self) {
        self.lines.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

impl Drop for StreamLog {
    /// Closing the channel is enough: the writer syncs what is left and
    /// exits on its own, without anyone having to wait on the disk.
    fn drop(&mut self) {
        self.lines.take();
    }
}

fn epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

fn write(lines: mpsc::Receiver<String>, mut file: File, path: &Path, part_bytes: u64) {
    let mut written = 0u64;
    let mut dirty = false;
    let mut synced = Instant::now();

    loop {
        match lines.recv_timeout(SYNC_EVERY) {
            Ok(line) => {
                if written > 0 && written + line.len() as u64 > part_bytes {
                    let _ = file.sync_data();
                    let _ = std::fs::rename(path, old_part(path));
                    match File::create(path) {
                        Ok(next) => file = next,
                        Err(_) => return,
                    }
                    written = 0;
                }
                // A disk that has stopped taking writes will not start again
                // for the rest of this session.
                if file.write_all(line.as_bytes()).is_err() {
                    return;
                }
                written += line.len() as u64;
                dirty = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if dirty && synced.elapsed() >= SYNC_EVERY {
            let _ = file.sync_data();
            dirty = false;
            synced = Instant::now();
        }
    }
    let _ = file.sync_data();
}

fn old_part(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// When a session's log was started, from its name.
fn started(name: &str) -> Option<u128> {
    name.strip_prefix("stream-")?
        .strip_suffix(".log")?
        .parse()
        .ok()
}

/// Remove all but the newest `keep` sessions' logs, and their `.old` parts.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(u128, PathBuf)> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            Some((started(path.file_name()?.to_str()?)?, path))
        })
        .collect();
    logs.sort_by_key(|(started, _)| std::cmp::Reverse(*started));
    for (_, path) in logs.into_iter().skip(keep) {
        let _ = std::fs::remove_file(old_part(&path));
        let _ = std::fs::remove_file(&path);
    }
}

/// Where stream logs are kept: a state directory, which outlives a reboot
/// where `/tmp` does not, named for the bundle like the settings are.
pub fn dir() -> Option<PathBuf> {
    state_base(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("LOCALAPPDATA"),
    )
    .map(|base| base.join(crate::store::IDENTIFIER).join("streams"))
}

/// The platform's directory for logs and state, from the environment values
/// that decide it. Split out from [`dir`] so the choice can be tested.
fn state_base(
    home: Option<std::ffi::OsString>,
    xdg_state_home: Option<std::ffi::OsString>,
    local_app_data: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let home = || home.filter(|h| !h.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return local_app_data.filter(|a| !a.is_empty()).map(PathBuf::from);
    }
    if cfg!(target_os = "macos") {
        return Some(home()?.join("Library/Logs"));
    }
    match xdg_state_home {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => Some(home()?.join(".local/state")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Dir(PathBuf);
    impl Dir {
        pub(crate) fn new(name: &str) -> Self {
            let mut bytes = [0u8; 8];
            getrandom::getrandom(&mut bytes).unwrap();
            let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("dusk-{name}-{suffix}"));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_line_is_kept_and_stamped_with_the_wall_clock() {
        let dir = Dir::new("stream-log");
        let before = epoch().as_secs();
        let log = StreamLog::create_in(dir.path(), PART_BYTES).unwrap();
        let path = log.path().to_owned();
        for i in 0..100 {
            log.line(&format!("line {i}"));
        }
        log.finish();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 100);
        assert!(lines[99].ends_with(" line 99"), "{}", lines[99]);
        let (stamp, _) = lines[0].split_once(' ').unwrap();
        let secs: u64 = stamp.split_once('.').unwrap().0.parse().unwrap();
        assert!(secs >= before && secs <= epoch().as_secs(), "{stamp}");
    }

    /// The point of the whole module: what was said reaches the disk without
    /// the session ending, because a freeze never lets it end.
    #[test]
    fn a_session_still_running_is_on_disk_within_a_second_or_so() {
        let dir = Dir::new("stream-log-live");
        let log = StreamLog::create_in(dir.path(), PART_BYTES).unwrap();
        log.line("decoder reset");
        std::thread::sleep(SYNC_EVERY * 2);

        // Read while the log is still open. A sync cannot be observed from
        // here, but a write that never left the channel can.
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert!(text.ends_with(" decoder reset\n"), "{text:?}");
        drop(log);
    }

    #[test]
    fn a_long_session_keeps_its_end_and_the_part_before_it() {
        let dir = Dir::new("stream-log-parts");
        let log = StreamLog::create_in(dir.path(), 200).unwrap();
        let path = log.path().to_owned();
        for i in 0..30 {
            log.line(&format!("line {i}"));
        }
        log.finish();

        let now = std::fs::read_to_string(&path).unwrap();
        let before = std::fs::read_to_string(old_part(&path)).unwrap();
        assert!(now.trim_end().ends_with(" line 29"), "{now}");
        assert!(now.len() <= 200 && before.len() <= 200);
        assert!(!before.contains(" line 29\n"));
    }

    #[test]
    fn only_the_newest_sessions_are_kept() {
        let dir = Dir::new("stream-log-prune");
        for started in 1..=5u32 {
            std::fs::write(dir.path().join(format!("stream-{started}.log")), "").unwrap();
        }
        std::fs::write(dir.path().join("stream-1.log.old"), "").unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), "").unwrap();

        prune(dir.path(), 3);

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "stream-3.log",
                "stream-4.log",
                "stream-5.log",
                "unrelated.txt"
            ]
        );
    }

    /// Not `/tmp`: on Bazzite that is memory, and a reboot empties it.
    #[test]
    #[cfg(target_os = "linux")]
    fn logs_live_in_the_state_directory() {
        let home = Some(std::ffi::OsString::from("/home/someone"));
        assert_eq!(
            state_base(home.clone(), None, None),
            Some(PathBuf::from("/home/someone/.local/state"))
        );
        assert_eq!(
            state_base(home.clone(), Some("".into()), None),
            Some(PathBuf::from("/home/someone/.local/state"))
        );
        assert_eq!(
            state_base(home, Some("/elsewhere".into()), None),
            Some(PathBuf::from("/elsewhere"))
        );
        assert_eq!(state_base(None, None, None), None);
        assert!(dir().unwrap().ends_with("dev.dusk.app/streams"));
    }
}
