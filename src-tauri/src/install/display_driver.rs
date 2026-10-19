//! A virtual display for a Windows host, installed only when asked.
//!
//! Streaming with no monitor attached, or at a resolution the attached one
//! does not have, needs Windows to believe a display is there. That takes an
//! indirect display driver, which Sunshine does not ship. Dusk offers the
//! Virtual Display Driver from github.com/VirtualDrivers (MIT, signed through
//! SignPath), and nothing else, because:
//!
//! - It is a user-mode (UMDF/IddCx) driver, not a kernel one. A fault in it
//!   takes down a display, not the machine.
//! - It is the driver Sunshine's own documentation and community point at,
//!   so it is the one most likely to have met the setups Dusk will meet.
//! - Its releases carry a driver-only package, so Dusk installs four files
//!   rather than running someone else's installer with admin rights.
//!
//! Two rules hold it at arm's length:
//!
//! - **Pinned, not "latest".** The release and the package's checksum are
//!   written here. A new upstream release changes nothing until someone
//!   reads it and moves the pin, the same as `SUNSHINE_TAG`.
//! - **Asked for, never assumed.** The step is never part of "Set up this
//!   machine". It is a separate press with its own explanation, because a
//!   driver is a different kind of thing to put on someone's machine than
//!   a firewall rule.
//!
//! Downloaded when asked rather than bundled: most hosts have a monitor and
//! never need it, and not redistributing it keeps Dusk's licence notices to
//! the two programs it actually ships.

/// The release the package below comes from.
pub const VERSION: &str = "25.7.23";

#[cfg_attr(not(windows), allow(dead_code))]
const ASSET: &str = "VirtualDisplayDriver-x86.Driver.Only.zip";

/// sha256 of [`ASSET`], taken when the pin was last moved. Unlike a digest
/// read from the release API at install time, this one does not come from
/// the same place as the file, so it also catches the release changing
/// underneath the pin.
#[cfg_attr(not(windows), allow(dead_code))]
const SHA256: &str = "e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a";

/// What the driver's INF matches. Creating a device with this ID is what
/// makes Windows install the driver onto it.
#[cfg_attr(not(windows), allow(dead_code))]
const HARDWARE_ID: &str = r"Root\MttVDD";

/// Where the driver reads its monitor list from unless the registry names
/// somewhere else. Without a file there it falls back to its own defaults,
/// but the one in the package is the documented starting point.
#[cfg_attr(not(windows), allow(dead_code))]
const SETTINGS_DIR: &str = r"C:\VirtualDisplayDriver";

/// The argument that turns a second, elevated Dusk into the installer.
pub const HELPER_FLAG: &str = "--dusk-install-display-driver";

#[cfg_attr(not(windows), allow(dead_code))]
pub fn asset() -> super::release::Asset {
    super::release::Asset {
        name: ASSET.to_string(),
        url: format!(
            "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/{VERSION}/{ASSET}"
        ),
        // Not used for the check, which is against the pinned hash; only
        // for progress, where zero means "unknown".
        size: 0,
        sha256: Some(SHA256.to_string()),
    }
}

/// Whether a device for this driver already exists.
///
/// Read from the registry's device tree, which any account can read, so the
/// checklist can say "done" without asking for anything. Every root
/// enumerated device is looked at rather than only `ROOT\DISPLAY`, because
/// the driver's own control app and Dusk need not pick the same name.
#[cfg(windows)]
pub fn installed() -> bool {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    let Ok(root) =
        RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SYSTEM\CurrentControlSet\Enum\ROOT")
    else {
        return false;
    };
    root.enum_keys().flatten().any(|class| {
        let Ok(class) = root.open_subkey(&class) else {
            return false;
        };
        class.enum_keys().flatten().any(|instance| {
            class
                .open_subkey(&instance)
                .and_then(|key| key.get_value::<Vec<String>, _>("HardwareID"))
                .is_ok_and(|ids| ids.iter().any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)))
        })
    })
}

#[cfg(not(windows))]
pub fn installed() -> bool {
    false
}

/// Download, check, unpack and install. Asks for administrator rights once.
///
/// The elevated part is Dusk itself, started again with [`HELPER_FLAG`]:
/// making a device node is a SetupAPI call, not a command Windows ships, and
/// running our own code for it beats bundling Microsoft's `devcon`, which
/// is not redistributable.
#[cfg(windows)]
pub async fn install(client: &reqwest::Client, dir: &std::path::Path) -> Result<(), String> {
    use crate::host::service;

    if installed() {
        return Ok(());
    }

    let zip = super::download::fetch(client, &asset(), dir, |_| {})
        .await
        .map_err(|e| e.to_string())?;

    // Windows has carried bsdtar since 1803, and it reads zip. Unpacked into
    // a fresh directory so nothing from an earlier attempt is mixed in.
    let unpacked = dir.join("virtual-display");
    let _ = tokio::fs::remove_dir_all(&unpacked).await;
    tokio::fs::create_dir_all(&unpacked)
        .await
        .map_err(|e| e.to_string())?;
    let tar = std::env::var_os("SystemRoot")
        .map(|root| std::path::PathBuf::from(root).join(r"System32\tar.exe"))
        .unwrap_or_else(|| "tar.exe".into());
    let untar = service::run(
        &tar.to_string_lossy(),
        &[
            "-xf",
            &zip.to_string_lossy(),
            "-C",
            &unpacked.to_string_lossy(),
        ],
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(&zip).await;
    if !untar.ok() {
        return Err(format!("could not unpack the driver: {}", untar.message()));
    }

    let inf = unpacked.join(r"VirtualDisplayDriver\MttVDD.inf");
    if !inf.is_file() {
        return Err("the driver package was not laid out as expected".into());
    }

    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let run = service::run_elevated(
        &me.to_string_lossy(),
        &[HELPER_FLAG, &inf.to_string_lossy()],
        service::ELEVATED_TIMEOUT,
    )
    .await
    .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_dir_all(&unpacked).await;

    match run.status {
        Some(0) => Ok(()),
        Some(code) if code == helper::REBOOT => {
            Err("The virtual display is installed. Restart this machine to finish.".into())
        }
        Some(code) => Err(format!(
            "The driver did not install (error {code}). Nothing was left half done."
        )),
        None => Err(format!("The driver did not install: {}", run.message())),
    }
}

#[cfg(not(windows))]
pub async fn install(_client: &reqwest::Client, _dir: &std::path::Path) -> Result<(), String> {
    Err("A virtual display driver is only offered on Windows.".into())
}

/// Run as the elevated helper if that is what this process was started as.
/// Returns the exit code to leave with, or `None` to start Dusk normally.
pub fn run_helper_if_asked() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != HELPER_FLAG {
        return None;
    }
    let inf = std::path::PathBuf::from(args.next()?);
    Some(helper::install(&inf))
}

