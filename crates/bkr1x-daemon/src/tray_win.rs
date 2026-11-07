//! Native Win32 System Tray (`Shell_NotifyIconW`) controller for `bkr1x-daemon`.
//!
//! Provides a zero-idle-CPU (`GetMessageW` kernel wait) G HUB-style taskbar tray icon:
//! - Left-click / Double-click: Launches `bkr1x-gui.exe` (or focuses it if already open).
//! - Right-click: Opens native popup menu with live battery status, Quick DPI switcher,
//!   Polling Rate switcher, saved Profile switcher, "Launch at Windows Startup" toggle, and Quit.
//! - Automatically restores the tray icon if Windows `explorer.exe` restarts (`TaskbarCreated`).

#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bkr1x_hal::icon::{TrayIconStatus, create_win32_tray_hicon};
use bkr1x_hal::{
    DaemonStatusSnapshot, IpcResponse, is_autostart_enabled, list_saved_profiles,
    resolve_sibling_binary, set_autostart_enabled,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, HICON, HMENU,
    KillTimer, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_RESTORE, SetForegroundWindow,
    SetMenuDefaultItem, SetTimer, ShowWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER, WM_USER,
    WNDCLASSW, WS_OVERLAPPED,
};

use crate::DaemonCommand;

/// Window title of `bkr1x-gui` used to bring an existing configurator window to the foreground.
pub const GUI_WINDOW_TITLE: &str = "Fire Phoenix BK-R1X — Native PAW3311 Configurator";

const WM_TRAY_CALLBACK: u32 = WM_USER + 10;
const WM_TRAY_REFRESH: u32 = WM_USER + 11;
const WM_TRAY_OPEN_GUI: u32 = WM_USER + 12;
const WM_TRAY_QUIT: u32 = WM_USER + 13;
const TRAY_ICON_UID: u32 = 1;
const TIMER_RETRY_TRAY_ADD: usize = 77;

// Menu command IDs
const CMD_OPEN_GUI: u32 = 1001;
const CMD_REFRESH_NOW: u32 = 1002;
const CMD_TOGGLE_AUTOSTART: u32 = 1003;
const CMD_QUIT_ALL: u32 = 1004;
const CMD_DPI_BASE: u32 = 2000;
const CMD_RATE_BASE: u32 = 3000;
const CMD_PROFILE_BASE: u32 = 4000;

const QUICK_DPI_VALUES: [u16; 8] = [400, 800, 1200, 1600, 2400, 3200, 5000, 18000];
const QUICK_RATE_VALUES: [u16; 4] = [125, 250, 500, 1000];

/// Thread-safe handle allowing background threads to notify the Win32 Tray loop.
#[derive(Clone, Default)]
pub struct TrayNotifier {
    hwnd_raw: Arc<AtomicIsize>,
}

impl TrayNotifier {
    pub fn notify_state_changed(&self) {
        let raw = self.hwnd_raw.load(Ordering::Acquire);
        if raw != 0 {
            unsafe {
                let _ = PostMessageW(raw as HWND, WM_TRAY_REFRESH, 0, 0);
            }
        }
    }

    pub fn request_open_gui(&self) {
        let raw = self.hwnd_raw.load(Ordering::Acquire);
        if raw != 0 {
            unsafe {
                let _ = PostMessageW(raw as HWND, WM_TRAY_OPEN_GUI, 0, 0);
            }
        } else {
            launch_or_focus_gui();
        }
    }

    pub fn request_quit(&self) {
        let raw = self.hwnd_raw.load(Ordering::Acquire);
        if raw != 0 {
            unsafe {
                let _ = PostMessageW(raw as HWND, WM_TRAY_QUIT, 0, 0);
            }
        }
    }
}

struct TrayContext {
    snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
    taskbar_created_msg: u32,
    current_icon: HICON,
    icon_added: bool,
    retry_attempts: u8,
}

