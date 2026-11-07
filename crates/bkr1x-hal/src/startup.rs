use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use bkr1x_protocol::{ButtonMapping, MouseConfig, PollingRate};

use crate::icon::{TrayIconStatus, encode_ico_file};
use crate::ipc::{IpcRequest, send_ipc_request};
use crate::profile::HardwareProfile;
use crate::sfx::{SfxBundle, extract_sfx_from_exe};

pub const STARTUP_REG_VALUE_NAME: &str = "FirePhoenixBKR1X";
pub const UNINSTALL_REG_SUBKEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\FirePhoenixBKR1X";
pub const GUI_WINDOW_TITLE: &str = "Fire Phoenix BK-R1X — Native PAW3311 Configurator";

pub fn user_config_dir() -> PathBuf {
    #[cfg(windows)]
    if let Ok(appdata) = env::var("APPDATA") {
        return PathBuf::from(appdata).join("bkr1x");
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

pub fn ensure_user_profiles_dir() -> PathBuf {
    let dir = user_config_dir().join("profiles");
    let _ = fs::create_dir_all(&dir);

    let has_existing = fs::read_dir(&dir)
        .ok()
        .is_some_and(|mut it| it.any(|entry| entry.is_ok()));
    if !has_existing {
        seed_default_profiles(&dir);
    }
    dir
}

fn seed_default_profiles(dir: &Path) {
    let mut desktop_cfg = MouseConfig::factory_default();
    desktop_cfg.dpi_stages = [400, 800, 1200, 1600, 3200, 5000];
    desktop_cfg.active_dpi_stage = 1;
    desktop_cfg.polling_rate = PollingRate::Hz1000;
    let desktop_prof = HardwareProfile::from_hardware(
        "Desktop Daily (800 DPI)",
        &desktop_cfg,
        &ButtonMapping::factory_default(),
    );
    let _ = desktop_prof.save_to_file(&dir.join("desktop-800dpi.toml"));

    let mut esports_cfg = MouseConfig::factory_default();
    esports_cfg.dpi_stages = [400, 800, 1600, 3200, 5000, 18000];
    esports_cfg.active_dpi_stage = 2;
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

pub fn list_saved_profiles() -> Vec<(PathBuf, HardwareProfile)> {
    let dir = ensure_user_profiles_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if is_profile_extension(&path)
            && let Ok(prof) = HardwareProfile::load_from_file(&path)
        {
            profiles.push((path, prof));
        }
    }
    profiles.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    profiles
}

pub fn is_profile_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml") || ext.eq_ignore_ascii_case("json"))
}

pub fn user_install_dir() -> PathBuf {
    #[cfg(windows)]
    if let Ok(local) = env::var("LOCALAPPDATA") {
        return PathBuf::from(local).join("Programs").join("BKR1X");
    }
    #[cfg(not(windows))]
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".local").join("bin");
    }
    PathBuf::from(".")
}

pub fn platform_exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

pub fn resolve_sibling_binary(bin_stem: &str) -> Option<PathBuf> {
    let exe_name = platform_exe_name(bin_stem);
    if let Ok(curr) = env::current_exe()
        && let Some(parent) = curr.parent()
    {
        let sibling = parent.join(&exe_name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let installed = user_install_dir().join(&exe_name);
    installed.is_file().then_some(installed)
}

pub fn is_autostart_enabled() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
        };

        let subkey = wide_null("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let val_name = wide_null(STARTUP_REG_VALUE_NAME);

        // SAFETY: Read-only query on `HKCU\...\Run` with properly null-terminated UTF-16 strings.
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
        user_config_dir().parent().is_some_and(|cfg_root| {
            cfg_root
                .join("autostart")
                .join("bkr1x-daemon.desktop")
                .is_file()
        })
    }
}

