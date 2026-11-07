#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ipc_server;
#[cfg(windows)]
mod tray_win;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use bkr1x_hal::profile::{MacroProfileEntry, decode_macro_entries, format_light_mode};
#[cfg(windows)]
use bkr1x_hal::send_ipc_request;
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, DeviceError, HardwareProfile, IpcRequest, IpcResponse,
    IpcWireTrace, TraceDirection, encode_hex_spaced, ensure_user_profiles_dir,
    is_profile_extension,
};
use bkr1x_protocol::{
    ButtonAction, ButtonMapping, HardwareStateMachine, MacroStep, PollingRate, ScrollDirection,
    validate_dpi,
};

pub(crate) enum DaemonCommand {
    GetFullState {
        reply: SyncSender<IpcResponse>,
    },
    Refresh {
        reply: SyncSender<IpcResponse>,
    },
    SetQuickDpi {
        dpi: u16,
        reply: SyncSender<IpcResponse>,
    },
    SetQuickRate {
        rate_hz: u16,
        reply: SyncSender<IpcResponse>,
    },
    FlashButtons {
        raw_slots: [[u8; 4]; 8],
        reply: SyncSender<IpcResponse>,
    },
    FlashMacros {
        macros: Vec<MacroProfileEntry>,
        reply: SyncSender<IpcResponse>,
    },
    ApplyProfile {
        profile: Box<HardwareProfile>,
        reply: SyncSender<IpcResponse>,
    },
    ReloadProfiles,
    Shutdown,
}

#[cfg_attr(not(windows), allow(dead_code))]
struct DaemonConfig {
    poll_interval: Duration,
    profiles_dir: PathBuf,
    default_profile: Option<HardwareProfile>,
    headless: bool,
    silent_if_running: bool,
}

fn parse_cli_args() -> DaemonConfig {
    let mut poll_secs = 60u64;
    let mut profiles_dir = ensure_user_profiles_dir();
    let mut default_profile_path: Option<PathBuf> = None;
    let mut headless = false;
    let mut silent_if_running = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--poll-secs" => {
                if let Some(secs) = args.next().and_then(|s| s.parse::<u64>().ok()) {
                    poll_secs = secs.max(5);
                }
            }
            "--profiles-dir" => {
                if let Some(dir) = args.next() {
                    profiles_dir = PathBuf::from(dir);
                }
            }
            "--default-profile" => {
                if let Some(path) = args.next() {
                    default_profile_path = Some(PathBuf::from(path));
                }
            }
            "--headless" => headless = true,
            "--silent" | "--silent-if-running" => silent_if_running = true,
            _ => {}
        }
    }

    let default_profile = default_profile_path
        .as_deref()
        .and_then(|path| HardwareProfile::load_from_file(path).ok());

    DaemonConfig {
        poll_interval: Duration::from_secs(poll_secs),
        profiles_dir,
        default_profile,
        headless,
        silent_if_running,
    }
}

fn load_app_profiles(dir: &Path) -> Vec<HardwareProfile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_profile_extension(path))
        .filter_map(|path| HardwareProfile::load_from_file(&path).ok())
        .filter(|prof| {
            prof.match_process
                .as_deref()
                .is_some_and(|pat| !pat.trim().is_empty())
        })
        .collect()
}