thread_local! {
    static TRAY_CTX: std::cell::RefCell<Option<TrayContext>> = const { std::cell::RefCell::new(None) };
}

fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Launches `bkr1x-gui.exe` or restores & focuses the existing configurator window if already open.
pub fn launch_or_focus_gui() {
    let wide_title = wide_null(GUI_WINDOW_TITLE);
    unsafe {
        let existing = FindWindowW(std::ptr::null(), wide_title.as_ptr());
        if !existing.is_null() {
            let _ = ShowWindow(existing, SW_RESTORE);
            let _ = SetForegroundWindow(existing);
            return;
        }
    }

    if let Some(gui_exe) = resolve_sibling_binary("bkr1x-gui") {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new(&gui_exe);
        if let Some(parent) = gui_exe.parent() {
            cmd.current_dir(parent);
        }
        let _ = cmd.creation_flags(CREATE_NO_WINDOW).spawn();
    }
}

/// Gracefully closes any open `bkr1x-gui` window when the user clicks "Quit BK-R1X" in the tray.
fn close_open_gui_window() {
    let wide_title = wide_null(GUI_WINDOW_TITLE);
    unsafe {
        let existing = FindWindowW(std::ptr::null(), wide_title.as_ptr());
        if !existing.is_null() {
            let _ = PostMessageW(existing, WM_CLOSE, 0, 0);
        }
    }
}

/// Runs the blocking zero-CPU Win32 message loop for the System Tray icon on the main thread.
pub fn run_system_tray_loop(
    notifier: TrayNotifier,
    snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
) {
    let class_name = wide_null("BKR1X_SystemTrayHostClass");
    let win_name = wide_null("BKR1X_TrayHost");
    let taskbar_msg_name = wide_null("TaskbarCreated");

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let taskbar_created_msg = RegisterWindowMessageW(taskbar_msg_name.as_ptr());

        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(tray_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        let _ = RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            win_name.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }

        let init_status = extract_tray_icon_status(&snapshot);
        let init_icon = create_win32_tray_hicon(32, init_status);
        let added = add_or_update_notify_icon(hwnd, NIM_ADD, init_icon, &format_tooltip(&snapshot));

        TRAY_CTX.with(|cell| {
            *cell.borrow_mut() = Some(TrayContext {
                snapshot: Arc::clone(&snapshot),
                cmd_tx: cmd_tx.clone(),
                running: Arc::clone(&running),
                taskbar_created_msg,
                current_icon: init_icon,
                icon_added: added,
                retry_attempts: 0,
            });
        });

        // If `explorer.exe`'s taskbar tray isn't ready yet during fast Windows boot, retry every 1.5s
        // and stop the timer permanently once registered so idle CPU stays at 0.0%.
        if !added {
            let _ = SetTimer(hwnd, TIMER_RETRY_TRAY_ADD, 1500, None);
        }
        notifier.hwnd_raw.store(hwnd as isize, Ordering::Release);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }

        notifier.hwnd_raw.store(0, Ordering::Release);
        remove_notify_icon(hwnd);
        TRAY_CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().take()
                && !ctx.current_icon.is_null()
            {
                let _ = DestroyIcon(ctx.current_icon);
            }
        });
        let _ = DestroyWindow(hwnd);
    }
}

fn extract_tray_icon_status(snapshot: &Arc<RwLock<DaemonStatusSnapshot>>) -> TrayIconStatus {
    if let Ok(s) = snapshot.read() {
        TrayIconStatus {
            connected: s.connected,
            online: s.online,
            battery_percent: s.battery_percent,
            charging: s.charging,
        }
    } else {
        TrayIconStatus::default()
    }
}