pub fn set_autostart_enabled(
    enable: bool,
    daemon_exe_override: Option<&Path>,
) -> Result<(), String> {
    let daemon_path = match daemon_exe_override {
        Some(path) => path.to_path_buf(),
        None => resolve_sibling_binary("bkr1x-daemon")
            .ok_or_else(|| "could not locate bkr1x-daemon binary".to_owned())?,
    };

    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
            RegSetValueExW,
        };

        let subkey = wide_null("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let val_name = wide_null(STARTUP_REG_VALUE_NAME);

        // SAFETY: Opens `HKCU\...\Run` with `KEY_SET_VALUE` and closes `hkey` before returning.
        unsafe {
            let mut hkey = std::ptr::null_mut();
            let open_rc = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            );
            if open_rc != 0 {
                return Err(format!("RegOpenKeyExW failed with Win32 code {open_rc}"));
            }

            let result = if enable {
                let wide_cmd = wide_null(&format!("\"{}\"", daemon_path.display()));
                let rc = RegSetValueExW(
                    hkey,
                    val_name.as_ptr(),
                    0,
                    REG_SZ,
                    wide_cmd.as_ptr().cast::<u8>(),
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
            result
        }
    }

    #[cfg(not(windows))]
    {
        let autostart_dir = user_config_dir()
            .parent()
            .map(|p| p.join("autostart"))
            .ok_or_else(|| "cannot resolve XDG autostart directory".to_owned())?;
        let desktop_file = autostart_dir.join("bkr1x-daemon.desktop");
        if !enable {
            let _ = fs::remove_file(&desktop_file);
            return Ok(());
        }
        fs::create_dir_all(&autostart_dir).map_err(|e| e.to_string())?;
        let content = format!(
            "[Desktop Entry]\nType=Application\nName=Fire Phoenix BK-R1X Daemon\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            daemon_path.display()
        );
        fs::write(&desktop_file, content).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub enable_autostart: bool,
    pub create_start_menu_shortcut: bool,
    pub create_desktop_shortcut: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            enable_autostart: true,
            create_start_menu_shortcut: true,
            create_desktop_shortcut: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallSummary {
    pub install_dir: PathBuf,
    pub daemon_exe: PathBuf,
    pub gui_exe: PathBuf,
    pub start_menu_shortcut: Option<PathBuf>,
    pub desktop_shortcut: Option<PathBuf>,
    pub autostart_registered: bool,
}

pub fn is_user_application_installed() -> bool {
    let dir = user_install_dir();
    dir.join(platform_exe_name("bkr1x-daemon")).is_file()
        && dir.join(platform_exe_name("bkr1x-gui")).is_file()
}

pub fn install_user_application(enable_autostart: bool) -> Result<InstallSummary, String> {
    install_with_options(&InstallOptions {
        enable_autostart,
        create_start_menu_shortcut: true,
        create_desktop_shortcut: false,
    })
}

pub fn install_with_options(opts: &InstallOptions) -> Result<InstallSummary, String> {
    close_running_bkr1x_processes();

    let curr_exe =
        env::current_exe().map_err(|e| format!("cannot determine current executable path: {e}"))?;
    let src_dir = curr_exe
        .parent()
        .ok_or_else(|| "cannot determine current executable directory".to_owned())?;

    let dest_dir = user_install_dir();
    fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("failed creating {}: {e}", dest_dir.display()))?;
    let _ = ensure_user_profiles_dir();

    let (daemon_exe, gui_exe) = match extract_sfx_from_exe(&curr_exe)? {
        Some(bundle) => unpack_sfx_binaries(&curr_exe, &bundle, &dest_dir)?,
        None => copy_sibling_binaries(src_dir, &dest_dir)?,
    };

    let ico_path = dest_dir.join("bkr1x.ico");
    let _ = fs::write(
        &ico_path,
        encode_ico_file(&[16, 32, 48, 64], TrayIconStatus::default()),
    );

    let (start_menu_shortcut, desktop_shortcut) = create_requested_shortcuts(
        opts.create_start_menu_shortcut,
        opts.create_desktop_shortcut,
        &gui_exe,
        &ico_path,
    );

    set_autostart_enabled(opts.enable_autostart, Some(&daemon_exe))?;

    #[cfg(windows)]
    register_windows_uninstall_entry(&dest_dir, &gui_exe);

    Ok(InstallSummary {
        install_dir: dest_dir,
        daemon_exe,
        gui_exe,
        start_menu_shortcut,
        desktop_shortcut,
        autostart_registered: opts.enable_autostart,
    })
}

fn unpack_sfx_binaries(
    curr_exe: &Path,
    bundle: &SfxBundle,
    dest_dir: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    for (fname, bytes) in &bundle.files {
        let safe_name = Path::new(fname)
            .file_name()
            .ok_or_else(|| format!("invalid embedded filename '{fname}'"))?;
        let dst = dest_dir.join(safe_name);
        fs::write(&dst, bytes).map_err(|e| format!("failed writing {}: {e}", dst.display()))?;
    }

    #[cfg(windows)]
    if let Ok(full_self) = fs::read(curr_exe)
        && let Ok(stub_len) = usize::try_from(bundle.stub_len)
        && stub_len <= full_self.len()
    {
        let _ = fs::write(
            dest_dir.join("Uninstall BK-R1X.exe"),
            &full_self[..stub_len],
        );
    }
    #[cfg(not(windows))]
    let _ = curr_exe;

    Ok((
        dest_dir.join(platform_exe_name("bkr1x-daemon")),
        dest_dir.join(platform_exe_name("bkr1x-gui")),
    ))
}

fn copy_sibling_binaries(src_dir: &Path, dest_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let copy_bin = |stem: &str| -> Result<PathBuf, String> {
        let fname = platform_exe_name(stem);
        let src = src_dir.join(&fname);
        let dst = dest_dir.join(&fname);
        if src.is_file() && src != dst {
            fs::copy(&src, &dst).map_err(|e| {
                format!("failed copying {} -> {}: {e}", src.display(), dst.display())
            })?;
        }
        if dst.is_file() {
            Ok(dst)
        } else if src.is_file() {
            Ok(src)
        } else {
            Err(format!(
                "missing compiled binary '{fname}' in {}",
                src_dir.display()
            ))
        }
    };

    let daemon_exe = copy_bin("bkr1x-daemon")?;
    let gui_exe = copy_bin("bkr1x-gui")?;
    let _ = copy_bin("bkr1x-cli");
    if cfg!(windows)
        && let Ok(uninst_src) = copy_bin("bkr1x-installer")
    {
        let _ = fs::copy(&uninst_src, dest_dir.join("Uninstall BK-R1X.exe"));
    }
    Ok((daemon_exe, gui_exe))
}

pub fn uninstall_user_application() -> Result<(), String> {
    uninstall_user_application_full(false)
}

pub fn uninstall_user_application_full(remove_binaries: bool) -> Result<(), String> {
    close_running_bkr1x_processes();
    let _ = set_autostart_enabled(false, None);

    #[cfg(windows)]
    {
        unregister_windows_uninstall_entry();
        if let Some(lnk) = windows_start_menu_shortcut_path() {
            let _ = fs::remove_file(lnk);
        }
        if let Some(desk) = windows_desktop_shortcut_path() {
            let _ = fs::remove_file(desk);
        }
    }

    if remove_binaries {
        let dir = user_install_dir();
        for stem in ["bkr1x-daemon", "bkr1x-gui", "bkr1x-cli", "bkr1x-installer"] {
            let _ = fs::remove_file(dir.join(platform_exe_name(stem)));
        }
        let _ = fs::remove_file(dir.join("bkr1x.ico"));

        #[cfg(windows)]
        {
            let uninst_exe = dir.join("Uninstall BK-R1X.exe");
            if env::current_exe().ok().as_deref() == Some(uninst_exe.as_path()) {
                schedule_self_delete_on_exit(&uninst_exe, &dir);
            } else {
                let _ = fs::remove_file(&uninst_exe);
                let _ = fs::remove_dir(&dir);
            }
        }
    }

    Ok(())
}

fn close_running_bkr1x_processes() {
    let _ = send_ipc_request(&IpcRequest::Shutdown);

    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
        let wide_title = wide_null(GUI_WINDOW_TITLE);
        // SAFETY: `wide_title` is null-terminated UTF-16; `PostMessageW` is safe on non-null `HWND`.
        unsafe {
            let hwnd = FindWindowW(std::ptr::null(), wide_title.as_ptr());
            if !hwnd.is_null() {
                let _ = PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }

    std::thread::sleep(std::time::Duration::from_millis(300));
}

#[cfg(windows)]
fn wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn register_windows_uninstall_entry(install_dir: &Path, gui_exe: &Path) {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };

    let subkey = wide_null(UNINSTALL_REG_SUBKEY);

    // SAFETY: Writes standard per-user `HKCU\...\Uninstall` values and closes `hkey` on exit.
    unsafe {
        let mut hkey = std::ptr::null_mut();
        let mut disposition = 0u32;
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            &mut disposition,
        ) != 0
        {
            return;
        }

        let set_sz = |name: &str, val: &str| {
            let wn = wide_null(name);
            let wv = wide_null(val);
            let _ = RegSetValueExW(
                hkey,
                wn.as_ptr(),
                0,
                REG_SZ,
                wv.as_ptr().cast::<u8>(),
                (wv.len() * 2) as u32,
            );
        };
        let set_dword = |name: &str, val: u32| {
            let wn = wide_null(name);
            let bytes = val.to_le_bytes();
            let _ = RegSetValueExW(hkey, wn.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4);
        };

        let uninst_path = install_dir.join("Uninstall BK-R1X.exe");
        let (uninst_cmd, quiet_cmd) = if uninst_path.is_file() {
            (
                format!("\"{}\" --uninstall", uninst_path.display()),
                format!("\"{}\" --uninstall --silent", uninst_path.display()),
            )
        } else {
            let fallback = format!(
                "\"{}\" uninstall",
                install_dir.join("bkr1x-cli.exe").display()
            );
            (fallback.clone(), fallback)
        };

        set_sz("DisplayName", "Fire Phoenix BK-R1X Driver");
        set_sz("DisplayVersion", env!("CARGO_PKG_VERSION"));
        set_sz("Publisher", "Fire Phoenix BK-R1X Open Driver");
        set_sz("DisplayIcon", &format!("{},0", gui_exe.display()));
        set_sz("InstallLocation", &install_dir.display().to_string());
        set_sz("UninstallString", &uninst_cmd);
        set_sz("QuietUninstallString", &quiet_cmd);
        set_dword("NoModify", 1);
        set_dword("NoRepair", 1);
        set_dword("EstimatedSize", 5450);

        let _ = RegCloseKey(hkey);
    }
}

