//! Persistence for the manual address book and Dusk's startup settings.
//!
//! Only hand-entered machines are stored. Anything found over mDNS is
//! rediscovered on launch, so writing it down would just create stale cards.
//! The file keeps its old name, `devices.json`, so an existing address book
//! is still found.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualEntry {
    pub address: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub manual: Vec<ManualEntry>,
    pub startup: crate::startup::Prefs,
    /// Let a machine that is not on this one's GitHub account pair with it:
    /// another person's Dusk by an Allow press here, anything else by a PIN.
    /// Off by default, because both need someone at this machine, and your
    /// own machines never do.
    pub allow_other_devices: bool,
    /// How big a stream from this machine asks to be.
    pub resolution: Resolution,
    /// How much data a stream may use, in Kbps. `None` lets the client pick
    /// from the stream's size and frame rate.
    pub bitrate_kbps: Option<u32>,
    /// Which video format a stream asks the host for.
    pub codec: Codec,
    /// How many frames a second a stream asks for. `None` lets the client use
    /// its own default. A cap the host and link can beat is only a ceiling;
    /// one they cannot reach streams at whatever they manage, so raising it
    /// never hurts a setup that cannot use it.
    pub fps: Option<u32>,
    /// Let the pointer leave a stream instead of being held inside it.
    ///
    /// A stream normally takes the mouse away from this machine entirely —
    /// the cursor stops existing here and every movement goes to the host,
    /// which is what a game turning its camera needs. On a desk with two
    /// screens that is a trap: a fullscreen stream on one of them cannot be
    /// moved out of without a keyboard, and on macOS where every fullscreen
    /// is a Space, that means Cmd+Tab out of the stream.
    ///
    /// On, the pointer is sent as a position rather than as movement, so it
    /// stays a pointer on this machine: it can walk off the edge of the
    /// stream onto the next monitor and the local cursor reappears when it
    /// does. The cost is real and belongs to games — a camera driven by
    /// mouse movement has nowhere to move once the pointer reaches the edge
    /// of the window — so this is off unless asked for.
    pub free_cursor: bool,
    /// Let a stream follow what the host's focused window is doing with the
    /// pointer, rather than keeping the mode it was launched with.
    ///
    /// This is the setting that makes [`Store::free_cursor`] stop mattering
    /// mid-stream: a game that takes the pointer gets relative motion even
    /// though the stream launched with the pointer free, and the desktop
    /// gets it back when the game is done.
    ///
    /// Off by default, and this one is a judgement rather than caution.
    /// Which windows want the pointer is decided from being fullscreen plus
    /// the window's class, because the signal everybody else uses — the
    /// host's cursor being hidden or locked — is not available on Wayland to
    /// anything but the compositor. That makes it a guess, and it was wrong
    /// within five minutes of first running: a fullscreen Telegram would
    /// have had the pointer taken off it. So it is asked for rather than
    /// assumed, it says what it did, and the mouse-mode shortcut overrides
    /// it for the rest of the session.
    ///
    /// Needs a host that can be asked, which today means KDE. Elsewhere it
    /// can be on and simply never report anything.
    pub follow_host_pointer: bool,
    /// Window classes that never take the pointer, whatever the rule says.
    ///
    /// The built-in list is known cases and not coverage, so this is the way
    /// out that does not need a release: when something fullscreen takes the
    /// pointer and should not have, this machine's log names the class it
    /// decided on, and a class written down here is never a game again.
    ///
    /// Only consulted while [`Store::follow_host_pointer`] is on, and only on
    /// the machine being streamed from, which is where the rule runs.
    pub pointer_not_games: Vec<String>,
    /// Also play a stream's sound on the host's own speakers, rather than
    /// only here.
    ///
    /// Off is not merely the quieter choice, it is the safer one, and the
    /// reason is worth writing down because it cost an afternoon on
    /// 2026-11-03. Asking for host audio changes what the *host* captures:
    /// Sunshine stops using the silent sink it makes for itself and reads
    /// the host's real output device instead. Whatever that device is, the
    /// host now holds it open for the whole stream.
    ///
    /// When it is a Bluetooth headset paired to both machines — the case
    /// that found this — the host monitoring it is enough to make the
    /// headset hand its audio to the host, and the machine you are sitting
    /// at goes silent. Nothing in either program says so; the sound simply
    /// stops. So Dusk asks for host audio only when someone wants it.
    pub host_audio: bool,
    /// Per-machine overrides over the four settings above. Keyed by device id;
    /// a machine with no entry, or an entry whose fields are all `None`,
    /// streams exactly as the globals say. The id is stable while a machine
    /// is online, which is the only time a stream is launched.
    #[serde(default)]
    pub device_streams: std::collections::HashMap<String, DeviceStream>,
    /// Streams from this machine are given a screen the size of the device
    /// watching: the virtual display on Windows, once the driver is
    /// installed, and the real output resized or scaled on Linux. Keeps its
    /// old name so an existing settings file still reads. See
    /// [`crate::host::virtual_screen`].
    pub virtual_screen: bool,
    /// Text copied on either machine during a stream can be pasted on the
    /// other. An enum rather than a `bool` so a missing field, from a fresh
    /// install or an older settings file, reads as on.
    pub clipboard_sharing: ClipboardSharing,
    /// Install updates and restart while nobody is streaming either way.
    /// On unless turned off, like [`Store::clipboard_sharing`]: a host
    /// nobody sits at is the machine this is for.
    pub auto_update: AutoUpdate,
    /// What the file looked like when it was last read or written here, so
    /// a change another process made since can be told from this copy's
    /// own. Never in the file.
    #[serde(skip)]
    pub(crate) on_disk: Option<Fingerprint>,
    /// Your own machines this one has paired by certificate, without a PIN.
    /// Dusk only ever touches the host's paired-machine entries named here.
    pub account_pairings: Vec<AccountPairing>,
    /// Entries Dusk added that a newer certificate has replaced, still to be
    /// taken out of the host's list.
    pub retired_pairings: Vec<String>,
    /// For each of your machines, the sha256 of every certificate a newer
    /// one has replaced. A request to pair one of these again is an old
    /// request played back, and is turned down.
    #[serde(default)]
    pub retired_certs: std::collections::HashMap<String, Vec<String>>,
    /// The host's list of paired machines was changed and the host has not
    /// restarted to load it yet. Kept on disk, so a Dusk that quits before
    /// the restart still owes it next time.
    pub restart_owed: bool,
}

