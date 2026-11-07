//! Per-user application directories, profile seeding, Windows `HKCU\...\Run` startup registration,
//! and Start Menu / user-programs installation (`%LOCALAPPDATA%\Programs\BKR1X`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use bkr1x_protocol::{ButtonMapping, MouseConfig, PollingRate};

use crate::icon::{TrayIconStatus, encode_ico_file};
use crate::ipc::{IpcRequest, send_ipc_request};
use crate::profile::HardwareProfile;

/// Registry value name under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub const STARTUP_REG_VALUE_NAME: &str = "FirePhoenixBKR1X";

/// Returns the user configuration root (`%APPDATA%\bkr1x` on Windows, `~/.config/bkr1x` on Linux).
pub fn user_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = env::var("APPDATA") {
            return PathBuf::from(appdata).join("bkr1x");
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join("bkr1x");
        }
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(".config").join("bkr1x");
        }
    }
    PathBuf::from("bkr1x-config")
}

/// Returns the user profiles directory (`%APPDATA%\bkr1x\profiles`), creating & seeding defaults if empty.
pub fn ensure_user_profiles_dir() -> PathBuf {
    let dir = user_config_dir().join("profiles");
    let _ = fs::create_dir_all(&dir);

    // Seed two starter profiles if none exist yet so the Tray & GUI Profile menus work out-of-the-box
    let has_any = fs::read_dir(&dir)
        .ok()
        .is_some_and(|mut it| it.any(|e| e.is_ok()));
    if !has_any {
        let mut desktop_cfg = MouseConfig::factory_default();
        desktop_cfg.dpi_stages = [400, 800, 1200, 1600, 3200, 5000];
        desktop_cfg.active_dpi_stage = 1; // 800 DPI
        desktop_cfg.polling_rate = PollingRate::Hz1000;
        let desktop_prof = HardwareProfile::from_hardware(
            "Desktop Daily (800 DPI)",
            &desktop_cfg,
            &ButtonMapping::factory_default(),
        );
        let _ = desktop_prof.save_to_file(&dir.join("desktop-800dpi.toml"));

        let mut esports_cfg = MouseConfig::factory_default();
        esports_cfg.dpi_stages = [400, 800, 1600, 3200, 5000, 18000];
        esports_cfg.active_dpi_stage = 2; // 1600 DPI
        esports_cfg.polling_rate = PollingRate::Hz1000;
        esports_cfg.sensor_flags.motion_sync = true;
        esports_cfg.sensor_flags.angle_snapping = false;
        esports_cfg.sensor_flags.ripple_control = false;
        esports_cfg.highspeed_mode = true;
        let esports_prof = HardwareProfile::from_hardware(
            "Esports Competitive (1600 DPI)",
            &esports_cfg,
            &ButtonMapping::factory_default(),
        );
        let _ = esports_prof.save_to_file(&dir.join("esports-1600dpi.toml"));
    }

    dir
}

/// Lists all valid `.toml` and `.json` profiles inside `ensure_user_profiles_dir()`.
pub fn list_saved_profiles() -> Vec<(PathBuf, HardwareProfile)> {
    let dir = ensure_user_profiles_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let is_cfg = p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("toml") || e.eq_ignore_ascii_case("json"));
        if is_cfg && let Ok(prof) = HardwareProfile::load_from_file(&p) {
            out.push((p, prof));
        }
    }
    out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    out
}

/// Returns the standard per-user installation folder (`%LOCALAPPDATA%\Programs\BKR1X` on Windows).
pub fn user_install_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(local) = env::var("LOCALAPPDATA") {
            return PathBuf::from(local).join("Programs").join("BKR1X");
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(".local").join("bin");
        }
    }
    PathBuf::from(".")
}

