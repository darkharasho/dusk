//! Streaming from the virtual display, sized to whoever connects.
//!
//! A Windows host with the virtual display driver installed can stream a
//! screen that is the connecting machine's shape, so a 3440x1440 ultrawide
//! gets a 3440x1440 desktop with no bars, whatever the host's own monitor is.
//! Three things make that happen, and this module owns all three:
//!
//! - **The driver has to know the size.** It only offers the modes listed in
//!   its settings file, and only rereads that file when its device restarts,
//!   which needs administrator rights. Asking mid-stream would put a prompt
//!   on a screen nobody is looking at, so the list is written once, when the
//!   setting is turned on, and is generous: [`MODES`] covers the screens
//!   people actually have.
//! - **Sunshine has to stream it.** `output_name` picks the display and
//!   `dd_configuration_option` makes it the only one for the length of a
//!   stream, at the client's resolution and refresh rate. Sunshine has no
//!   API that lists displays, so the virtual one's id is read from the list
//!   it logs at startup.
//! - **The client has to ask for a size the driver has.** A mode the driver
//!   does not list makes Sunshine undo the whole display change and stream
//!   the physical monitor instead. So the host answers the client's
//!   question about size with the closest mode it really has; see
//!   [`best_mode`].
//!
//! Only Windows has the driver, so elsewhere most of this goes unused.
#![cfg_attr(not(windows), allow(dead_code))]

/// Screen sizes written into the driver's list, in real pixels.
///
/// The common monitors in each shape, plus the laptops and handhelds whose
/// panels are a size of their own. A size missing here still streams, just
/// at the nearest listed size of the same shape.
pub const MODES: &[(u32, u32)] = &[
    // 16:9
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3200, 1800),
    (3840, 2160),
    // 16:10, and the Steam Deck
    (1280, 800),
    (1440, 900),
    (1680, 1050),
    (1920, 1200),
    (2560, 1600),
    (2880, 1800),
    (3840, 2400),
    // Ultrawide
    (2560, 1080),
    (3440, 1440),
    (3840, 1600),
    (5120, 2160),
    // Super ultrawide
    (3840, 1080),
    (5120, 1440),
    // 3:2
    (2256, 1504),
    (2880, 1920),
    (3000, 2000),
    // MacBooks
    (2560, 1664),
    (2880, 1864),
    (3024, 1964),
    (3456, 2234),
];

/// Refresh rates offered at every size. Sunshine sets the client's frame
/// rate along with its size, and a rate the driver lacks fails the same way
/// a size does.
pub const RATES: &[u32] = &[30, 60, 75, 90, 100, 120, 144, 165, 240];

/// The name the driver's built-in EDID gives its monitor, which is what
/// Sunshine logs as the display's friendly name.
const FRIENDLY_NAME: &str = "VDD by MTT";

/// What Sunshine logs just before the displays it can see.
const DEVICES_MARKER: &str = "Currently available display devices:";