/// One of your machines, paired with this one by its certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPairing {
    /// Its id on the account.
    pub machine: String,
    pub name: String,
    pub cert: String,
    /// The id of its entry in the host's list. Minted by Dusk, so an entry
    /// it did not make is never one it takes out.
    pub uuid: String,
}

/// Per-machine streaming overrides over the global settings.
///
/// Each field is `None` to inherit the global, or set to override it just for
/// one machine — so a fast host gets 120 fps and HEVC while everything else
/// keeps the defaults. A machine whose fields are all `None` is the same as
/// having no entry at all. See [`Store::device_streams`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceStream {
    pub resolution: Option<Resolution>,
    pub bitrate_kbps: Option<u32>,
    pub fps: Option<u32>,
    pub codec: Option<Codec>,
    pub free_cursor: Option<bool>,
}

impl DeviceStream {
    /// Nothing is overridden, so the entry can be dropped rather than stored.
    pub fn is_empty(&self) -> bool {
        self.resolution.is_none()
            && self.bitrate_kbps.is_none()
            && self.fps.is_none()
            && self.codec.is_none()
            && self.free_cursor.is_none()
    }
}

/// The name Sunshine gives the entry that streams the desktop itself.
///
/// Matched by name because that is all there is to match on: the app list
/// carries an id, a name and whether HDR is supported, and the id is
/// generated per host. A renamed desktop entry reads as an app here, which
/// the per-machine override exists to correct.
const DESKTOP_APP: &str = "desktop";

/// Whether `app_name` streams the desktop rather than launching something.
pub fn is_desktop_app(app_name: &str) -> bool {
    app_name.trim().eq_ignore_ascii_case(DESKTOP_APP)
}