/// Resolves a sibling binary (`bkr1x-gui.exe` or `bkr1x-daemon.exe`) next to the currently running executable,
/// falling back to `user_install_dir()` if needed.
pub fn resolve_sibling_binary(bin_stem: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        format!("{bin_stem}.exe")
    } else {
        bin_stem.to_owned()
    };

    if let Ok(curr) = env::current_exe()
        && let Some(parent) = curr.parent()
    {
        let sibling = parent.join(&exe_name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let installed = user_install_dir().join(&exe_name);
    if installed.is_file() {
        return Some(installed);
    }

    None
}

/// Checks whether BK-R1X is registered to launch automatically when the user logs into the OS.
pub fn is_autostart_enabled() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
        };

        let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let val_name: Vec<u16> = STARTUP_REG_VALUE_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: Read-only query on standard HKCU Run key.
        unsafe {
            let mut hkey = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                return false;
            }
            let mut reg_type = 0u32;
            let mut data_len = 0u32;
            let status = RegQueryValueExW(
                hkey,
                val_name.as_ptr(),
                std::ptr::null(),
                &mut reg_type,
                std::ptr::null_mut(),
                &mut data_len,
            );
            let _ = RegCloseKey(hkey);
            status == 0 && reg_type == REG_SZ && data_len > 2
        }
    }

    #[cfg(not(windows))]
    {
        user_config_dir()
            .parent()
            .map(|cfg_root| {
                cfg_root
                    .join("autostart")
                    .join("bkr1x-daemon.desktop")
                    .is_file()
            })
            .unwrap_or(false)
    }
}

/// Enables or disables automatic launch of `bkr1x-daemon` at OS login.
pub fn set_autostart_enabled(enable: bool, daemon_exe_override: Option<&Path>) -> Result<(), String> {
    let daemon_path = match daemon_exe_override {
        Some(p) => p.to_path_buf(),
        None => resolve_sibling_binary("bkr1x-daemon")
            .ok_or_else(|| "could not locate bkr1x-daemon binary".to_owned())?,
    };

    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
            RegSetValueExW,
        };

        let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let val_name: Vec<u16> = STARTUP_REG_VALUE_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: Opens HKCU\Software\Microsoft\Windows\CurrentVersion\Run with KEY_SET_VALUE.
        unsafe {
            let mut hkey = std::ptr::null_mut();
            let open_rc =
                RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_SET_VALUE, &mut hkey);
            if open_rc != 0 {
                return Err(format!("RegOpenKeyExW failed with Win32 code {open_rc}"));
            }

            let res = if enable {
                let cmd = format!("\"{}\"", daemon_path.display());
                let wide_cmd: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
                let rc = RegSetValueExW(
                    hkey,
                    val_name.as_ptr(),
                    0,
                    REG_SZ,
                    wide_cmd.as_ptr() as *const u8,
                    (wide_cmd.len() * 2) as u32,
                );
                if rc == 0 {
                    Ok(())
                } else {
                    Err(format!("RegSetValueExW failed with Win32 code {rc}"))
                }
            } else {
                let _ = RegDeleteValueW(hkey, val_name.as_ptr());
                Ok(())
            };

            let _ = RegCloseKey(hkey);
            res
        }
    }

    #[cfg(not(windows))]
    {
        let autostart_dir = user_config_dir()
            .parent()
            .map(|p| p.join("autostart"))
            .ok_or_else(|| "cannot resolve XDG autostart directory".to_owned())?;
        let desktop_file = autostart_dir.join("bkr1x-daemon.desktop");
        if enable {
            fs::create_dir_all(&autostart_dir).map_err(|e| e.to_string())?;
            let content = format!(
                "[Desktop Entry]\nType=Application\nName=Fire Phoenix BK-R1X Daemon\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
                daemon_path.display()
            );
            fs::write(&desktop_file, content).map_err(|e| e.to_string())
        } else {
            let _ = fs::remove_file(&desktop_file);
            Ok(())
        }
    }
}

/// Summary returned after installing BK-R1X into `%LOCALAPPDATA%\Programs\BKR1X`.
#[derive(Debug, Clone)]
pub struct InstallSummary {
    pub install_dir: PathBuf,
    pub daemon_exe: PathBuf,
    pub gui_exe: PathBuf,
    pub start_menu_shortcut: Option<PathBuf>,
    pub autostart_registered: bool,
}