mod helper {
    /// Installed, and Windows wants a restart before it is used.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub const REBOOT: i32 = 3010;

    /// Make a root device carrying the driver's hardware ID, then install
    /// the INF onto it. What `devcon install` does, in the same order.
    ///
    /// If the driver will not go on, the device made for it is removed
    /// again, so a failure does not leave an unknown device in Device
    /// Manager. Exit codes are Win32 errors, since the elevated process has
    /// no other way to say what went wrong.
    #[cfg(windows)]
    pub fn install(inf: &std::path::Path) -> i32 {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
        use windows_sys::Win32::Foundation::{GetLastError, FALSE, INVALID_HANDLE_VALUE};

        fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
            s.encode_wide().chain(Some(0)).collect()
        }

        copy_settings(inf);

        let inf_w = wide(inf.as_os_str());
        // A REG_MULTI_SZ: the ID, its terminator, and the list's terminator.
        let ids: Vec<u16> = super::HARDWARE_ID.encode_utf16().chain([0, 0]).collect();
        let class_name = wide(std::ffi::OsStr::new("Display"));

        // SAFETY: plain SetupAPI calls on buffers that outlive them; the
        // device info set is destroyed on every path out.
        unsafe {
            let set = SetupDiCreateDeviceInfoList(&GUID_DEVCLASS_DISPLAY, std::ptr::null_mut());
            if set == INVALID_HANDLE_VALUE as HDEVINFO {
                return GetLastError() as i32;
            }
            let mut device: SP_DEVINFO_DATA = std::mem::zeroed();
            device.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;

            let fail = |set: HDEVINFO| {
                let code = GetLastError() as i32;
                SetupDiDestroyDeviceInfoList(set);
                if code == 0 {
                    1
                } else {
                    code
                }
            };

            if SetupDiCreateDeviceInfoW(
                set,
                class_name.as_ptr(),
                &GUID_DEVCLASS_DISPLAY,
                std::ptr::null(),
                std::ptr::null_mut(),
                DICD_GENERATE_ID,
                &mut device,
            ) == FALSE
            {
                return fail(set);
            }
            if SetupDiSetDeviceRegistryPropertyW(
                set,
                &mut device,
                SPDRP_HARDWAREID,
                ids.as_ptr().cast(),
                (ids.len() * 2) as u32,
            ) == FALSE
            {
                return fail(set);
            }
            if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, &device) == FALSE {
                return fail(set);
            }

            let hwid = wide(std::ffi::OsStr::new(super::HARDWARE_ID));
            let mut reboot = FALSE;
            if UpdateDriverForPlugAndPlayDevicesW(
                std::ptr::null_mut(),
                hwid.as_ptr(),
                inf_w.as_ptr(),
                INSTALLFLAG_FORCE,
                &mut reboot,
            ) == FALSE
            {
                let code = GetLastError() as i32;
                SetupDiCallClassInstaller(DIF_REMOVE, set, &device);
                SetupDiDestroyDeviceInfoList(set);
                return if code == 0 { 1 } else { code };
            }

            SetupDiDestroyDeviceInfoList(set);
            if reboot != FALSE {
                REBOOT
            } else {
                0
            }
        }
    }

    /// Put the package's monitor list where the driver looks, unless one is
    /// already there: it may be someone's own, from an earlier install.
    #[cfg(windows)]
    fn copy_settings(inf: &std::path::Path) {
        let Some(source) = inf.parent().map(|dir| dir.join("vdd_settings.xml")) else {
            return;
        };
        let dir = std::path::Path::new(super::SETTINGS_DIR);
        let target = dir.join("vdd_settings.xml");
        if target.exists() || !source.is_file() {
            return;
        }
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::copy(&source, &target);
        }
    }

    #[cfg(not(windows))]
    pub fn install(_inf: &std::path::Path) -> i32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_is_a_full_sha256() {
        assert_eq!(SHA256.len(), 64);
        assert!(SHA256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn the_download_is_the_pinned_release_and_carries_the_pinned_hash() {
        let a = asset();
        assert!(a.url.contains(&format!("/download/{VERSION}/")));
        assert!(a.url.ends_with(ASSET));
        assert_eq!(a.sha256.as_deref(), Some(SHA256));
    }
}