/// Whether a stream of `app_name` lets the pointer leave the stream.
///
/// Free cursor sends absolute pointer positions, which a pointer-locked game
/// ignores outright — so the camera does not turn and nothing on screen says
/// why. The global toggle is therefore read as a preference about *desktop*
/// streaming, which is the only place it can be honoured, and an app that is
/// not the desktop gets relative capture regardless of it.
///
/// A per-machine override wins over both, so a host whose desktop entry was
/// renamed, or a game that wants the pointer anyway, is still reachable.
/// Returning the decision rather than applying it keeps this testable and
/// keeps the one caller free to log what it chose and why.
pub fn free_cursor_for(app_name: &str, over: Option<&DeviceStream>, global: bool) -> bool {
    if let Some(explicit) = over.and_then(|o| o.free_cursor) {
        return explicit;
    }
    is_desktop_app(app_name) && global
}

/// The lowest and highest bitrates the client accepts, in Kbps.
pub const BITRATE_RANGE: std::ops::RangeInclusive<u32> = 500..=500_000;

/// The lowest and highest frame rates a stream may ask for. Wide enough for
/// a 24fps film cap at one end and a 480Hz panel at the other.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 24..=480;

/// Which video format a stream asks the host for.
///
/// Automatic is the default because the client and host agree on the best
/// format both can decode and encode in hardware, which a person picking
/// by name cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Codec {
    #[default]
    Auto,
    H264,
    Hevc,
    Av1,
}

/// Whether text copied during a stream is shared with the other machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardSharing {
    #[default]
    On,
    Off,
}

impl ClipboardSharing {
    pub fn is_on(self) -> bool {
        self == ClipboardSharing::On
    }
}

/// Whether Dusk updates itself while this machine is idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoUpdate {
    #[default]
    On,
    Off,
}

impl AutoUpdate {
    pub fn is_on(self) -> bool {
        self == AutoUpdate::On
    }
}

/// How big a stream asks to be.
///
/// Matching the host is the default because a stream whose shape differs
/// from the host's screen gets bars on the host's side, baked into the
/// picture, and the pointer cannot reach into them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Resolution {
    #[default]
    MatchHost,
    MatchThisDisplay,
    Fixed {
        width: u32,
        height: u32,
    },
}

impl Resolution {
    /// Sizes a stream can actually be: encoders want even dimensions, and
    /// nothing outside these bounds is a screen.
    pub fn is_valid(&self) -> bool {
        match *self {
            Resolution::Fixed { width, height } => {
                (320..=15360).contains(&width)
                    && (240..=8640).contains(&height)
                    && width % 2 == 0
                    && height % 2 == 0
            }
            _ => true,
        }
    }
}

impl Store {
    /// Read the address book, treating a missing or corrupt file as empty.
    ///
    /// A parse failure must not stop the app from starting — the grid still
    /// works from discovery alone, and refusing to launch over a bad config
    /// file would be a worse outcome than losing the list.
    ///
    /// A file that would not parse is moved aside rather than left in
    /// place: the next save would otherwise overwrite it, and with it the
    /// only record of which pairings Dusk added to the host and which
    /// restart it still owes. Kept beside the original so it can be
    /// recovered by hand.
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Store>(&raw) {
            Ok(mut store) => {
                store.on_disk = fingerprint(path);
                store
            }
            Err(err) => {
                let aside = set_aside(path);
                match std::fs::rename(path, &aside) {
                    Ok(()) => eprintln!(
                        "dusk: could not read the settings at {path:?} ({err}); kept it at {aside:?} and started afresh"
                    ),
                    Err(move_err) => eprintln!(
                        "dusk: could not read the settings at {path:?} ({err}) nor move it aside ({move_err}); it will be overwritten"
                    ),
                }
                Self::default()
            }
        }
    }

    /// Write the settings whole, never half: what this file holds includes
    /// a note that the host owes a restart, and a crash part way through a
    /// plain write could lose it along with everything else here.
    ///
    /// `&mut` only to note what the file looks like afterwards, for
    /// [`Store::adopt_external_changes`].
    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_atomically(path, json.as_bytes()).map_err(|e| e.to_string())?;
        self.on_disk = fingerprint(path);
        Ok(())
    }

    /// Take on a change another process made to the file since this copy
    /// last read or wrote it. Answers whether anything changed.
    ///
    /// `dusk screen` is the one command that writes a setting while a
    /// window may be running, from a shell on a machine whose Dusk sits in
    /// the tray. Without this the window kept answering other machines
    /// from its own copy, where the setting was still off, and wrote that
    /// copy over the file at its next save. Only the sized-screen setting
    /// is taken: nothing else is written from outside, and everything else
    /// in the file is this window's own bookkeeping.
    pub fn adopt_external_changes(&mut self, path: &Path) -> bool {
        let now = fingerprint(path);
        if now == self.on_disk {
            return false;
        }
        self.on_disk = now;
        let Ok(raw) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(disk) = serde_json::from_str::<Store>(&raw) else {
            return false;
        };
        if disk.virtual_screen == self.virtual_screen {
            return false;
        }
        self.virtual_screen = disk.virtual_screen;
        true
    }

    pub fn upsert(&mut self, entry: ManualEntry) {
        match self
            .manual
            .iter_mut()
            .find(|e| e.address.eq_ignore_ascii_case(&entry.address) && e.port == entry.port)
        {
            Some(existing) => *existing = entry,
            None => self.manual.push(entry),
        }
    }

    pub fn remove(&mut self, address: &str, port: Option<u16>) {
        self.manual
            .retain(|e| !(e.address.eq_ignore_ascii_case(address) && e.port == port));
    }
}

