//! Streaming a screen the size of the device watching, on Linux.
//!
//! Windows does this with a virtual display ([`super::virtual_screen`]), and
//! none of that machinery exists here. The driver is user-mode IddCx, a
//! Windows interface; `amdgpu` offers no `VIRTUAL` output to stand in for
//! it; and Sunshine's display-device settings are Windows-only — a Linux
//! host's configuration has no `dd_*` key at all, measured on
//! `2026.914.233613`. So there is nothing to point a stream at but the
//! output the host is really using.
//!
//! This switches that output to the size the client asked for and switches
//! it back afterwards, through Sunshine's own `global_prep_cmd` hooks. Three
//! things follow, all of them different from Windows, and all worth knowing
//! before changing this:
//!
//! - **The host's own monitor is not left alone.** On Windows the physical
//!   monitors switch off and the virtual one is streamed; here there is one
//!   screen, and the person sitting at it sees it change size or go squashed
//!   for the length of the stream.
//! - **A size the monitor has no mode for is reached by scaling, never by
//!   inventing a mode.** `cvt` and `xrandr --newmode` would drive the panel
//!   at something it may not be able to display, leaving the host dark if
//!   the `undo` hook never runs. Scaling keeps the panel on a mode it
//!   already had and resizes only the framebuffer, which is what a stream
//!   captures. See [`Plan::Scaled`] — without it the feature does nothing
//!   whatsoever for an ultrawide client against a 16:9 host.
//! - **Sunshine does the calling, not Dusk.** `do` and `undo` are the only
//!   hooks that reliably bracket a stream, including one that dies, and the
//!   host's Dusk has no equivalent. So Dusk registers *itself* as the prep
//!   command and is re-run by Sunshine with [`HOOK_FLAG`].
//!
//! Not done: the client's frame rate is ignored, so a mode is chosen by
//! size and keeps whatever rate `xrandr` picks first for it.
//!
//! Only Linux calls any of this, but it is compiled everywhere on purpose:
//! reading `xrandr` output and merging Sunshine's prep commands are plain
//! text handling, and the tests for them are worth running on whichever
//! machine the work is being done on. There is no Linux equivalent of
//! `scripts/check-windows.sh`, so a module gated to Linux would only ever
//! be compiled by CI.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::PathBuf;
use std::process::Command;

/// The argument that makes Dusk switch the screen instead of opening a
/// window. Sunshine runs this, never a person.
pub const HOOK_FLAG: &str = "--dusk-match-screen";

/// What Sunshine runs before a stream, and after it.
const DO: &str = "do";
const UNDO: &str = "undo";

/// Where Sunshine keeps the commands it runs around every stream.
const PREP_KEY: &str = "global_prep_cmd";

/// A mode an output offers.
///
/// The name is kept as well as the size because `xrandr --mode` takes the
/// name, and a name is not always its size: interlaced and generated modes
/// are called things like `1920x1080i` and `1920x1080_60.00`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub name: String,
    pub size: (u32, u32),
}

/// A connected output, as `xrandr --query` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub name: String,
    pub primary: bool,
    /// The mode it is showing. `None` for an output that is connected and
    /// switched off, which cannot be restored to anything and so is left
    /// alone.
    pub current: Option<Mode>,
    pub modes: Vec<Mode>,
}

impl Output {
    /// The sizes it can show, each once, in the order `xrandr` lists them.
    pub fn sizes(&self) -> Vec<(u32, u32)> {
        let mut sizes = Vec::new();
        for mode in &self.modes {
            if !sizes.contains(&mode.size) {
                sizes.push(mode.size);
            }
        }
        sizes
    }

    /// The mode to ask for to get `size`: the first of that size, which is
    /// also the one `xrandr --mode <width>x<height>` would choose.
    fn mode_for(&self, size: (u32, u32)) -> Option<&Mode> {
        self.modes.iter().find(|m| m.size == size)
    }
}

/// A size written `1920x1080`, ignoring anything after it.
fn size_of(name: &str) -> Option<(u32, u32)> {
    let (w, rest) = name.split_once('x')?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .map_or(rest, |i| &rest[..i]);
    Some((w.parse().ok()?, digits.parse().ok()?))
}

