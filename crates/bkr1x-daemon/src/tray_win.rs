#![cfg(windows)]

use core::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bkr1x_hal::icon::{TrayIconStatus, create_win32_tray_hicon};
use bkr1x_hal::{
    DaemonStatusSnapshot, GUI_WINDOW_TITLE, HardwareProfile, IpcResponse, is_autostart_enabled,
    list_saved_profiles, resolve_sibling_binary, set_autostart_enabled,
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

const WM_TRAY_CALLBACK: u32 = WM_USER + 10;
const WM_TRAY_REFRESH: u32 = WM_USER + 11;
const WM_TRAY_OPEN_GUI: u32 = WM_USER + 12;
const WM_TRAY_QUIT: u32 = WM_USER + 13;
const TRAY_ICON_UID: u32 = 1;
const TIMER_RETRY_TRAY_ADD: usize = 77;
const MAX_TRAY_ADD_RETRIES: u8 = 20;

const CMD_OPEN_GUI: u32 = 1001;
const CMD_REFRESH_NOW: u32 = 1002;
const CMD_TOGGLE_AUTOSTART: u32 = 1003;
const CMD_QUIT_ALL: u32 = 1004;
const CMD_DPI_BASE: u32 = 2000;
const CMD_RATE_BASE: u32 = 3000;
const CMD_PROFILE_BASE: u32 = 4000;

const QUICK_DPI_VALUES: [u16; 8] = [400, 800, 1200, 1600, 2400, 3200, 5000, 18000];
const QUICK_RATE_VALUES: [u16; 4] = [125, 250, 500, 1000];

/// Pre-Vista `NOTIFYICONDATAW_V2_SIZE` fallback in case Common-Controls v6 fusion activation is unavailable.
const NOTIFYICONDATAW_COMPAT_SIZE: u32 = std::mem::offset_of!(NOTIFYICONDATAW, guidItem) as u32;

#[derive(Clone, Default)]
pub struct TrayNotifier {
    hwnd: Arc<AtomicPtr<c_void>>,
}

impl TrayNotifier {
    fn post(&self, msg: u32) -> bool {
        let ptr = self.hwnd.load(Ordering::Acquire);
        if ptr.is_null() {
            return false;
        }
        // SAFETY: `ptr` holds the live message-only `HWND` owned by `run_system_tray_loop` until cleared.
        unsafe { PostMessageW(ptr, msg, 0, 0) != 0 }
    }

    pub fn notify_state_changed(&self) {
        let _ = self.post(WM_TRAY_REFRESH);
    }

    pub fn request_open_gui(&self) {
        if !self.post(WM_TRAY_OPEN_GUI) {
            launch_or_focus_gui();
        }
    }

    pub fn request_quit(&self) {
        let _ = self.post(WM_TRAY_QUIT);
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

fn wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn launch_or_focus_gui() {
    let wide_title = wide_null(GUI_WINDOW_TITLE);
    // SAFETY: `wide_title` is null-terminated; window restoration is a no-op if the handle closed mid-call.
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

fn close_open_gui_window() {
    let wide_title = wide_null(GUI_WINDOW_TITLE);
    // SAFETY: Posts `WM_CLOSE` asynchronously if a matching top-level GUI window exists.
    unsafe {
        let existing = FindWindowW(std::ptr::null(), wide_title.as_ptr());
        if !existing.is_null() {
            let _ = PostMessageW(existing, WM_CLOSE, 0, 0);
        }
    }
}

pub fn run_system_tray_loop(
    notifier: TrayNotifier,
    snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
) {
    let class_name = wide_null("BKR1X_SystemTrayHostClass");
    let win_name = wide_null("BKR1X_TrayHost");
    let taskbar_msg_name = wide_null("TaskbarCreated");

    // SAFETY: Standard Win32 window registration, message pump, and teardown on the dedicated GUI/tray thread.
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

        let init_icon = create_win32_tray_hicon(32, extract_tray_icon_status(&snapshot));
        let added = add_or_update_notify_icon(hwnd, NIM_ADD, init_icon, &format_tooltip(&snapshot));

        TRAY_CTX.with(|cell| {
            *cell.borrow_mut() = Some(TrayContext {
                snapshot: Arc::clone(&snapshot),
                cmd_tx,
                running,
                taskbar_created_msg,
                current_icon: init_icon,
                icon_added: added,
                retry_attempts: 0,
            });
        });

        // During early Windows login `Shell_TrayWnd` may not exist yet; retry until registered,
        // then kill the timer permanently so the message thread sleeps at 0.0% CPU.
        if !added {
            let _ = SetTimer(hwnd, TIMER_RETRY_TRAY_ADD, 1500, None);
        }
        notifier.hwnd.store(hwnd, Ordering::Release);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }

        notifier.hwnd.store(std::ptr::null_mut(), Ordering::Release);
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
    let Ok(snap) = snapshot.read() else {
        return TrayIconStatus::default();
    };
    TrayIconStatus {
        connected: snap.connected,
        online: snap.online,
        battery_percent: snap.battery_percent,
        charging: snap.charging,
    }
}

fn format_tooltip(snapshot: &Arc<RwLock<DaemonStatusSnapshot>>) -> String {
    let Ok(snap) = snapshot.read() else {
        return "Fire Phoenix BK-R1X".to_owned();
    };
    if !snap.connected {
        return "Fire Phoenix BK-R1X — Disconnected\nClick to open configurator".to_owned();
    }
    let state_tag = if snap.charging {
        "Charging"
    } else if !snap.online {
        "Sleep"
    } else {
        &snap.connection_mode
    };
    format!(
        "Fire Phoenix BK-R1X ({state_tag})\nBattery: {}% · {} DPI · {} Hz",
        snap.battery_percent, snap.active_dpi, snap.polling_rate_hz
    )
}

unsafe fn add_or_update_notify_icon(hwnd: HWND, action: u32, hicon: HICON, tip: &str) -> bool {
    if hicon.is_null() {
        return false;
    }
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_UID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_CALLBACK;
    nid.hIcon = hicon;

    for (idx, code_unit) in tip.encode_utf16().take(127).enumerate() {
        nid.szTip[idx] = code_unit;
    }

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
    match msg {
        WM_TRAY_CALLBACK => {
            match (lparam as u32) & 0xFFFF {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => launch_or_focus_gui(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_context_menu(hwnd),
                _ => {}
            }
            return 0;
        }
        WM_TIMER if wparam == TIMER_RETRY_TRAY_ADD => {
            handle_tray_retry_timer(hwnd);
            return 0;
        }
        WM_TRAY_REFRESH => {
            handle_tray_refresh(hwnd);
            return 0;
        }
        WM_TRAY_OPEN_GUI => {
            launch_or_focus_gui();
            return 0;
        }
        WM_TRAY_QUIT | WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        _ => {}
    }

    let handled_explorer_restart = TRAY_CTX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(ctx) = borrow.as_mut() else {
            return false;
        };
        if ctx.taskbar_created_msg == 0 || msg != ctx.taskbar_created_msg {
            return false;
        }
        let tip = format_tooltip(&ctx.snapshot);
        ctx.icon_added =
            unsafe { add_or_update_notify_icon(hwnd, NIM_ADD, ctx.current_icon, &tip) };
        true
    });
    if handled_explorer_restart {
        return 0;
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn handle_tray_retry_timer(hwnd: HWND) {
    TRAY_CTX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(ctx) = borrow.as_mut() else {
            return;
        };
        if ctx.icon_added || ctx.retry_attempts >= MAX_TRAY_ADD_RETRIES {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_RETRY_TRAY_ADD);
            }
            return;
        }
        ctx.retry_attempts += 1;
        let tip = format_tooltip(&ctx.snapshot);
        if unsafe { add_or_update_notify_icon(hwnd, NIM_ADD, ctx.current_icon, &tip) } {
            ctx.icon_added = true;
            unsafe {
                let _ = KillTimer(hwnd, TIMER_RETRY_TRAY_ADD);
            }
        }
    });
}