/// Installs `bkr1x-daemon`, `bkr1x-gui`, `bkr1x-cli`, icon assets, Start Menu shortcut,
/// and Windows login auto-start into the user's profile (`%LOCALAPPDATA%\Programs\BKR1X`).
pub fn install_user_application(enable_autostart: bool) -> Result<InstallSummary, String> {
    // Gracefully stop any running daemon instance so binary files on Windows can be overwritten cleanly
    let _ = send_ipc_request(&IpcRequest::Shutdown);
    std::thread::sleep(std::time::Duration::from_millis(250));

    let src_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| "cannot determine current executable directory".to_owned())?;

    let dest_dir = user_install_dir();
    fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("failed creating {}: {e}", dest_dir.display()))?;

    let _ = ensure_user_profiles_dir();

    let copy_bin = |stem: &str| -> Result<PathBuf, String> {
        let fname = if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_owned()
        };
        let src = src_dir.join(&fname);
        let dst = dest_dir.join(&fname);
        if src.is_file() && src != dst {
            fs::copy(&src, &dst)
                .map_err(|e| format!("failed copying {} -> {}: {e}", src.display(), dst.display()))?;
        }
        if dst.is_file() {
            Ok(dst)
        } else if src.is_file() {
            Ok(src)
        } else {
            Err(format!("missing compiled binary '{}' in {}", fname, src_dir.display()))
        }
    };

    let daemon_exe = copy_bin("bkr1x-daemon")?;
    let gui_exe = copy_bin("bkr1x-gui")?;
    let _ = copy_bin("bkr1x-cli");

    let ico_path = dest_dir.join("bkr1x.ico");
    let ico_bytes = encode_ico_file(&[16, 32, 48, 64], TrayIconStatus::default());
    let _ = fs::write(&ico_path, ico_bytes);

    let shortcut = create_start_menu_shortcut(&gui_exe, &ico_path);

    if enable_autostart {
        set_autostart_enabled(true, Some(&daemon_exe))?;
    }

    Ok(InstallSummary {
        install_dir: dest_dir,
        daemon_exe,
        gui_exe,
        start_menu_shortcut: shortcut,
        autostart_registered: enable_autostart,
    })
}

/// Removes the Windows Startup entry and Start Menu shortcut (and stops any running daemon).
pub fn uninstall_user_application() -> Result<(), String> {
    let _ = send_ipc_request(&IpcRequest::Shutdown);
    let _ = set_autostart_enabled(false, None);
    #[cfg(windows)]
    if let Ok(appdata) = env::var("APPDATA") {
        let lnk = PathBuf::from(appdata)
            .join("Microsoft\\Windows\\Start Menu\\Programs\\Fire Phoenix BK-R1X.lnk");
        let _ = fs::remove_file(lnk);
    }
    Ok(())
}

#[cfg(windows)]
fn create_start_menu_shortcut(gui_exe: &Path, ico_path: &Path) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let appdata = env::var("APPDATA").ok()?;
    let lnk_path = PathBuf::from(appdata)
        .join("Microsoft\\Windows\\Start Menu\\Programs\\Fire Phoenix BK-R1X.lnk");

    let ps_script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.IconLocation = '{},0'; \
         $s.Description = 'Fire Phoenix BK-R1X Native PAW3311 Configurator'; \
         $s.Save()",
        lnk_path.display().to_string().replace('\'', "''"),
        gui_exe.display().to_string().replace('\'', "''"),
        gui_exe
            .parent()?
            .display()
            .to_string()
            .replace('\'', "''"),
        ico_path.display().to_string().replace('\'', "''"),
    );

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .ok()?;

    status.success().then_some(lnk_path)
}

#[cfg(not(windows))]
fn create_start_menu_shortcut(_gui_exe: &Path, _ico_path: &Path) -> Option<PathBuf> {
    None
}