fn format_tooltip(snapshot: &Arc<RwLock<DaemonStatusSnapshot>>) -> String {
    let Ok(s) = snapshot.read() else {
        return "Fire Phoenix BK-R1X".to_owned();
    };
    if !s.connected {
        return "Fire Phoenix BK-R1X — Disconnected\nClick to open configurator".to_owned();
    }
    let state_tag = if s.charging {
        "Charging"
    } else if !s.online {
        "Sleep"
    } else {
        &s.connection_mode
    };
    format!(
        "Fire Phoenix BK-R1X ({state_tag})\nBattery: {}% · {} DPI · {} Hz",
        s.battery_percent, s.active_dpi, s.polling_rate_hz
    )
}

const NOTIFYICONDATAW_COMPAT_SIZE: u32 = std::mem::offset_of!(NOTIFYICONDATAW, guidItem) as u32;

unsafe fn add_or_update_notify_icon(hwnd: HWND, action: u32, hicon: HICON, tip: &str) -> bool {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_CALLBACK;
    nid.hIcon = hicon;

    let mut wide_tip = tip.encode_utf16().collect::<Vec<u16>>();
    if wide_tip.len() >= 127 {
        wide_tip.truncate(127);
    }
    nid.szTip[..wide_tip.len()].copy_from_slice(&wide_tip);
    nid.szTip[wide_tip.len()] = 0;

    if unsafe { Shell_NotifyIconW(action, &nid) } != 0 {
        return true;
    }
    nid.cbSize = NOTIFYICONDATAW_COMPAT_SIZE;
    unsafe { Shell_NotifyIconW(action, &nid) != 0 }
}