/// Read `xrandr --query`.
///
/// Only connected outputs are kept. A mode line belongs to the output above
/// it, and the mode marked `*` is the one being shown — read from the
/// markers rather than from the geometry on the output line, because the
/// markers name the mode and the geometry only gives its size.
pub fn parse_query(query: &str) -> Vec<Output> {
    let mut outputs: Vec<Output> = Vec::new();
    for line in query.lines() {
        if line.starts_with([' ', '\t']) {
            let Some(output) = outputs.last_mut() else {
                continue;
            };
            let mut tokens = line.split_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            let Some(size) = size_of(name) else {
                continue;
            };
            let mode = Mode {
                name: name.to_string(),
                size,
            };
            // `59.95*`, `60.00*+` and a lone `*` all mean the same thing.
            if tokens.any(|t| t.contains('*')) {
                output.current = Some(mode.clone());
            }
            output.modes.push(mode);
            continue;
        }

        // `HDMI-A-1 connected primary 2560x1440+0+0 (normal ...) 480mm x 270mm`
        let mut tokens = line.split_whitespace();
        let Some(name) = tokens.next() else {
            continue;
        };
        // Everything else at column zero — `Screen 0: ...` — is not an output.
        if tokens.next() != Some("connected") {
            continue;
        }
        outputs.push(Output {
            name: name.to_string(),
            primary: tokens.next() == Some("primary"),
            current: None,
            modes: Vec::new(),
        });
    }
    outputs
}

/// The output a stream shows: the primary one, else the only connected one.
///
/// Several connected outputs with none marked primary is left alone rather
/// than guessed at — switching the wrong screen is worse than switching
/// none.
pub fn target(outputs: Vec<Output>) -> Option<Output> {
    if let Some(primary) = outputs.iter().find(|o| o.primary) {
        return Some(primary.clone());
    }
    match outputs.len() {
        1 => outputs.into_iter().next(),
        _ => None,
    }
}

/// Run `xrandr`, defaulting `DISPLAY` because Sunshine's prep commands
/// inherit its environment and a service started without a session may not
/// carry one.
fn xrandr(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("xrandr");
    command.args(args);
    if std::env::var_os("DISPLAY").is_none() {
        command.env("DISPLAY", ":0");
    }
    let output = command
        .output()
        .map_err(|e| format!("xrandr could not be run: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "xrandr refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The `Screen 0:` line: the size X is rendering at now, and the largest it
/// is willing to.
///
/// Both matter and neither is on an output's line. The current size is the
/// *framebuffer*, which is what a scaled screen changes and what Sunshine
/// captures — an output's mode stays where it was, so reading the mode back
/// would say nothing had happened. The maximum is the bound on asking for a
/// size at all.
pub fn screen_line(query: &str) -> Option<((u32, u32), (u32, u32))> {
    let line = query.lines().next()?;
    let after = |key: &str| -> Option<(u32, u32)> {
        let rest = line.split_once(key)?.1;
        let mut parts = rest.split_whitespace();
        let width: u32 = parts.next()?.trim_end_matches(',').parse().ok()?;
        if parts.next()? != "x" {
            return None;
        }
        let height: u32 = parts.next()?.trim_end_matches(',').parse().ok()?;
        Some((width, height))
    };
    Some((after("current ")?, after("maximum ")?))
}

/// The output a stream from this machine shows, and the biggest screen X
/// will give it.
fn current_screen() -> Option<(Output, (u32, u32))> {
    let query = xrandr(&["--query"]).ok()?;
    let (_, limit) = screen_line(&query)?;
    Some((target(parse_query(&query))?, limit))
}

/// The framebuffer size right now, which is what a stream is capturing.
fn framebuffer() -> Option<(u32, u32)> {
    screen_line(&xrandr(&["--query"]).ok()?).map(|(current, _)| current)
}

/// The sizes this machine can stream at without scaling anything.
pub fn modes() -> Vec<(u32, u32)> {
    current_screen().map(|(o, _)| o.sizes()).unwrap_or_default()
}

/// The size to offer a client whose screen is `want`.
///
/// Its own size, near enough always: scaling means the shape of the host's
/// monitor no longer limits what a stream can be, so there is nothing to
/// snap to and the answer is the client's screen as it is. Only an
/// impossible size is turned down, and then the host offers its own instead.
pub fn offer_for(want: (u32, u32)) -> Option<(u32, u32)> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return None;
    }
    let (output, limit) = current_screen()?;
    match plan_for(&output, want, limit)? {
        Plan::Already => Some(even(want)),
        Plan::Mode(mode) => Some(mode.size),
        Plan::Scaled { size, .. } => Some(size),
    }
}

/// Whether this machine can resize the screen it streams.
///
/// Wayland cannot: `xrandr` under XWayland reports a single made-up output
/// and setting a mode on it moves nothing real, so a session with a Wayland
/// display is ruled out before `xrandr` is even asked. What is left is
/// checked by doing it — an output with a choice of modes — rather than by
/// reading `XDG_SESSION_TYPE`, which says `tty` for a perfectly good X
/// session reached over SSH.
pub fn available() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return false;
    }
    current_screen().is_some_and(|(o, _)| o.current.is_some() && o.sizes().len() > 1)
}