#[cfg(windows)]
fn unregister_windows_uninstall_entry() {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyW};
    let subkey = wide_null(UNINSTALL_REG_SUBKEY);
    // SAFETY: Deletes only the application's own subkey under `HKCU`.
    unsafe {
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, subkey.as_ptr());
    }
}

#[cfg(windows)]
fn schedule_self_delete_on_exit(uninst_exe: &Path, dir: &Path) {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Windows locks the running `Uninstall BK-R1X.exe` image until our process terminates.
    let cmd_str = format!(
        "ping 127.0.0.1 -n 2 > nul & del /f /q \"{}\" & rmdir \"{}\" 2>nul",
        uninst_exe.display(),
        dir.display()
    );
    let _ = Command::new("cmd.exe")
        .args(["/C", &cmd_str])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(windows)]
fn windows_start_menu_shortcut_path() -> Option<PathBuf> {
    let appdata = env::var("APPDATA").ok()?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft\\Windows\\Start Menu\\Programs\\Fire Phoenix BK-R1X.lnk"),
    )
}

#[cfg(windows)]
fn windows_desktop_shortcut_path() -> Option<PathBuf> {
    let profile = env::var("USERPROFILE").ok()?;
    let desktop = PathBuf::from(profile).join("Desktop");
    desktop
        .is_dir()
        .then(|| desktop.join("Fire Phoenix BK-R1X.lnk"))
}