unsafe fn remove_notify_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_UID;
    if unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) } == 0 {
        nid.cbSize = NOTIFYICONDATAW_COMPAT_SIZE;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TRAY_CALLBACK {
        let event = (lparam as u32) & 0xFFFF;
        match event {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                launch_or_focus_gui();
                return 0;
            }
            WM_RBUTTONUP | WM_CONTEXTMENU => {
                show_tray_context_menu(hwnd);
                return 0;
            }
            _ => return 0,
        }
    }

    if msg == WM_TIMER && wparam == TIMER_RETRY_TRAY_ADD {
        TRAY_CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                if ctx.icon_added || ctx.retry_attempts >= 20 {
                    unsafe {
                        let _ = KillTimer(hwnd, TIMER_RETRY_TRAY_ADD);
                    }
                    return;
                }
                ctx.retry_attempts += 1;
                let tip = format_tooltip(&ctx.snapshot);
                let ok = unsafe { add_or_update_notify_icon(hwnd, NIM_ADD, ctx.current_icon, &tip) };
                if ok {
                    ctx.icon_added = true;
                    unsafe {
                        let _ = KillTimer(hwnd, TIMER_RETRY_TRAY_ADD);
                    }
                }
            }
        });
        return 0;
    }

    if msg == WM_TRAY_REFRESH {
        TRAY_CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                let status = extract_tray_icon_status(&ctx.snapshot);
                let new_icon = create_win32_tray_hicon(32, status);
                let tip = format_tooltip(&ctx.snapshot);
                let action = if ctx.icon_added { NIM_MODIFY } else { NIM_ADD };
                unsafe {
                    let ok = add_or_update_notify_icon(hwnd, action, new_icon, &tip);
                    if ok {
                        ctx.icon_added = true;
                    }
                    if !ctx.current_icon.is_null() {
                        let _ = DestroyIcon(ctx.current_icon);
                    }
                }
                ctx.current_icon = new_icon;
            }
        });
        return 0;
    }

    if msg == WM_TRAY_OPEN_GUI {
        launch_or_focus_gui();
        return 0;
    }

    if msg == WM_TRAY_QUIT {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }

    if msg == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }

    // Re-register tray icon automatically if Windows Explorer restarts
    let handled_taskbar_restart = TRAY_CTX.with(|cell| {
        if let Some(ctx) = cell.borrow().as_ref()
            && ctx.taskbar_created_msg != 0
            && msg == ctx.taskbar_created_msg
        {
            let tip = format_tooltip(&ctx.snapshot);
            unsafe {
                add_or_update_notify_icon(hwnd, NIM_ADD, ctx.current_icon, &tip);
            }
            return true;
        }
        false
    });
    if handled_taskbar_restart {
        return 0;
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn show_tray_context_menu(hwnd: HWND) {
    TRAY_CTX.with(|cell| {
        let borrow = cell.borrow();
        let Some(ctx) = borrow.as_ref() else {
            return;
        };

        let snap = ctx.snapshot.read().ok().map(|s| s.clone());
        let saved_profiles = list_saved_profiles();
        let autostart = is_autostart_enabled();

        unsafe {
            let menu: HMENU = CreatePopupMenu();
            if menu.is_null() {
                return;
            }

            // 1. Primary bold action: Open UI
            let open_lbl = wide_null("Open BK-R1X Configurator");
            let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_GUI as usize, open_lbl.as_ptr());
            let _ = SetMenuDefaultItem(menu, CMD_OPEN_GUI, 0);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

            // 2. Live telemetry header (non-clickable)
            let header_str = match &snap {
                Some(s) if s.connected => format!(
                    "BK-R1X ({}) · {}%{}",
                    s.connection_mode,
                    s.battery_percent,
                    if s.charging { " ⚡" } else { "" }
                ),
                _ => "BK-R1X · Disconnected".to_owned(),
            };
            let header_w = wide_null(&header_str);
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, header_w.as_ptr());

            // 3. Quick DPI submenu
            let dpi_menu = CreatePopupMenu();
            let active_dpi = snap.as_ref().map(|s| s.active_dpi).unwrap_or(0);
            for (i, &dpi) in QUICK_DPI_VALUES.iter().enumerate() {
                let flags = if active_dpi == dpi {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let lbl = wide_null(&format!("{dpi} DPI"));
                let _ = AppendMenuW(dpi_menu, flags, (CMD_DPI_BASE as usize) + i, lbl.as_ptr());
            }
            let dpi_title = wide_null(&format!("DPI Sensitivity ({active_dpi} DPI)"));
            let _ = AppendMenuW(menu, MF_POPUP, dpi_menu as usize, dpi_title.as_ptr());

            // 4. Quick Polling Rate submenu
            let rate_menu = CreatePopupMenu();
            let active_rate = snap.as_ref().map(|s| s.polling_rate_hz).unwrap_or(1000);
            for (i, &hz) in QUICK_RATE_VALUES.iter().enumerate() {
                let flags = if active_rate == hz {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let lbl = wide_null(&format!("{hz} Hz"));
                let _ = AppendMenuW(rate_menu, flags, (CMD_RATE_BASE as usize) + i, lbl.as_ptr());
            }
            let rate_title = wide_null(&format!("Polling Rate ({active_rate} Hz)"));
            let _ = AppendMenuW(menu, MF_POPUP, rate_menu as usize, rate_title.as_ptr());

            // 5. Profiles submenu
            if !saved_profiles.is_empty() {
                let prof_menu = CreatePopupMenu();
                let active_prof = snap
                    .as_ref()
                    .and_then(|s| s.active_profile.as_deref())
                    .unwrap_or("");
                for (i, (_, prof)) in saved_profiles.iter().enumerate().take(32) {
                    let flags = if prof.name == active_prof {
                        MF_STRING | MF_CHECKED
                    } else {
                        MF_STRING
                    };
                    let suffix = prof
                        .match_process
                        .as_deref()
                        .map(|p| format!(" [Auto: {p}]"))
                        .unwrap_or_default();
                    let lbl = wide_null(&format!("{}{suffix}", prof.name));
                    let _ = AppendMenuW(
                        prof_menu,
                        flags,
                        (CMD_PROFILE_BASE as usize) + i,
                        lbl.as_ptr(),
                    );
                }
                let prof_title = wide_null("Hardware Profiles");
                let _ = AppendMenuW(menu, MF_POPUP, prof_menu as usize, prof_title.as_ptr());
            }

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

            // 6. Startup toggle & Refresh
            let startup_flags = if autostart {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let startup_lbl = wide_null("Launch at Windows Startup");
            let _ = AppendMenuW(
                menu,
                startup_flags,
                CMD_TOGGLE_AUTOSTART as usize,
                startup_lbl.as_ptr(),
            );

            let refresh_lbl = wide_null("Refresh Telemetry Now");
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_REFRESH_NOW as usize,
                refresh_lbl.as_ptr(),
            );

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

            // 7. Quit
            let quit_lbl = wide_null("Quit BK-R1X");
            let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT_ALL as usize, quit_lbl.as_ptr());

            let mut pt = POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let selected = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            ) as u32;
            let _ = PostMessageW(hwnd, WM_NULL, 0, 0);
            let _ = DestroyMenu(menu);

            dispatch_menu_selection(selected, ctx, &saved_profiles, autostart);
        }
    });
}