/// Where the mode to go back to is remembered.
///
/// The runtime directory, so that a reboot forgets it: afterwards the screen
/// is at whatever X chose and restoring a mode from before would be wrong.
fn state_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dusk-screen-before")
}

/// The size the client asked for, as Sunshine passes it to a prep command.
fn client_size() -> Option<(u32, u32)> {
    let read = |key: &str| std::env::var(key).ok()?.parse::<u32>().ok();
    Some((
        read("SUNSHINE_CLIENT_WIDTH")?,
        read("SUNSHINE_CLIENT_HEIGHT")?,
    ))
}

/// How to give the screen the size a client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// It is already that size.
    Already,
    /// A mode the output lists. Preferred where there is one: the monitor
    /// shows it properly and nothing is scaled.
    Mode(Mode),
    /// A mode the output lists, with the framebuffer scaled on top of it to
    /// the size wanted.
    ///
    /// This is what makes the setting worth having. A monitor only lists the
    /// shapes it is: Venus's lists no ultrawide, so an ultrawide client got
    /// the nearest 16:9 mode — which is the size it already was — and the
    /// feature did nothing at all for the pair of machines it was built for.
    /// Scaling detaches the size X renders at from the mode the panel is
    /// driven at, so the client gets its exact size whatever the monitor is.
    ///
    /// The base mode is left as it is rather than chosen, so the panel keeps
    /// the signal it already had: a mode a monitor cannot display would
    /// leave the host dark, which is the risk that ruled out `--newmode`.
    /// The cost is that the host's own screen shows the whole desktop
    /// squashed into its real mode for the length of the stream.
    Scaled { base: Mode, size: (u32, u32) },
}

/// Sizes are rounded down to even: hardware encoders want even dimensions,
/// and a client reporting an odd one would otherwise be passed straight
/// through to the framebuffer.
fn even(size: (u32, u32)) -> (u32, u32) {
    (size.0 & !1, size.1 & !1)
}

/// How to reach `want` on `output`, or `None` if it cannot be reached.
pub fn plan_for(output: &Output, want: (u32, u32), limit: (u32, u32)) -> Option<Plan> {
    let current = output.current.clone()?;
    let want = even(want);
    if want.0 == 0 || want.1 == 0 || want.0 > limit.0 || want.1 > limit.1 {
        return None;
    }
    if want == current.size {
        return Some(Plan::Already);
    }
    if let Some(mode) = output.mode_for(want) {
        return Some(Plan::Mode(mode.clone()));
    }
    Some(Plan::Scaled {
        base: current,
        size: want,
    })
}

/// The `--scale` factors that turn `base` into `size`.
///
/// Six decimals, which is enough: measured on a live X server, 2000x1300
/// from a 2560x1440 base lands exactly, and so does an ultrawide 3440x1440.
/// The framebuffer is checked afterwards all the same.
fn scale_factors(base: (u32, u32), size: (u32, u32)) -> String {
    format!(
        "{:.6}x{:.6}",
        f64::from(size.0) / f64::from(base.0),
        f64::from(size.1) / f64::from(base.1)
    )
}