fn handle_tray_refresh(hwnd: HWND) {
    TRAY_CTX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(ctx) = borrow.as_mut() else {
            return;
        };
        let new_icon = create_win32_tray_hicon(32, extract_tray_icon_status(&ctx.snapshot));
        if new_icon.is_null() {
            return;
        }
        let tip = format_tooltip(&ctx.snapshot);
        let action = if ctx.icon_added { NIM_MODIFY } else { NIM_ADD };
        // SAFETY: `hwnd` and `new_icon` are valid handles on this thread; old icon is freed after swap.
        unsafe {
            if add_or_update_notify_icon(hwnd, action, new_icon, &tip) {
                ctx.icon_added = true;
            }
            if !ctx.current_icon.is_null() {
                let _ = DestroyIcon(ctx.current_icon);
            }
        }
        ctx.current_icon = new_icon;
    });
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

        // SAFETY: Constructs, displays, and destroys a modal Win32 `HMENU` synchronously on the UI thread.
        unsafe {
            let menu: HMENU = CreatePopupMenu();
            if menu.is_null() {
                return;
            }

            append_menu_str(
                menu,
                MF_STRING,
                CMD_OPEN_GUI as usize,
                "Open BK-R1X Configurator",
            );
            let _ = SetMenuDefaultItem(menu, CMD_OPEN_GUI, 0);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

            let header = match &snap {
                Some(s) if s.connected => format!(
                    "BK-R1X ({}) · {}%{}",
                    s.connection_mode,
                    s.battery_percent,
                    if s.charging { " ⚡" } else { "" }
                ),
                _ => "BK-R1X · Disconnected".to_owned(),
            };
            append_menu_str(menu, MF_STRING | MF_GRAYED, 0, &header);

            append_dpi_submenu(menu, snap.as_ref().map_or(0, |s| s.active_dpi));
            append_rate_submenu(menu, snap.as_ref().map_or(1000, |s| s.polling_rate_hz));
            append_profiles_submenu(
                menu,
                &saved_profiles,
                snap.as_ref().and_then(|s| s.active_profile.as_deref()),
            );

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            let startup_flags = if autostart {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            append_menu_str(
                menu,
                startup_flags,
                CMD_TOGGLE_AUTOSTART as usize,
                "Launch at Windows Startup",
            );
            append_menu_str(
                menu,
                MF_STRING,
                CMD_REFRESH_NOW as usize,
                "Refresh Telemetry Now",
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            append_menu_str(menu, MF_STRING, CMD_QUIT_ALL as usize, "Quit BK-R1X");

            let mut cursor = POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&mut cursor);
            let _ = SetForegroundWindow(hwnd);
            let selected = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                cursor.x,
                cursor.y,
                0,
                hwnd,
                std::ptr::null(),
            ) as u32;
            // Win32 tray menu quirk: post `WM_NULL` so clicking outside dismisses the popup cleanly.
            let _ = PostMessageW(hwnd, WM_NULL, 0, 0);
            let _ = DestroyMenu(menu);

            dispatch_menu_selection(selected, ctx, &saved_profiles, autostart);
        }
    });
}