/// Replace the file at `path` with `contents` so that it is only ever the
/// old file or the new one: written to a sibling temp file, flushed to disk,
/// renamed over it, and the rename itself flushed by syncing the directory.
/// The old file's permissions are kept, set before any content lands so a
/// private file is never briefly readable under the default mode.
/// Enough of the file's metadata to notice it being rewritten: its
/// modification time and its length, since a rewrite within the clock's
/// resolution is unlikely to also be the same length.
pub(crate) type Fingerprint = (std::time::SystemTime, u64);

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let tmp = temp_beside(path);
    let result = (|| {
        // Never an existing file, should a name ever come round twice.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Ok(meta) = std::fs::metadata(path) {
            file.set_permissions(meta.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // The rename only survives a crash once the directory holding it is on
    // disk. Windows has no handle to sync a directory through, and its
    // rename is already written through by the filesystem. Only logged:
    // the file has been replaced by now, and a caller told otherwise would
    // act as though it still held the old contents.
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Err(err) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
            eprintln!("dusk: wrote {path:?} but could not flush its directory: {err}");
        }
    }
    Ok(())
}

/// Where an unreadable settings file is kept: `devices.json.corrupt-<secs>`
/// beside the original.
fn set_aside(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".corrupt-{secs}"));
    path.with_file_name(name)
}

/// A temp file name beside `path`, in the same directory so the rename that
/// replaces `path` stays atomic, and random, so two processes writing the
/// same file (the `dusk` command and the window) never share one.
fn temp_beside(path: &Path) -> PathBuf {
    let mut nonce = [0u8; 8];
    getrandom::getrandom(&mut nonce).expect("the system would not supply randomness");
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(
        ".{}.dusk-tmp",
        crate::install::download::hex(&nonce)
    ));
    PathBuf::from(tmp)
}

pub fn store_path(config_dir: &Path) -> PathBuf {
    config_dir.join("devices.json")
}

/// This machine's bundle identifier, which is the directory the settings
/// live under. Pinned here to match `tauri.conf.json`.
pub(crate) const IDENTIFIER: &str = "dev.dusk.app";

/// Where the settings live, worked out without asking Tauri.
///
/// The CLI runs before Tauri starts, so `app.path().app_config_dir()` is not
/// available to it — and this **must** give the same answer that does. The
/// day the two disagree, one of them writes a setting where the other never
/// looks: exactly the failure [`crate::secrets`] argues about, and the
/// reason the window checks this against Tauri's own answer at startup
/// rather than trusting that they agree. `None` when the home directory
/// cannot be found, which is a machine nothing can be stored on anyway.
///
/// What Tauri does, reproduced: the platform's configuration directory with
/// the bundle identifier under it.
pub fn config_dir() -> Option<PathBuf> {
    config_base(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("APPDATA"),
    )
    .map(|base| base.join(IDENTIFIER))
}