fn snapshot_from_device(
    dev_opt: Option<&Bkr1xDevice>,
    fallback_state: &HardwareStateMachine,
    active_profile: Option<&str>,
) -> DaemonStatusSnapshot {
    let (connected, vid, pid, prod_name, state) = match dev_opt {
        Some(dev) => (
            true,
            dev.info().vendor_id,
            dev.info().product_id,
            dev.info().product_name.clone(),
            dev.state(),
        ),
        None => (false, 0, 0, String::new(), fallback_state),
    };
    let cfg = &state.config;
    let raw_buttons: [[u8; 4]; 8] = core::array::from_fn(|idx| state.buttons.slots[idx].to_bytes());

    DaemonStatusSnapshot {
        connected,
        online: connected && state.online,
        connection_mode: state
            .connection
            .map(|mode| mode.as_str().to_owned())
            .unwrap_or_else(|| "Disconnected".to_owned()),
        vendor_id: vid,
        product_id: pid,
        product_name: prod_name,
        firmware_version: state.firmware.to_string(),
        battery_percent: state.battery.percent,
        charging: state.battery.charging,
        active_dpi: cfg.active_dpi(),
        active_dpi_stage: cfg.active_dpi_stage.saturating_add(1),
        dpi_stages: cfg
            .dpi_stages
            .get(..usize::from(cfg.dpi_count))
            .unwrap_or(&cfg.dpi_stages)
            .to_vec(),
        polling_rate_hz: cfg.polling_rate.as_hz(),
        lod_mm: cfg.lod.as_mm(),
        debounce_ms: cfg.debounce_ms,
        motion_sync: cfg.sensor_flags.motion_sync,
        angle_snapping: cfg.sensor_flags.angle_snapping,
        ripple_control: cfg.sensor_flags.ripple_control,
        reverse_scroll: cfg.scroll_direction == ScrollDirection::Reverse,
        highspeed_mode: cfg.highspeed_mode,
        wakeup_on_move: cfg.wakeup_on_move,
        move_lights_off: cfg.move_lights_off,
        sleep_minutes: cfg.sleep_minutes,
        light_mode: format_light_mode(cfg.light_mode).to_owned(),
        active_profile: active_profile.map(str::to_owned),
        button_slots: Some(raw_buttons),
    }
}

fn collect_ipc_traces(dev: &mut Bkr1xDevice) -> Vec<IpcWireTrace> {
    let checksum = dev.checksum_strategy();
    let traces = dev
        .trace_log()
        .iter()
        .map(|entry| IpcWireTrace {
            timestamp_unix_ms: entry.timestamp_unix_ms,
            is_out: entry.direction == TraceDirection::Out,
            report_id: entry.report_id,
            payload_hex: encode_hex_spaced(&entry.payload),
            summary: entry.format_summary(checksum),
        })
        .collect();
    dev.clear_trace_log();
    traces
}

fn apply_profile_to_device(
    dev: &mut Bkr1xDevice,
    prof: &HardwareProfile,
) -> Result<(), DeviceError> {
    let cfg = prof.to_mouse_config()?;
    dev.write_config(&cfg)?;
    dev.write_light_mode(cfg.light_mode)?;
    if prof.buttons.is_some() {
        let buttons = prof.to_button_mapping()?;
        dev.write_buttons(&buttons)?;
    }
    if !prof.macros.is_empty() {
        let vecs = prof.to_macro_vecs()?;
        let slices: Vec<&[MacroStep]> = vecs.iter().map(Vec::as_slice).collect();
        dev.write_macros(&slices)?;
    }
    Ok(())
}

#[cfg(windows)]
struct SingleInstanceGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl SingleInstanceGuard {
    fn acquire() -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;

        let wide_name: Vec<u16> = "Local\\BKR1X_Daemon_SingleInstance"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: Creates or opens a per-session named Win32 mutex; duplicate handles are closed immediately.
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, wide_name.as_ptr());
            if handle.is_null() {
                return None;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid Win32 mutex handle owned exclusively by this guard.
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn main() {
    let cfg = parse_cli_args();

    #[cfg(windows)]
    let Some(_instance_guard) = SingleInstanceGuard::acquire() else {
        if !cfg.silent_if_running {
            let _ = send_ipc_request(&IpcRequest::OpenGui);
        }
        return;
    };
    #[cfg(not(windows))]
    let _ = cfg.silent_if_running;

    let running = Arc::new(AtomicBool::new(true));
    let fallback_state = HardwareStateMachine::new();
    let shared_snapshot = Arc::new(RwLock::new(snapshot_from_device(
        None,
        &fallback_state,
        None,
    )));

    let (cmd_tx, cmd_rx): (Sender<DaemonCommand>, Receiver<DaemonCommand>) = mpsc::channel();

    #[cfg(windows)]
    let tray_notifier = tray_win::TrayNotifier::default();

    let ipc_snapshot = Arc::clone(&shared_snapshot);
    let ipc_running = Arc::clone(&running);
    let ipc_tx = cmd_tx.clone();
    #[cfg(windows)]
    let ipc_tray = tray_notifier.clone();
    let Ok(_ipc_handle) = thread::Builder::new()
        .name("bkr1x-ipc".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            ipc_server::run_ipc_listener(
                ipc_snapshot,
                ipc_tx,
                ipc_running,
                #[cfg(windows)]
                ipc_tray,
            )
        })
    else {
        return;
    };

    #[cfg(windows)]
    if !cfg.headless {
        let hw_snapshot = Arc::clone(&shared_snapshot);
        let hw_running = Arc::clone(&running);
        let hw_tray = tray_notifier.clone();
        let Ok(_hw_handle) = thread::Builder::new()
            .name("bkr1x-hw".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                hardware_event_loop(cfg, hw_snapshot, cmd_rx, hw_running, Some(hw_tray));
            })
        else {
            return;
        };

        tray_win::run_system_tray_loop(tray_notifier, shared_snapshot, cmd_tx, running);
        return;
    }

    hardware_event_loop(
        cfg,
        shared_snapshot,
        cmd_rx,
        running,
        #[cfg(windows)]
        None,
    );
}