unsafe fn append_menu_str(menu: HMENU, flags: u32, id_or_sub: usize, label: &str) {
    let wide = wide_null(label);
    unsafe {
        let _ = AppendMenuW(menu, flags, id_or_sub, wide.as_ptr());
    }
}

unsafe fn append_dpi_submenu(parent: HMENU, active_dpi: u16) {
    let sub = unsafe { CreatePopupMenu() };
    if sub.is_null() {
        return;
    }
    for (idx, &dpi) in QUICK_DPI_VALUES.iter().enumerate() {
        let flags = if active_dpi == dpi {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        unsafe {
            append_menu_str(
                sub,
                flags,
                (CMD_DPI_BASE as usize) + idx,
                &format!("{dpi} DPI"),
            )
        };
    }
    unsafe {
        append_menu_str(
            parent,
            MF_POPUP,
            sub as usize,
            &format!("DPI Sensitivity ({active_dpi} DPI)"),
        );
    }
}

unsafe fn append_rate_submenu(parent: HMENU, active_rate: u16) {
    let sub = unsafe { CreatePopupMenu() };
    if sub.is_null() {
        return;
    }
    for (idx, &hz) in QUICK_RATE_VALUES.iter().enumerate() {
        let flags = if active_rate == hz {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        unsafe {
            append_menu_str(
                sub,
                flags,
                (CMD_RATE_BASE as usize) + idx,
                &format!("{hz} Hz"),
            )
        };
    }
    unsafe {
        append_menu_str(
            parent,
            MF_POPUP,
            sub as usize,
            &format!("Polling Rate ({active_rate} Hz)"),
        );
    }
}

unsafe fn append_profiles_submenu(
    parent: HMENU,
    saved_profiles: &[(PathBuf, HardwareProfile)],
    active_profile: Option<&str>,
) {
    if saved_profiles.is_empty() {
        return;
    }
    let sub = unsafe { CreatePopupMenu() };
    if sub.is_null() {
        return;
    }
    for (idx, (_, prof)) in saved_profiles.iter().enumerate().take(32) {
        let flags = if active_profile == Some(prof.name.as_str()) {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let label = match prof.match_process.as_deref() {
            Some(proc_name) => format!("{} [Auto: {proc_name}]", prof.name),
            None => prof.name.clone(),
        };
        unsafe { append_menu_str(sub, flags, (CMD_PROFILE_BASE as usize) + idx, &label) };
    }
    unsafe { append_menu_str(parent, MF_POPUP, sub as usize, "Hardware Profiles") };
}

fn dispatch_menu_selection(
    cmd_id: u32,
    ctx: &TrayContext,
    saved_profiles: &[(PathBuf, HardwareProfile)],
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
            unsafe { PostQuitMessage(0) };
        }
        id if (CMD_DPI_BASE..CMD_DPI_BASE + QUICK_DPI_VALUES.len() as u32).contains(&id) => {
            let dpi = QUICK_DPI_VALUES[(id - CMD_DPI_BASE) as usize];
            let (tx, _) = mpsc::sync_channel::<IpcResponse>(1);
            let _ = ctx
                .cmd_tx
                .send(DaemonCommand::SetQuickDpi { dpi, reply: tx });
        }
        id if (CMD_RATE_BASE..CMD_RATE_BASE + QUICK_RATE_VALUES.len() as u32).contains(&id) => {
            let rate_hz = QUICK_RATE_VALUES[(id - CMD_RATE_BASE) as usize];
            let (tx, _) = mpsc::sync_channel::<IpcResponse>(1);
            let _ = ctx
                .cmd_tx
                .send(DaemonCommand::SetQuickRate { rate_hz, reply: tx });
        }
        id if (CMD_PROFILE_BASE..CMD_PROFILE_BASE + saved_profiles.len() as u32).contains(&id) => {
            let (_, prof) = &saved_profiles[(id - CMD_PROFILE_BASE) as usize];
            let (tx, _) = mpsc::sync_channel::<IpcResponse>(1);
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