#[cfg(windows)]
fn escape_ps_single_quoted(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

#[cfg(windows)]
fn create_requested_shortcuts(
    want_start_menu: bool,
    want_desktop: bool,
    gui_exe: &Path,
    ico_path: &Path,
) -> (Option<PathBuf>, Option<PathBuf>) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let start_menu_lnk = want_start_menu
        .then(windows_start_menu_shortcut_path)
        .flatten();
    let desktop_lnk = want_desktop.then(windows_desktop_shortcut_path).flatten();
    if start_menu_lnk.is_none() && desktop_lnk.is_none() {
        return (None, None);
    }

    let Some(work_dir) = gui_exe.parent() else {
        return (None, None);
    };

    // Batch all requested `.lnk` files into one PowerShell invocation to avoid double startup cost.
    let mut script = String::from("$ws = New-Object -ComObject WScript.Shell; ");
    let target_escaped = escape_ps_single_quoted(gui_exe);
    let work_escaped = escape_ps_single_quoted(work_dir);
    let ico_escaped = escape_ps_single_quoted(ico_path);

    for lnk in [start_menu_lnk.as_deref(), desktop_lnk.as_deref()]
        .into_iter()
        .flatten()
    {
        let lnk_escaped = escape_ps_single_quoted(lnk);
        script.push_str(&format!(
            "$s = $ws.CreateShortcut('{lnk_escaped}'); \
             $s.TargetPath = '{target_escaped}'; \
             $s.WorkingDirectory = '{work_escaped}'; \
             $s.IconLocation = '{ico_escaped},0'; \
             $s.Description = '{GUI_WINDOW_TITLE}'; \
             $s.Save(); "
        ));
    }

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let ok = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|st| st.success());

    if ok {
        (start_menu_lnk, desktop_lnk)
    } else {
        (None, None)
    }
}

#[cfg(not(windows))]
fn create_requested_shortcuts(
    _want_start_menu: bool,
    _want_desktop: bool,
    _gui_exe: &Path,
    _ico_path: &Path,
) -> (Option<PathBuf>, Option<PathBuf>) {
    (None, None)
}