struct LoopContext {
    device: Option<Bkr1xDevice>,
    fallback_state: HardwareStateMachine,
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    active_profile_name: Option<String>,
    #[cfg(windows)]
    tray: Option<tray_win::TrayNotifier>,
}

impl LoopContext {
    fn notify_tray(&self) {
        #[cfg(windows)]
        if let Some(ref tray) = self.tray {
            tray.notify_state_changed();
        }
    }

    fn publish_snapshot(&self) {
        if let Ok(mut guard) = self.shared_snapshot.write() {
            *guard = snapshot_from_device(
                self.device.as_ref(),
                &self.fallback_state,
                self.active_profile_name.as_deref(),
            );
        }
    }

    fn cloned_boxed_snapshot(&self) -> Option<Box<DaemonStatusSnapshot>> {
        self.shared_snapshot
            .read()
            .ok()
            .map(|guard| Box::new(guard.clone()))
    }

    fn ensure_connected(&mut self, sync_on_open: bool) -> Result<&mut Bkr1xDevice, DeviceError> {
        if self.device.is_none() {
            let mut dev = Bkr1xDevice::open_auto()?;
            if sync_on_open {
                let _ = dev.sync_all();
            }
            self.device = Some(dev);
        }
        self.device.as_mut().ok_or(DeviceError::NotFound)
    }

    fn run_device_mutation<F>(
        &mut self,
        sync_on_open: bool,
        refresh_tray: bool,
        mutate: F,
    ) -> IpcResponse
    where
        F: FnOnce(&mut Bkr1xDevice, &mut Option<String>) -> Result<String, DeviceError>,
    {
        let mut new_profile = self.active_profile_name.clone();
        let outcome = self
            .ensure_connected(sync_on_open)
            .and_then(|dev| mutate(dev, &mut new_profile));

        match outcome {
            Ok(message) => {
                self.active_profile_name = new_profile;
                self.publish_snapshot();
                if refresh_tray {
                    self.notify_tray();
                }
                let traces = self.device.as_mut().map(collect_ipc_traces);
                IpcResponse::Ok {
                    snapshot: self.cloned_boxed_snapshot(),
                    traces,
                    message: Some(message),
                }
            }
            Err(err) => {
                if matches!(err, DeviceError::Hid(_)) {
                    self.device = None;
                    self.publish_snapshot();
                    self.notify_tray();
                }
                IpcResponse::Error {
                    message: err.to_string(),
                }
            }
        }
    }

    fn poll_hardware_once(&mut self) {
        if self.device.is_none() {
            match Bkr1xDevice::open_auto() {
                Ok(mut dev) => {
                    let _ = dev.sync_all();
                    self.device = Some(dev);
                    self.publish_snapshot();
                }
                Err(_) => self.publish_snapshot(),
            }
            return;
        }

        let mut disconnected = false;
        if let Some(dev) = self.device.as_mut() {
            match dev.poll_battery(Duration::from_secs(5)) {
                Ok(_) => self.publish_snapshot(),
                Err(DeviceError::Hid(_)) => disconnected = true,
                Err(_) => {}
            }
        }
        if disconnected {
            self.device = None;
            self.publish_snapshot();
        }
    }
}