/// Switch the screen to the size the client asked for, remembering what it
/// was on first so that a switch which only half happens is still
/// recoverable.
fn switch_to_client() -> Result<(), String> {
    let want = client_size().ok_or("the host did not say what size the client wants")?;
    let (output, limit) = current_screen().ok_or("no single output to resize")?;
    let current = output
        .current
        .clone()
        .ok_or("that output is not showing anything")?;

    let plan = plan_for(&output, want, limit).ok_or("that is not a size this screen can be")?;
    if plan == Plan::Already {
        return Ok(());
    }

    std::fs::write(state_path(), format!("{}\n{}\n", output.name, current.name))
        .map_err(|e| format!("could not record the screen's mode: {e}"))?;

    let size = match &plan {
        Plan::Already => return Ok(()),
        Plan::Mode(mode) => {
            xrandr(&[
                "--output",
                &output.name,
                "--mode",
                &mode.name,
                "--scale",
                "1x1",
            ])?;
            mode.size
        }
        Plan::Scaled { base, size } => {
            let factors = scale_factors(base.size, *size);
            xrandr(&[
                "--output",
                &output.name,
                "--mode",
                &base.name,
                "--scale",
                &factors,
            ])?;
            // Scaling asks for a size indirectly, so the framebuffer is read
            // back and pinned if it came out a pixel off: an odd width would
            // reach the encoder as an odd width.
            if framebuffer() != Some(*size) {
                let _ = xrandr(&["--fb", &format!("{}x{}", size.0, size.1)]);
            }
            *size
        }
    };

    eprintln!(
        "dusk: streaming at {}x{}, was {}x{}{}",
        size.0,
        size.1,
        current.size.0,
        current.size.1,
        match plan {
            Plan::Scaled { .. } => " (scaled)",
            _ => "",
        }
    );
    Ok(())
}

/// Put the screen back. Nothing recorded means nothing was changed.
fn restore() -> Result<(), String> {
    let path = state_path();
    let Ok(recorded) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut lines = recorded.lines();
    let (Some(output), Some(mode)) = (lines.next(), lines.next()) else {
        let _ = std::fs::remove_file(&path);
        return Err("what the screen was on was not readable".into());
    };
    // `--scale 1x1` whether or not a scale was applied: resetting one that
    // was never set costs nothing, where leaving one that was would hold the
    // host's screen at the client's size after the stream had ended.
    let result = xrandr(&["--output", output, "--mode", mode, "--scale", "1x1"]);
    // Removed either way: a mode that cannot be restored now will not be
    // restorable at the end of the next stream either, and leaving the file
    // would have every later stream try it again.
    let _ = std::fs::remove_file(&path);
    result.map(|_| ())
}

/// Sunshine ran Dusk to switch the screen. `None` if it did not.
pub fn run_hook_if_asked() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != HOOK_FLAG {
        return None;
    }
    let action = args.next().unwrap_or_default();
    let result = match action.to_str() {
        Some(DO) => switch_to_client(),
        Some(UNDO) => restore(),
        other => Err(format!(
            "{} is not something to do",
            other.unwrap_or("that")
        )),
    };
    if let Err(err) = result {
        eprintln!("dusk: the screen was left as it was: {err}");
    }
    // Always nothing-went-wrong. Sunshine abandons a stream whose prep
    // command fails, and a stream at the host's own size is a far better
    // outcome than no stream at all.
    Some(0)
}

/// How Dusk names itself to Sunshine.
///
/// An AppImage's own path is inside a mount that is gone by the next run, so
/// `APPIMAGE` is asked first — the same order [`crate::updates`] uses.
fn own_command() -> Result<String, String> {
    let path = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .ok_or("Dusk could not work out where it is installed.")?;
    let path = path.to_string_lossy().into_owned();
    // Sunshine splits a prep command on whitespace. Quoting is only
    // plausible rather than measured, so a path with a space is at least not
    // silently mangled.
    Ok(if path.contains(char::is_whitespace) {
        format!("\"{path}\"")
    } else {
        path
    })
}