/// Sunshine settings that stream the virtual display at the client's size.
///
/// `ensure_only_display` turns the host's other monitors off for the length
/// of a stream: without that, Windows keeps the taskbar and new windows on
/// the primary monitor, and the stream shows an empty desktop. Resolution
/// and refresh rate are left at Sunshine's default, `auto`, which is "what
/// the client asked for", but written so a value set by hand cannot quietly
/// undo the setting.
pub fn sunshine_settings(device_id: &str) -> serde_json::Map<String, serde_json::Value> {
    [
        ("output_name", device_id),
        ("dd_configuration_option", "ensure_only_display"),
        ("dd_resolution_option", "auto"),
        ("dd_refresh_rate_option", "auto"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
    .collect()
}

/// Sunshine settings that put streaming back on the host's own screen.
pub fn sunshine_defaults() -> serde_json::Map<String, serde_json::Value> {
    [("output_name", ""), ("dd_configuration_option", "disabled")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
        .collect()
}

/// The size to stream at for a client whose screen is `want`, from the
/// sizes the driver has.
///
/// Its own size if the driver has it. Otherwise the largest listed size of
/// the same shape that fits inside it, which streams without bars and is
/// scaled up by the client; then the largest that fits at all; and for a
/// screen smaller than anything listed, the smallest of its shape. `None`
/// when nothing is close enough to be better than the host's own screen.
pub fn best_mode(want: (u32, u32), have: &[(u32, u32)]) -> Option<(u32, u32)> {
    if have.contains(&want) {
        return Some(want);
    }
    let (w, h) = (u64::from(want.0), u64::from(want.1));
    // Within 1%: 2560x1600 and 1440x900 are the same shape; 1366x768 and
    // 1920x1080 are not quite, but close enough that nobody sees the bars.
    let same_shape = |&&(mw, mh): &&(u32, u32)| {
        let (a, b) = (u64::from(mw) * h, u64::from(mh) * w);
        a.abs_diff(b) * 100 <= a.max(b)
    };
    let fits = |&&(mw, mh): &&(u32, u32)| mw <= want.0 && mh <= want.1;
    let area = |&&(mw, mh): &&(u32, u32)| u64::from(mw) * u64::from(mh);

    have.iter()
        .filter(same_shape)
        .filter(fits)
        .max_by_key(area)
        .or_else(|| have.iter().filter(fits).max_by_key(area))
        .or_else(|| have.iter().filter(same_shape).min_by_key(area))
        .copied()
}

/// The sizes listed in the driver's settings file.
pub fn listed_modes(xml: &str) -> Vec<(u32, u32)> {
    let Some(block) = inner(xml, "resolutions") else {
        return Vec::new();
    };
    let mut modes = Vec::new();
    let mut rest = block;
    while let Some(entry) = inner(rest, "resolution") {
        let size = (number(entry, "width"), number(entry, "height"));
        if let (Some(w), Some(h)) = size {
            if !modes.contains(&(w, h)) {
                modes.push((w, h));
            }
        }
        // Past this entry's closing tag, to the next one.
        let end = rest.find("</resolution>").map_or(rest.len(), |i| i + 13);
        rest = &rest[end..];
    }
    modes
}

/// The driver's settings with [`MODES`] and [`RATES`] added to its lists.
///
/// Everything already listed stays, so a size someone added by hand is not
/// lost, and everything outside the two lists is left exactly as it was.
/// A file that is missing or not the driver's starts from the driver's own
/// defaults.
pub fn with_modes(xml: Option<&str>) -> String {
    let xml = match xml {
        Some(xml) if xml.contains("</vdd_settings>") => xml,
        _ => DEFAULT_SETTINGS,
    };

    let mut modes = listed_modes(xml);
    for mode in MODES {
        if !modes.contains(mode) {
            modes.push(*mode);
        }
    }
    let mut rates: Vec<u32> = inner(xml, "global")
        .map(|block| all_numbers(block, "g_refresh_rate"))
        .unwrap_or_default();
    for rate in RATES {
        if !rates.contains(rate) {
            rates.push(*rate);
        }
    }
    rates.sort_unstable();

    let mut resolutions = String::from("<resolutions>\n");
    for (w, h) in &modes {
        resolutions.push_str(&format!(
            "        <resolution>\n            <width>{w}</width>\n            \
             <height>{h}</height>\n            <refresh_rate>60</refresh_rate>\n        \
             </resolution>\n"
        ));
    }
    resolutions.push_str("    </resolutions>");

    let mut global = String::from("<global>\n");
    for rate in &rates {
        global.push_str(&format!(
            "        <g_refresh_rate>{rate}</g_refresh_rate>\n"
        ));
    }
    global.push_str("    </global>");

    let xml = replace_block(xml, "global", &global);
    replace_block(&xml, "resolutions", &resolutions)
}

/// The virtual display's id in the list Sunshine logs at startup: what its
/// `output_name` setting wants. The last list in the log, since the log
/// may hold more than one start.
pub fn device_id_from_log(log: &str) -> Option<String> {
    let at = log.rfind(DEVICES_MARKER)? + DEVICES_MARKER.len();
    let list: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&log[at..])
        .into_iter()
        .next()?
        .ok()?;
    list.iter()
        .find(|device| {
            device
                .get("friendly_name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.trim().eq_ignore_ascii_case(FRIENDLY_NAME))
        })
        .and_then(|device| device.get("device_id")?.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------- xml bits
//
// The driver's file is small, flat and written by hand as often as not, with
// comments a parser and writer would reorder or drop. Editing only the two
// blocks Dusk owns, as text, leaves the rest exactly as someone left it.

/// The text between `<tag>` and `</tag>`, first occurrence.
fn inner<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&close)?;
    Some(&xml[start..end])
}

fn number(xml: &str, tag: &str) -> Option<u32> {
    inner(xml, tag)?.trim().parse().ok()
}

fn all_numbers(xml: &str, tag: &str) -> Vec<u32> {
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(value) = inner(rest, tag) {
        if let Ok(n) = value.trim().parse() {
            if !found.contains(&n) {
                found.push(n);
            }
        }
        let end = rest.find(&close).map_or(rest.len(), |i| i + close.len());
        rest = &rest[end..];
    }
    found
}

/// `xml` with the whole `<tag>…</tag>` block replaced by `block`, or with
/// `block` added before the closing root tag if there was none.
fn replace_block(xml: &str, tag: &str, block: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(start) = xml.find(&open) {
        if let Some(len) = xml[start..].find(&close) {
            let end = start + len + close.len();
            return format!("{}{}{}", &xml[..start], block, &xml[end..]);
        }
    }
    match xml.rfind("</vdd_settings>") {
        Some(at) => format!("{}    {}\n{}", &xml[..at], block, &xml[at..]),
        None => xml.to_string(),
    }
}

/// The driver's own defaults, as its package ships them, minus the comments.
const DEFAULT_SETTINGS: &str = "<?xml version='1.0' encoding='utf-8'?>
<vdd_settings>
    <monitors>
        <count>1</count>
    </monitors>
    <gpu>
        <friendlyname>default</friendlyname>
    </gpu>
    <global>
    </global>
    <resolutions>
    </resolutions>
    <options>
        <CustomEdid>false</CustomEdid>
        <PreventSpoof>false</PreventSpoof>
        <EdidCeaOverride>false</EdidCeaOverride>
        <HardwareCursor>true</HardwareCursor>
        <SDR10bit>false</SDR10bit>
        <HDRPlus>false</HDRPlus>
        <logging>false</logging>
        <debuglogging>false</debuglogging>
    </options>
</vdd_settings>
";

// ----------------------------------------------------------------- windows

/// Turn streaming from the virtual display on: list the sizes, restart the
/// driver so it reads them, then point Sunshine at it. One administrator
/// prompt, for the restart.
#[cfg(windows)]
pub async fn enable(
    api: &crate::sunshine::SunshineApi,
    credentials: &crate::sunshine::Credentials,
) -> Result<(), String> {
    use crate::host::service;
    use crate::install::display_driver;

    let instance =
        display_driver::instance_id().ok_or("Install the virtual display first, from Setup.")?;

    let target = display_driver::settings_file();
    let current = std::fs::read_to_string(&target).ok();
    let staged = std::env::temp_dir().join("dusk-vdd_settings.xml");
    std::fs::write(&staged, with_modes(current.as_deref()))
        .map_err(|e| format!("could not write the display settings: {e}"))?;

    let script = restart_script(
        &staged.to_string_lossy(),
        &target.to_string_lossy(),
        &instance,
    );
    let run = service::run_elevated(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &service::encode_powershell(&script),
        ],
        service::ELEVATED_TIMEOUT,
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&staged);
    if !run.ok() {
        return Err("Windows did not let Dusk set up the virtual display.".into());
    }

    // Sunshine only lists displays as it starts, and it has to see the
    // driver's new modes too, so it is restarted before the log is read.
    let log = sunshine_log().ok_or("Dusk could not find the host's log.")?;
    let before = modified(&log);
    api.restart(credentials).await.map_err(|e| e.to_string())?;
    let device_id = wait_for_device_id(&log, before)
        .await
        .ok_or("The host did not see the virtual display. Restart this machine and try again.")?;

    api.save_config(credentials, sunshine_settings(&device_id))
        .await
        .map_err(|e| e.to_string())?;
    api.restart(credentials).await.map_err(|e| e.to_string())
}

/// Turn it off: streams show the host's own screen again. The driver's list
/// is left alone; it costs nothing and saves a prompt if it is turned back
/// on.
#[cfg(windows)]
pub async fn disable(
    api: &crate::sunshine::SunshineApi,
    credentials: &crate::sunshine::Credentials,
) -> Result<(), String> {
    api.save_config(credentials, sunshine_defaults())
        .await
        .map_err(|e| e.to_string())?;
    api.restart(credentials).await.map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub async fn enable(
    _api: &crate::sunshine::SunshineApi,
    _credentials: &crate::sunshine::Credentials,
) -> Result<(), String> {
    Err("A virtual display is only offered on Windows.".into())
}

#[cfg(not(windows))]
pub async fn disable(
    _api: &crate::sunshine::SunshineApi,
    _credentials: &crate::sunshine::Credentials,
) -> Result<(), String> {
    Ok(())
}

/// The sizes the driver has right now, for answering a client. Empty when
/// the file cannot be read, which makes [`best_mode`] answer `None`.
pub fn current_modes() -> Vec<(u32, u32)> {
    #[cfg(windows)]
    {
        std::fs::read_to_string(crate::install::display_driver::settings_file())
            .map(|xml| listed_modes(&xml))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// Copy the new list into place and restart the driver's device. `pnputil`
/// has restarted devices since Windows 10 2004; the device is disabled and
/// enabled again where it cannot.
#[cfg_attr(not(windows), allow(dead_code))]
fn restart_script(staged: &str, target: &str, instance: &str) -> String {
    use crate::host::service::powershell_quote as q;
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         New-Item -ItemType Directory -Force -Path (Split-Path {target}) | Out-Null\n\
         Copy-Item -LiteralPath {staged} -Destination {target} -Force\n\
         & pnputil.exe /restart-device {instance} | Out-Null\n\
         if ($LASTEXITCODE -ne 0) {{\n\
         \x20   Disable-PnpDevice -InstanceId {instance} -Confirm:$false\n\
         \x20   Enable-PnpDevice -InstanceId {instance} -Confirm:$false\n\
         }}\n\
         exit 0",
        target = q(target),
        staged = q(staged),
        instance = q(instance),
    )
}

#[cfg(windows)]
fn sunshine_log() -> Option<std::path::PathBuf> {
    std::env::var_os("ProgramFiles")
        .map(|base| std::path::PathBuf::from(base).join(r"Sunshine\config\sunshine.log"))
}

#[cfg(windows)]
fn modified(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Wait for Sunshine to start again and log the virtual display. Sunshine
/// starts its log afresh each time, so a log changed since `before` that
/// names the display is from this start.
#[cfg(windows)]
async fn wait_for_device_id(
    log: &std::path::Path,
    before: Option<std::time::SystemTime>,
) -> Option<String> {
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if before.is_some() && modified(log) <= before {
            continue;
        }
        if let Some(id) = std::fs::read_to_string(log)
            .ok()
            .and_then(|text| device_id_from_log(&text))
        {
            return Some(id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = include_str!("../../tests/fixtures/vdd_settings.xml");

    #[test]
    fn the_shipped_file_lists_its_five_sizes() {
        assert_eq!(
            listed_modes(SHIPPED),
            vec![
                (800, 600),
                (1366, 768),
                (1920, 1080),
                (2560, 1440),
                (3840, 2160)
            ]
        );
    }

    #[test]
    fn adding_modes_keeps_what_was_there_and_adds_the_ultrawide() {
        let xml = with_modes(Some(SHIPPED));
        let modes = listed_modes(&xml);
        assert!(modes.contains(&(800, 600)), "a size already listed stays");
        assert!(modes.contains(&(3440, 1440)));
        for mode in MODES {
            assert!(modes.contains(mode), "{mode:?} missing");
        }
        assert_eq!(
            modes.iter().filter(|m| **m == (1920, 1080)).count(),
            1,
            "a size in both lists is listed once"
        );
    }

    #[test]
    fn adding_modes_keeps_the_rates_and_options_and_comments() {
        let xml = with_modes(Some(SHIPPED));
        let rates = all_numbers(inner(&xml, "global").unwrap(), "g_refresh_rate");
        assert!(rates.contains(&244), "a rate already listed stays");
        assert!(rates.contains(&60) && rates.contains(&75));
        assert!(xml.contains("<HardwareCursor>true</HardwareCursor>"));
        assert!(xml.contains("Custom Edid should be named"));
        assert!(xml.contains("<count>1</count>"));
        assert_eq!(xml.matches("<resolutions>").count(), 1);
        assert_eq!(xml.matches("<global>").count(), 1);
    }

    #[test]
    fn adding_modes_twice_changes_nothing_the_second_time() {
        let once = with_modes(Some(SHIPPED));
        assert_eq!(with_modes(Some(&once)), once);
    }

    #[test]
    fn a_missing_or_foreign_file_starts_from_the_drivers_defaults() {
        for xml in [None, Some(""), Some("<html></html>")] {
            let written = with_modes(xml);
            assert!(written.contains("</vdd_settings>"));
            assert!(listed_modes(&written).contains(&(3440, 1440)));
        }
    }

    #[test]
    fn a_file_without_the_blocks_gets_them_inside_the_root() {
        let xml = "<vdd_settings>\n    <monitors><count>1</count></monitors>\n</vdd_settings>\n";
        let written = with_modes(Some(xml));
        assert!(listed_modes(&written).contains(&(3440, 1440)));
        assert!(written.trim_end().ends_with("</vdd_settings>"));
        assert!(written.contains("<count>1</count>"));
    }

    #[test]
    fn a_listed_size_is_streamed_as_it_is() {
        assert_eq!(best_mode((3440, 1440), MODES), Some((3440, 1440)));
        assert_eq!(best_mode((1280, 800), MODES), Some((1280, 800)));
    }

    #[test]
    fn an_unlisted_size_gets_the_largest_of_its_shape_that_fits() {
        // A 16:9 screen between two listed 16:9 sizes takes the smaller.
        let have = [(1920, 1080), (2560, 1440), (3840, 2160)];
        assert_eq!(best_mode((3000, 1688), &have), Some((2560, 1440)));
        // 1366x768 is close enough to 16:9 to take 1280x720 over nothing.
        assert_eq!(best_mode((1366, 768), MODES), Some((1280, 720)));
        // A 5K ultrawide not in the list takes the 21:9 size under it.
        assert_eq!(best_mode((4000, 1674), MODES), Some((3840, 1600)));
    }

    #[test]
    fn a_shape_with_nothing_listed_gets_the_largest_that_fits() {
        assert_eq!(best_mode((2048, 1536), MODES), Some((1920, 1200)));
    }

    #[test]
    fn a_screen_smaller_than_everything_gets_the_smallest_of_its_shape() {
        assert_eq!(best_mode((1024, 576), MODES), Some((1280, 720)));
        assert_eq!(best_mode((640, 480), &[(1920, 1080)]), None);
    }

    #[test]
    fn nothing_listed_means_no_answer() {
        assert_eq!(best_mode((3440, 1440), &[]), None);
    }

    #[test]
    fn the_device_id_is_found_by_the_drivers_monitor_name() {
        let log = r#"[2026-10-20 09:00:00.000]: Info: Sunshine version: v2026.914.233613
[2026-10-20 09:00:00.100]: Info: Currently available display devices:
[
  {
    "device_id": "{11111111-2222-3333-4444-555555555555}",
    "display_name": "\\\\.\\DISPLAY1",
    "friendly_name": "AW3423DW",
    "info": {"primary": true}
  },
  {
    "device_id": "{daeac860-f4db-5208-b1f5-cf59444fb768}",
    "display_name": "",
    "friendly_name": "VDD by MTT",
    "info": null
  }
]
[2026-10-20 09:00:00.200]: Info: Configuration UI available at [https://localhost:47990]
"#;
        assert_eq!(
            device_id_from_log(log).as_deref(),
            Some("{daeac860-f4db-5208-b1f5-cf59444fb768}")
        );
    }

    #[test]
    fn the_last_start_in_the_log_is_the_one_believed() {
        let entry = |id: &str| {
            format!(
                "Info: Currently available display devices:\n[{{\"device_id\":\"{id}\",\"friendly_name\":\"VDD by MTT\"}}]\n"
            )
        };
        let log = entry("{old}") + &entry("{new}");
        assert_eq!(device_id_from_log(&log).as_deref(), Some("{new}"));
    }

    #[test]
    fn a_log_without_the_virtual_display_has_no_id() {
        assert_eq!(device_id_from_log("nothing here"), None);
        let log = "Currently available display devices:\n[{\"device_id\":\"{x}\",\"friendly_name\":\"DELL\"}]";
        assert_eq!(device_id_from_log(log), None);
        assert_eq!(
            device_id_from_log("Currently available display devices:\n[ truncated"),
            None
        );
    }

    #[test]
    fn the_settings_stream_only_the_virtual_display_at_the_clients_size() {
        let s = sunshine_settings("{abc}");
        assert_eq!(s["output_name"], "{abc}");
        assert_eq!(s["dd_configuration_option"], "ensure_only_display");
        assert_eq!(s["dd_resolution_option"], "auto");
        assert_eq!(s["dd_refresh_rate_option"], "auto");
        let d = sunshine_defaults();
        assert_eq!(d["output_name"], "");
        assert_eq!(d["dd_configuration_option"], "disabled");
    }

    #[test]
    fn the_restart_script_quotes_every_path_and_falls_back() {
        let s = restart_script(
            r"C:\T\a b.xml",
            r"C:\VirtualDisplayDriver\vdd_settings.xml",
            r"ROOT\DISPLAY\0000",
        );
        assert!(s.contains(r"-LiteralPath 'C:\T\a b.xml'"));
        assert!(s.contains(r"/restart-device 'ROOT\DISPLAY\0000'"));
        assert!(s.contains("Disable-PnpDevice"));
        assert!(s.starts_with("$ErrorActionPreference = 'Stop'"));
    }
}