fn hardware_event_loop(
    cfg: DaemonConfig,
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_rx: Receiver<DaemonCommand>,
    running: Arc<AtomicBool>,
    #[cfg(windows)] tray: Option<tray_win::TrayNotifier>,
) {
    let mut app_profiles = load_app_profiles(&cfg.profiles_dir);
    let mut saved_desktop_baseline: Option<HardwareProfile> = None;
    let mut in_auto_game_profile = false;
    let mut last_foreground_exe = String::new();

    let mut ctx = LoopContext {
        device: None,
        fallback_state: HardwareStateMachine::new(),
        shared_snapshot,
        active_profile_name: None,
        #[cfg(windows)]
        tray,
    };

    ctx.poll_hardware_once();
    ctx.notify_tray();

    let mut elapsed_since_battery = Duration::ZERO;

    while running.load(Ordering::Relaxed) {
        let wait_quantum = if app_profiles.is_empty() {
            cfg.poll_interval
        } else {
            Duration::from_secs(2).min(cfg.poll_interval)
        };

        match cmd_rx.recv_timeout(wait_quantum) {
            Ok(DaemonCommand::Shutdown) => {
                running.store(false, Ordering::SeqCst);
                #[cfg(windows)]
                if let Some(ref tray) = ctx.tray {
                    tray.request_quit();
                }
                break;
            }
            Ok(DaemonCommand::ReloadProfiles) => {
                app_profiles = load_app_profiles(&cfg.profiles_dir);
            }
            Ok(DaemonCommand::GetFullState { reply }) => {
                if ctx.device.is_none() {
                    ctx.poll_hardware_once();
                    ctx.notify_tray();
                }
                let traces = ctx.device.as_mut().map(collect_ipc_traces);
                let _ = reply.send(IpcResponse::Ok {
                    snapshot: ctx.cloned_boxed_snapshot(),
                    traces,
                    message: None,
                });
            }
            Ok(DaemonCommand::Refresh { reply }) => {
                if let Some(dev) = ctx.device.as_mut() {
                    let _ = dev.sync_all();
                    ctx.publish_snapshot();
                } else {
                    ctx.poll_hardware_once();
                }
                ctx.notify_tray();
                let traces = ctx.device.as_mut().map(collect_ipc_traces);
                let _ = reply.send(IpcResponse::Ok {
                    snapshot: ctx.cloned_boxed_snapshot(),
                    traces,
                    message: Some("Hardware telemetry refreshed".to_owned()),
                });
                elapsed_since_battery = Duration::ZERO;
            }
            Ok(DaemonCommand::SetQuickDpi { dpi, reply }) => {
                let resp = ctx.run_device_mutation(true, true, |dev, _| {
                    let mut mcfg = dev.state().config;
                    let active_idx = usize::from(mcfg.active_dpi_stage);
                    validate_dpi(active_idx, dpi)?;
                    let enabled_stages = mcfg
                        .dpi_stages
                        .get(..usize::from(mcfg.dpi_count))
                        .unwrap_or(&mcfg.dpi_stages);
                    if let Some(idx) = enabled_stages
                        .iter()
                        .position(|&stage_dpi| stage_dpi == dpi)
                    {
                        mcfg.active_dpi_stage = idx as u8;
                    } else if let Some(slot) = mcfg.dpi_stages.get_mut(active_idx) {
                        *slot = dpi;
                    }
                    dev.write_config(&mcfg)?;
                    Ok(format!("Switched to {dpi} DPI"))
                });
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::SetQuickRate { rate_hz, reply }) => {
                let resp = ctx.run_device_mutation(true, true, |dev, _| {
                    let rate = PollingRate::from_hz(rate_hz)?;
                    let mut mcfg = dev.state().config;
                    mcfg.polling_rate = rate;
                    dev.write_config(&mcfg)?;
                    Ok(format!("Switched polling rate to {rate_hz} Hz"))
                });
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::FlashButtons { raw_slots, reply }) => {
                let resp = ctx.run_device_mutation(false, false, |dev, _| {
                    let mapping = ButtonMapping {
                        slots: core::array::from_fn(|idx| ButtonAction::from_bytes(raw_slots[idx])),
                    };
                    dev.write_buttons(&mapping)?;
                    Ok("Flashed 8-slot button map to BK-R1X.".to_owned())
                });
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::FlashMacros { macros, reply }) => {
                let resp = ctx.run_device_mutation(false, false, |dev, _| {
                    let vecs = decode_macro_entries(&macros)?;
                    let slices: Vec<&[MacroStep]> = vecs.iter().map(Vec::as_slice).collect();
                    let bytes = dev.write_macros(&slices)?;
                    Ok(format!("Flashed {bytes} bytes of hardware macros to MCU."))
                });
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::ApplyProfile { profile, reply }) => {
                let resp = ctx.run_device_mutation(false, true, |dev, active_name| {
                    apply_profile_to_device(dev, &profile)?;
                    *active_name = Some(profile.name.clone());
                    Ok(format!("Applied profile '{}'", profile.name))
                });
                let _ = reply.send(resp);
            }
            Err(RecvTimeoutError::Timeout) => {
                elapsed_since_battery += wait_quantum;

                handle_foreground_profile_switch(
                    &mut ctx,
                    &cfg,
                    &app_profiles,
                    &mut last_foreground_exe,
                    &mut saved_desktop_baseline,
                    &mut in_auto_game_profile,
                );

                if elapsed_since_battery >= cfg.poll_interval {
                    ctx.poll_hardware_once();
                    ctx.notify_tray();
                    elapsed_since_battery = Duration::ZERO;
                } else if let Some(dev) = ctx.device.as_mut()
                    && dev.drain_async_events() > 0
                {
                    ctx.publish_snapshot();
                    ctx.notify_tray();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    #[cfg(unix)]
    {
        let _ = fs::remove_file(bkr1x_hal::unix_socket_path());
    }
}

fn handle_foreground_profile_switch(
    ctx: &mut LoopContext,
    cfg: &DaemonConfig,
    app_profiles: &[HardwareProfile],
    last_foreground_exe: &mut String,
    saved_desktop_baseline: &mut Option<HardwareProfile>,
    in_auto_game_profile: &mut bool,
) {
    if app_profiles.is_empty() {
        return;
    }
    let Some(fg_exe) = query_foreground_process_name() else {
        return;
    };
    if fg_exe.eq_ignore_ascii_case(last_foreground_exe) {
        return;
    }
    *last_foreground_exe = fg_exe.clone();

    let matched = app_profiles.iter().find(|prof| {
        prof.match_process
            .as_deref()
            .is_some_and(|pat| contains_ignore_ascii_case(&fg_exe, pat.trim()))
    });

    if let Some(game_prof) = matched {
        if ctx.active_profile_name.as_deref() == Some(game_prof.name.as_str()) {
            return;
        }
        let Some(dev) = ctx.device.as_mut() else {
            return;
        };
        if !*in_auto_game_profile {
            *saved_desktop_baseline = Some(HardwareProfile::from_hardware(
                "Desktop Baseline",
                &dev.state().config,
                &dev.state().buttons,
            ));
        }
        if apply_profile_to_device(dev, game_prof).is_ok() {
            *in_auto_game_profile = true;
            ctx.active_profile_name = Some(game_prof.name.clone());
            ctx.publish_snapshot();
            ctx.notify_tray();
        }
        return;
    }

    if !*in_auto_game_profile {
        return;
    }
    let Some(base_prof) = cfg
        .default_profile
        .as_ref()
        .or(saved_desktop_baseline.as_ref())
    else {
        return;
    };
    let Some(dev) = ctx.device.as_mut() else {
        return;
    };
    if apply_profile_to_device(dev, base_prof).is_ok() {
        *in_auto_game_profile = false;
        ctx.active_profile_name = cfg.default_profile.as_ref().map(|p| p.name.clone());
        ctx.publish_snapshot();
        ctx.notify_tray();
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    !n.is_empty() && h.windows(n.len()).any(|win| win.eq_ignore_ascii_case(n))
}

#[cfg(windows)]
fn query_foreground_process_name() -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    // SAFETY: Queries read-only image path of the foreground window's PID and closes `proc_handle`.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let proc_handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if proc_handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc_handle, 0, buf.as_mut_ptr(), &mut len);
        let _ = CloseHandle(proc_handle);
        if ok == 0 || len == 0 {
            return None;
        }
        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&full_path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
    }
}

#[cfg(not(windows))]
fn query_foreground_process_name() -> Option<String> {
    None
}