/// Dusk's own entry in `global_prep_cmd`.
fn entry(command: &str) -> serde_json::Value {
    serde_json::json!({
        "do": format!("{command} {HOOK_FLAG} {DO}"),
        "undo": format!("{command} {HOOK_FLAG} {UNDO}"),
        // A string and not a boolean. Measured: this is what Sunshine
        // writes into `sunshine.conf` and reads back.
        "elevated": "false",
    })
}

/// The commands already there, with any entry of Dusk's dropped.
///
/// `global_prep_cmd` is a JSON *string* holding an array, not an array —
/// measured against a live host, which reads back exactly what it was
/// given. An unreadable or absent value is treated as no commands rather
/// than as a failure: refusing to act would make the setting unusable on a
/// host whose config has something unexpected in it.
fn others(existing: Option<&str>) -> Vec<serde_json::Value> {
    let Some(text) = existing.map(str::trim).filter(|t| !t.is_empty()) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(text)
        .unwrap_or_default()
        .into_iter()
        .filter(|cmd| {
            !cmd.get("do")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|d| d.contains(HOOK_FLAG))
        })
        .collect()
}

/// `global_prep_cmd` with Dusk's entry first.
///
/// First because the screen has to be the right size before anything else
/// someone has asked to run around a stream looks at it. Any earlier copy of
/// Dusk's entry is dropped, so turning the setting on twice, or after Dusk
/// moves, leaves one.
pub fn prep_with(existing: Option<&str>, command: &str) -> String {
    let mut commands = vec![entry(command)];
    commands.extend(others(existing));
    serde_json::Value::Array(commands).to_string()
}

/// `global_prep_cmd` with Dusk's entry removed and everyone else's kept.
pub fn prep_without(existing: Option<&str>) -> String {
    serde_json::Value::Array(others(existing)).to_string()
}