fn dispatch_menu_selection(
    cmd_id: u32,
    ctx: &TrayContext,
    saved_profiles: &[(std::path::PathBuf, bkr1x_hal::HardwareProfile)],
    currently_autostart: bool,
) {
    match cmd_id {
        0 => {}
        CMD_OPEN_GUI => launch_or_focus_gui(),
        CMD_REFRESH_NOW => {
            let (tx, rx) = mpsc::sync_channel::<IpcResponse>(1);
            let _ = ctx.cmd_tx.send(DaemonCommand::Refresh { reply: tx });
            let _ = rx.recv_timeout(Duration::from_millis(800));
        }
        CMD_TOGGLE_AUTOSTART => {
            let _ = set_autostart_enabled(!currently_autostart, None);
        }
        CMD_QUIT_ALL => {
            close_open_gui_window();
            ctx.running.store(false, Ordering::SeqCst);
            let _ = ctx.cmd_tx.send(DaemonCommand::Shutdown);
            unsafe {
                PostQuitMessage(0);
            }
        }
        id if (CMD_DPI_BASE..CMD_DPI_BASE + QUICK_DPI_VALUES.len() as u32).contains(&id) => {
            let dpi = QUICK_DPI_VALUES[(id - CMD_DPI_BASE) as usize];
            let (tx, _rx) = mpsc::sync_channel::<IpcResponse>(1);
            let _ = ctx
                .cmd_tx
                .send(DaemonCommand::SetQuickDpi { dpi, reply: tx });
        }
        id if (CMD_RATE_BASE..CMD_RATE_BASE + QUICK_RATE_VALUES.len() as u32).contains(&id) => {
            let rate_hz = QUICK_RATE_VALUES[(id - CMD_RATE_BASE) as usize];
            let (tx, _rx) = mpsc::sync_channel::<IpcResponse>(1);
            let _ = ctx
                .cmd_tx
                .send(DaemonCommand::SetQuickRate { rate_hz, reply: tx });
        }
        id if (CMD_PROFILE_BASE..CMD_PROFILE_BASE + saved_profiles.len() as u32).contains(&id) => {
            let (_, prof) = &saved_profiles[(id - CMD_PROFILE_BASE) as usize];
            let (tx, _rx) = mpsc::sync_channel::<IpcResponse>(1);
            let _ = ctx.cmd_tx.send(DaemonCommand::ApplyProfile {
                profile: Box::new(prof.clone()),
                reply: tx,
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_hicon_and_struct_offsets_are_valid() {
        unsafe {
            let hicon = create_win32_tray_hicon(32, TrayIconStatus::default());
            assert!(!hicon.is_null());
            assert_eq!(std::mem::size_of::<NOTIFYICONDATAW>(), 976);
            assert_eq!(NOTIFYICONDATAW_COMPAT_SIZE, 952);
            let _ = DestroyIcon(hicon);
        }
    }
}