/// The platform's configuration directory, from the three environment
/// values that can decide it. Split out from [`config_dir`] so the choice
/// between them can be tested without changing the environment the test
/// itself is running in.
fn config_base(
    home: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    appdata: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let home = || home.filter(|h| !h.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return appdata.filter(|a| !a.is_empty()).map(PathBuf::from);
    }
    if cfg!(target_os = "macos") {
        return Some(home()?.join("Library/Application Support"));
    }
    match xdg_config_home {
        // An empty value is the same as unset, which is what the XDG
        // specification says and what every other reader of it here does.
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => Some(home()?.join(".config")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_update_is_on_for_an_existing_file_without_it() {
        let store: Store = serde_json::from_str(r#"{"codec":"auto"}"#).unwrap();
        assert!(store.auto_update.is_on());
    }

    #[test]
    fn clipboard_sharing_is_on_for_an_existing_file_without_it() {
        let store: Store = serde_json::from_str(r#"{"codec":"auto"}"#).unwrap();
        assert!(store.clipboard_sharing.is_on());
    }

    #[test]
    fn clipboard_sharing_off_survives_a_save() {
        let store = Store {
            clipboard_sharing: ClipboardSharing::Off,
            ..Store::default()
        };
        let back: Store = serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
        assert!(!back.clipboard_sharing.is_on());
    }

    /// The pointer is held by the stream unless someone asks otherwise:
    /// that is what a game needs, and an existing settings file never said.
    #[test]
    fn the_cursor_is_held_by_the_stream_until_asked_and_then_stays_free() {
        let store: Store = serde_json::from_str(r#"{"codec":"auto"}"#).unwrap();
        assert!(!store.free_cursor);

        let asked = Store {
            free_cursor: true,
            ..Store::default()
        };
        let back: Store = serde_json::from_str(&serde_json::to_string(&asked).unwrap()).unwrap();
        assert!(back.free_cursor);
    }

    /// The host stays quiet unless someone asks, and an existing settings
    /// file never asked. Defaulting the other way would hand every upgrade
    /// the Bluetooth bug this setting exists to stop.
    #[test]
    fn the_host_is_silent_until_asked_and_then_stays_asked() {
        let store: Store = serde_json::from_str(r#"{"codec":"auto"}"#).unwrap();
        assert!(!store.host_audio);

        let asked = Store {
            host_audio: true,
            ..Store::default()
        };
        let back: Store = serde_json::from_str(&serde_json::to_string(&asked).unwrap()).unwrap();
        assert!(back.host_audio);
    }

    #[test]
    fn the_settings_directory_is_named_for_the_bundle() {
        // If this ever stops matching `tauri.conf.json`, `dusk screen`
        // writes where the window never reads. The window says so at
        // startup; this says so at build time.
        let dir = config_dir().expect("a home to put settings in");
        assert!(dir.ends_with(IDENTIFIER), "{dir:?}");
        assert_eq!(store_path(&dir).file_name().unwrap(), "devices.json");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn xdg_config_home_wins_but_only_when_it_says_something() {
        let home = Some(std::ffi::OsString::from("/home/someone"));
        let xdg = Some(std::ffi::OsString::from("/elsewhere/config"));

        assert_eq!(
            config_base(home.clone(), xdg, None),
            Some(PathBuf::from("/elsewhere/config"))
        );
        // Empty is unset, per the specification and the rest of this crate.
        assert_eq!(
            config_base(home.clone(), Some(std::ffi::OsString::new()), None),
            Some(PathBuf::from("/home/someone/.config"))
        );
        assert_eq!(
            config_base(home, None, None),
            Some(PathBuf::from("/home/someone/.config"))
        );
        // Nowhere to put anything is an answer of none, not a guess at /.
        assert_eq!(config_base(None, None, None), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_keeps_settings_under_application_support() {
        assert_eq!(
            config_base(
                Some(std::ffi::OsString::from("/Users/someone")),
                // Ignored here: Tauri does not consult it on macOS, so
                // neither may this.
                Some(std::ffi::OsString::from("/elsewhere")),
                None
            ),
            Some(PathBuf::from("/Users/someone/Library/Application Support"))
        );
        assert_eq!(config_base(None, None, None), None);
    }

    #[test]
    fn a_file_from_before_the_setting_keeps_other_devices_out() {
        let stored = r#"{"manual":[],"startup":{"startInBackground":true,"closeToTray":true}}"#;
        let store: Store = serde_json::from_str(stored).unwrap();
        assert!(!store.allow_other_devices);
    }

    #[test]
    fn a_file_from_before_the_setting_matches_the_host() {
        let stored = r#"{"manual":[],"allow_other_devices":true}"#;
        let store: Store = serde_json::from_str(stored).unwrap();
        assert_eq!(store.resolution, Resolution::MatchHost);
    }

    #[test]
    fn a_file_from_before_the_settings_leaves_bitrate_and_codec_to_the_client() {
        let stored = r#"{"manual":[],"resolution":{"kind":"matchHost"}}"#;
        let store: Store = serde_json::from_str(stored).unwrap();
        assert_eq!(store.bitrate_kbps, None);
        assert_eq!(store.fps, None);
        assert_eq!(store.codec, Codec::Auto);
        assert!(store.device_streams.is_empty());
    }

    #[test]
    fn a_device_override_round_trips_and_knows_when_it_is_empty() {
        assert!(DeviceStream::default().is_empty());

        let set = DeviceStream {
            fps: Some(120),
            codec: Some(Codec::Hevc),
            ..Default::default()
        };
        assert!(!set.is_empty());
        assert!(set.resolution.is_none());

        let json = serde_json::to_string(&set).unwrap();
        let back: DeviceStream = serde_json::from_str(&json).unwrap();
        assert_eq!(back, set);
    }

    #[test]
    fn a_device_override_reads_only_the_fields_present() {
        // The window sends just the fields it means to set; the rest inherit.
        let over: DeviceStream = serde_json::from_str(r#"{"fps":120}"#).unwrap();
        assert_eq!(over.fps, Some(120));
        assert_eq!(over.bitrate_kbps, None);
        assert_eq!(over.codec, None);
        assert!(over.resolution.is_none());
    }

    #[test]
    fn following_the_host_pointer_is_off_until_asked_for() {
        // Off by default because the rule behind it is a guess, and it was
        // wrong within five minutes of first running. A file written before
        // the setting existed reads as off too.
        assert!(!Store::default().follow_host_pointer);

        let old: Store = serde_json::from_str("{}").unwrap();
        assert!(!old.follow_host_pointer);

        let on: Store = serde_json::from_str(r#"{"follow_host_pointer":true}"#).unwrap();
        assert!(on.follow_host_pointer);
    }

    #[test]
    fn the_desktop_entry_is_recognised_by_name_whatever_its_case() {
        assert!(is_desktop_app("Desktop"));
        assert!(is_desktop_app("desktop"));
        assert!(is_desktop_app("  Desktop  "));
        assert!(!is_desktop_app("Steam Big Picture"));
        assert!(!is_desktop_app("Desktop Dungeons"));
        assert!(!is_desktop_app(""));
    }

    #[test]
    fn only_the_desktop_honours_the_global_free_cursor() {
        // The whole point: the toggle is on, and a game still gets relative
        // capture, because absolute positions cannot steer a locked pointer.
        assert!(free_cursor_for("Desktop", None, true));
        assert!(!free_cursor_for("Steam Big Picture", None, true));

        // With it off, nothing turns it on.
        assert!(!free_cursor_for("Desktop", None, false));
        assert!(!free_cursor_for("Steam Big Picture", None, false));
    }

    #[test]
    fn a_per_machine_free_cursor_wins_over_both() {
        let on = DeviceStream {
            free_cursor: Some(true),
            ..Default::default()
        };
        let off = DeviceStream {
            free_cursor: Some(false),
            ..Default::default()
        };

        // The escape hatch for a renamed desktop entry: a game name, the
        // global off, and the pointer still leaves because it was asked for.
        assert!(free_cursor_for("My Desktop", Some(&on), false));
        // And the other way, for a desktop that should hold the pointer.
        assert!(!free_cursor_for("Desktop", Some(&off), true));

        // An override that sets other fields leaves this one inheriting.
        let other = DeviceStream {
            fps: Some(120),
            ..Default::default()
        };
        assert!(free_cursor_for("Desktop", Some(&other), true));
        assert!(!free_cursor_for("Steam Big Picture", Some(&other), true));
    }

    #[test]
    fn a_codec_round_trips_in_the_shape_the_window_sends() {
        let codec: Codec = serde_json::from_str(r#""hevc""#).unwrap();
        assert_eq!(codec, Codec::Hevc);
        assert_eq!(serde_json::to_string(&Codec::Av1).unwrap(), r#""av1""#);
        assert_eq!(serde_json::to_string(&Codec::H264).unwrap(), r#""h264""#);
    }

    #[test]
    fn a_resolution_round_trips_in_the_shape_the_window_sends() {
        let fixed: Resolution =
            serde_json::from_str(r#"{"kind":"fixed","width":1920,"height":1080}"#).unwrap();
        assert_eq!(
            fixed,
            Resolution::Fixed {
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(
            serde_json::to_string(&Resolution::MatchThisDisplay).unwrap(),
            r#"{"kind":"matchThisDisplay"}"#
        );
    }

    #[test]
    fn only_a_real_screen_size_is_a_resolution() {
        assert!(Resolution::Fixed {
            width: 2560,
            height: 1440
        }
        .is_valid());
        assert!(!Resolution::Fixed {
            width: 0,
            height: 1440
        }
        .is_valid());
        assert!(!Resolution::Fixed {
            width: 1921,
            height: 1080
        }
        .is_valid());
        assert!(Resolution::MatchHost.is_valid());
    }

    #[test]
    fn a_corrupt_file_reads_as_empty_and_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("dusk-test-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("devices.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Store::load(&path).manual.is_empty());

        // The next save must not be the end of the only copy of what the
        // host owes, so the unreadable file is moved, not left to be
        // overwritten.
        assert!(
            !path.exists(),
            "the unreadable file is no longer in the way"
        );
        let kept: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("devices.json.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join(&kept[0])).unwrap(),
            "{ not json"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_setting_written_by_the_command_line_reaches_a_running_window() {
        let dir = std::env::temp_dir().join(format!("dusk-store-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("devices.json");
        let mut window = Store::default();
        window.save(&path).unwrap();
        // Its own write is not news.
        assert!(!window.adopt_external_changes(&path));

        // `dusk screen on`, in another process: load, set, save.
        let mut cli = Store::load(&path);
        cli.virtual_screen = true;
        cli.save(&path).unwrap();
        assert!(window.adopt_external_changes(&path));
        assert!(window.virtual_screen);
        assert!(!window.adopt_external_changes(&path));

        // The window's next save keeps it, rather than writing its old
        // copy over the file.
        window.restart_owed = true;
        window.save(&path).unwrap();
        assert!(Store::load(&path).virtual_screen);

        // A change made in the window is not undone by looking at the file.
        window.virtual_screen = false;
        window.save(&path).unwrap();
        assert!(!window.adopt_external_changes(&path));
        assert!(!window.virtual_screen);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_replaces_the_file_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("dusk-store-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("devices.json");
        let mut store = Store {
            restart_owed: true,
            ..Default::default()
        };
        store.save(&path).unwrap();
        assert!(Store::load(&path).restart_owed);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no temp file left behind");

        // A write that cannot be made leaves the file as it was, rather
        // than half-written over.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let before = std::fs::read_to_string(&path).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let mut store = Store {
                restart_owed: false,
                ..store
            };
            let saved = store.save(&path);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(saved.is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_write_gets_a_temp_file_of_its_own_beside_the_target() {
        // The `dusk` command and the window are separate processes writing
        // the same settings: a shared temp name would let one rename the
        // other's half-written file into place.
        let path = Path::new("/some/dir/devices.json");
        let one = temp_beside(path);
        let two = temp_beside(path);
        assert_ne!(one, two);
        assert_eq!(one.parent(), path.parent());
        assert_eq!(two.parent(), path.parent());
        let name = one.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("devices.json.") && name.ends_with(".dusk-tmp"),
            "{name}"
        );
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating() {
        let mut store = Store::default();
        store.upsert(ManualEntry {
            address: "192.168.1.40".into(),
            port: None,
            name: None,
        });
        store.upsert(ManualEntry {
            address: "192.168.1.40".into(),
            port: None,
            name: Some("Workshop PC".into()),
        });
        assert_eq!(store.manual.len(), 1);
        assert_eq!(store.manual[0].name.as_deref(), Some("Workshop PC"));
    }
}