/// Read the host's current `global_prep_cmd`.
async fn existing_prep(
    api: &crate::sunshine::SunshineApi,
    credentials: &crate::sunshine::Credentials,
) -> Result<Option<String>, String> {
    let config = api
        .get_config(credentials)
        .await
        .map_err(|e| e.to_string())?;
    Ok(config
        .get(PREP_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

async fn save_prep(
    api: &crate::sunshine::SunshineApi,
    credentials: &crate::sunshine::Credentials,
    value: String,
) -> Result<(), String> {
    let changes = [(PREP_KEY.to_string(), serde_json::Value::from(value))]
        .into_iter()
        .collect();
    api.save_config(credentials, changes)
        .await
        .map_err(|e| e.to_string())
}

/// Register Dusk as the thing that resizes the screen around a stream.
///
/// No restart: Sunshine reads its prep commands when a stream starts, and
/// restarting the host would drop one that is running.
pub async fn enable(
    api: &crate::sunshine::SunshineApi,
    credentials: &crate::sunshine::Credentials,
) -> Result<(), String> {
    if !available() {
        return Err("This machine's screen cannot be resized for a stream.".into());
    }
    let command = own_command()?;
    let existing = existing_prep(api, credentials).await?;
    save_prep(api, credentials, prep_with(existing.as_deref(), &command)).await
}

pub async fn disable(
    api: &crate::sunshine::SunshineApi,
    credentials: &crate::sunshine::Credentials,
) -> Result<(), String> {
    let existing = existing_prep(api, credentials).await?;
    save_prep(api, credentials, prep_without(existing.as_deref())).await?;
    // A stream interrupted by turning the setting off has no `undo` left to
    // run, so the screen is put back now rather than at the next stream.
    let _ = restore();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Venus, word for word, which is the shape this has to read.
    const VENUS: &str = "\
Screen 0: minimum 320 x 200, current 2560 x 1440, maximum 16384 x 16384
HDMI-A-0 disconnected (normal left inverted right x axis y axis)
HDMI-A-1 connected primary 2560x1440+0+0 (normal left inverted right x axis y axis) 480mm x 270mm
   1920x1080     60.00 + 120.00   119.88    74.99    50.00    24.00    23.98
   4096x2160     24.00    23.98
   3840x2160     30.00    30.00    25.00    24.00    29.97    23.98
   2880x1800     60.00
   2560x1600     59.97
   2560x1440     59.95*
   1920x1200     59.95
   1280x720      60.00    50.00    59.94
";

    #[test]
    fn the_connected_output_and_the_mode_it_is_showing_are_read() {
        let outputs = parse_query(VENUS);
        assert_eq!(
            outputs.len(),
            1,
            "the disconnected output is not one to use"
        );

        let output = &outputs[0];
        assert_eq!(output.name, "HDMI-A-1");
        assert!(output.primary);
        // From the `*`, not from `2560x1440+0+0`: the marker names the mode
        // and the geometry only gives its size.
        assert_eq!(
            output.current.as_ref().map(|m| m.name.as_str()),
            Some("2560x1440")
        );
        assert_eq!(output.sizes().len(), 8);
        assert!(output.sizes().contains(&(3840, 2160)));
    }

    #[test]
    fn the_mode_to_restore_is_the_one_in_use_and_not_the_preferred_one() {
        // Venus sits at 2560x1440 while `+` marks 1920x1080 as preferred, so
        // `xrandr --auto` would "restore" it to the wrong size. This is why
        // the mode in use is recorded rather than reached for afterwards.
        let output = parse_query(VENUS).remove(0);
        assert_eq!(output.current.map(|m| m.size), Some((2560, 1440)));
        assert_eq!(output.modes[0].size, (1920, 1080));
    }

    #[test]
    fn a_generated_mode_keeps_its_name_and_still_reports_its_size() {
        let outputs = parse_query(
            "DP-1 connected primary 1920x1080+0+0 (normal)\n   1920x1080_60.00  59.96*\n   1280x720i  60.00\n",
        );
        let modes = &outputs[0].modes;
        assert_eq!(modes[0].name, "1920x1080_60.00");
        assert_eq!(modes[0].size, (1920, 1080));
        assert_eq!(modes[1].name, "1280x720i");
        assert_eq!(modes[1].size, (1280, 720));
    }

    #[test]
    fn several_screens_with_no_primary_are_left_alone() {
        let two = "\
DP-1 connected 1920x1080+0+0 (normal)
   1920x1080     60.00*
DP-2 connected 1920x1080+1920+0 (normal)
   1920x1080     60.00*
";
        assert!(target(parse_query(two)).is_none());

        // One screen needs no marking to be the one.
        let one = "DP-1 connected 1920x1080+0+0 (normal)\n   1920x1080     60.00*\n";
        assert_eq!(
            target(parse_query(one)).map(|o| o.name),
            Some("DP-1".into())
        );
    }

    #[test]
    fn the_screen_line_gives_the_framebuffer_and_the_limit() {
        // The framebuffer is what a scaled screen changes and what a stream
        // captures; an output's mode stays put, so this line is the only
        // place the change shows.
        let (current, limit) = screen_line(VENUS).expect("the first line");
        assert_eq!(current, (2560, 1440));
        assert_eq!(limit, (16384, 16384));

        assert!(screen_line("HDMI-A-1 connected primary 2560x1440+0+0").is_none());
    }

    #[test]
    fn an_ultrawide_client_is_scaled_rather_than_given_up_on() {
        // The case the feature exists for, and the one it originally failed:
        // this Mac is 3440x1440 and Venus's monitor lists no ultrawide mode,
        // so snapping to the nearest listed size returned the size it
        // already was and nothing happened at all.
        let output = parse_query(VENUS).remove(0);
        let plan = plan_for(&output, (3440, 1440), (16384, 16384)).expect("a plan");
        match plan {
            Plan::Scaled { base, size } => {
                assert_eq!(size, (3440, 1440), "the client gets its exact size");
                assert_eq!(
                    base.size,
                    (2560, 1440),
                    "the panel keeps the mode it already had, so it keeps its signal"
                );
            }
            other => panic!("expected a scaled plan, got {other:?}"),
        }
        // Measured against a live X server: these are the factors that put
        // the framebuffer exactly on 3440x1440.
        assert_eq!(
            scale_factors((2560, 1440), (3440, 1440)),
            "1.343750x1.000000"
        );
    }

    #[test]
    fn a_listed_mode_is_preferred_to_scaling_it() {
        let output = parse_query(VENUS).remove(0);
        // 1920x1080 is listed, so the panel is driven at it properly.
        assert_eq!(
            plan_for(&output, (1920, 1080), (16384, 16384)),
            Some(Plan::Mode(Mode {
                name: "1920x1080".into(),
                size: (1920, 1080)
            }))
        );
        // The size it is already at needs nothing doing, and must not be
        // recorded as a change or the restore would be spurious.
        assert_eq!(
            plan_for(&output, (2560, 1440), (16384, 16384)),
            Some(Plan::Already)
        );
    }

    #[test]
    fn an_impossible_size_is_turned_down_rather_than_attempted() {
        let output = parse_query(VENUS).remove(0);
        assert_eq!(plan_for(&output, (99999, 1440), (16384, 16384)), None);
        assert_eq!(plan_for(&output, (0, 1080), (16384, 16384)), None);
        // A client reporting an odd size gets an even one: encoders want
        // even dimensions and the framebuffer would take the odd one.
        match plan_for(&output, (3441, 1441), (16384, 16384)) {
            Some(Plan::Scaled { size, .. }) => assert_eq!(size, (3440, 1440)),
            other => panic!("expected a scaled plan, got {other:?}"),
        }
    }

    #[test]
    fn dusks_entry_goes_first_and_never_twice() {
        let mine =
            serde_json::from_str::<Vec<serde_json::Value>>(&prep_with(None, "/usr/bin/dusk"))
                .expect("an array");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["do"], "/usr/bin/dusk --dusk-match-screen do");
        assert_eq!(mine[0]["undo"], "/usr/bin/dusk --dusk-match-screen undo");
        // A string, as Sunshine writes it.
        assert_eq!(mine[0]["elevated"], "false");

        // Turning it on again, or after Dusk has moved, leaves one entry —
        // the new one — and keeps what someone else put there.
        let theirs = r#"[{"do":"/opt/warm-up.sh","undo":"","elevated":"false"},
                         {"do":"/old/dusk --dusk-match-screen do","undo":"/old/dusk --dusk-match-screen undo","elevated":"false"}]"#;
        let merged =
            serde_json::from_str::<Vec<serde_json::Value>>(&prep_with(Some(theirs), "/new/dusk"))
                .expect("an array");
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["do"], "/new/dusk --dusk-match-screen do");
        assert_eq!(merged[1]["do"], "/opt/warm-up.sh");
    }

    #[test]
    fn turning_it_off_keeps_everyone_elses_commands() {
        let existing = r#"[{"do":"/opt/warm-up.sh","undo":"/opt/cool-down.sh","elevated":"false"},
                           {"do":"/usr/bin/dusk --dusk-match-screen do","undo":"/usr/bin/dusk --dusk-match-screen undo","elevated":"false"}]"#;
        let left = serde_json::from_str::<Vec<serde_json::Value>>(&prep_without(Some(existing)))
            .expect("an array");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0]["do"], "/opt/warm-up.sh");
    }

    #[test]
    fn a_config_with_nothing_usable_in_it_is_treated_as_empty() {
        // Absent, blank, and not JSON at all. None of these may stop the
        // setting being turned on — the alternative is a host nobody can
        // use the feature on because of something unrelated in its config.
        for existing in [None, Some(""), Some("   "), Some("not json"), Some("{}")] {
            let made = serde_json::from_str::<Vec<serde_json::Value>>(&prep_with(
                existing,
                "/usr/bin/dusk",
            ))
            .expect("an array");
            assert_eq!(made.len(), 1, "{existing:?}");
        }
    }

    #[test]
    fn a_path_with_a_space_is_quoted_rather_than_split() {
        let made = prep_with(None, "\"/home/a b/dusk\"");
        assert!(made.contains("\\\"/home/a b/dusk\\\" --dusk-match-screen do"));
    }
}
