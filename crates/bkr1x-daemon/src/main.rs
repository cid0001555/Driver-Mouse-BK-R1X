#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
//! `bkr1x-daemon` — Silent System Tray agent, sub-millisecond IPC server, and per-app profile
//! switcher for the Fire Phoenix BK-R1X (`< 2 MB` background RSS, `0.0%` idle CPU).

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

use bkr1x_hal::profile::{MacroProfileEntry, format_light_mode};
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, HardwareProfile, IpcRequest, IpcResponse, IpcWireTrace,
    TraceDirection, ensure_user_profiles_dir, send_ipc_request,
};
use bkr1x_protocol::{
    ButtonAction, ButtonMapping, HardwareStateMachine, MacroInput, MacroStep, MouseClickButton,
    MouseConfig, PollingRate, ScrollDirection, validate_dpi,
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
                if let Some(v) = args.next().and_then(|s| s.parse::<u64>().ok()) {
                    poll_secs = v.max(5);
                }
            }
            "--profiles-dir" => {
                if let Some(p) = args.next() {
                    profiles_dir = PathBuf::from(p);
                }
            }
            "--default-profile" => {
                if let Some(p) = args.next() {
                    default_profile_path = Some(PathBuf::from(p));
                }
            }
            "--headless" => headless = true,
            "--silent" | "--silent-if-running" => silent_if_running = true,
            _ => {}
        }
    }

    let default_profile = default_profile_path
        .as_deref()
        .and_then(|p| HardwareProfile::load_from_file(p).ok());

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
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let is_cfg = p
            .extension()
            .and_then(|s| s.to_str())
            .and_then(e_eq_cfg)
            .unwrap_or(false);
        if is_cfg
            && let Ok(prof) = HardwareProfile::load_from_file(&p)
            && prof.match_process.as_deref().is_some_and(|s| !s.trim().is_empty())
        {
            out.push(prof);
        }
    }
    out
}

fn e_eq_cfg(ext: &str) -> Option<bool> {
    Some(ext.eq_ignore_ascii_case("toml") || ext.eq_ignore_ascii_case("json"))
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
    let raw_buttons: [[u8; 4]; 8] = core::array::from_fn(|i| state.buttons.slots[i].to_bytes());

    DaemonStatusSnapshot {
        connected,
        online: connected && state.online,
        connection_mode: state
            .connection
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| "Disconnected".to_owned()),
        vendor_id: vid,
        product_id: pid,
        product_name: prod_name,
        firmware_version: state.firmware.to_string(),
        battery_percent: state.battery.percent,
        charging: state.battery.charging,
        active_dpi: cfg.active_dpi(),
        active_dpi_stage: cfg.active_dpi_stage + 1,
        dpi_stages: cfg.dpi_stages[..cfg.dpi_count as usize].to_vec(),
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
    let out = dev
        .trace_log()
        .iter()
        .map(|t| IpcWireTrace {
            timestamp_unix_ms: t.timestamp_unix_ms,
            is_out: t.direction == TraceDirection::Out,
            report_id: t.report_id,
            payload_hex: t
                .payload
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" "),
            summary: t.summary.clone(),
        })
        .collect();
    dev.clear_trace_log();
    out
}

fn apply_profile_to_device(
    dev: &mut Bkr1xDevice,
    prof: &HardwareProfile,
) -> Result<(), bkr1x_hal::DeviceError> {
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
fn acquire_single_instance_mutex() -> Option<isize> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\BKR1X_Daemon_SingleInstance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(h as isize)
    }
}

fn main() {
    let cfg = parse_cli_args();

    #[cfg(windows)]
    let _single_instance_guard = match acquire_single_instance_mutex() {
        Some(guard) => guard,
        None => {
            if !cfg.silent_if_running {
                let _ = send_ipc_request(&IpcRequest::OpenGui);
            }
            return;
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    let initial_state = HardwareStateMachine {
        config: MouseConfig::factory_default(),
        ..HardwareStateMachine::default()
    };
    let shared_snapshot = Arc::new(RwLock::new(snapshot_from_device(
        None,
        &initial_state,
        None,
    )));

    let (cmd_tx, cmd_rx): (Sender<DaemonCommand>, Receiver<DaemonCommand>) = mpsc::channel();

    #[cfg(windows)]
    let tray_notifier = tray_win::TrayNotifier::default();

    // 1. Spawn minimal 64 KiB-stack IPC listener thread
    let ipc_snapshot = Arc::clone(&shared_snapshot);
    let ipc_running = Arc::clone(&running);
    let ipc_tx = cmd_tx.clone();
    #[cfg(windows)]
    let ipc_tray = tray_notifier.clone();
    let _ipc_handle = thread::Builder::new()
        .name("bkr1x-ipc".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            run_ipc_listener(
                ipc_snapshot,
                ipc_tx,
                ipc_running,
                #[cfg(windows)]
                ipc_tray,
            )
        })
        .expect("failed to spawn IPC listener thread");

    // 2. Run hardware loop + optional Win32 System Tray host
    #[cfg(windows)]
    if !cfg.headless {
        let hw_snapshot = Arc::clone(&shared_snapshot);
        let hw_running = Arc::clone(&running);
        let hw_tray = tray_notifier.clone();
        let _hw_handle = thread::Builder::new()
            .name("bkr1x-hw".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                hardware_event_loop(cfg, hw_snapshot, cmd_rx, hw_running, Some(hw_tray));
            })
            .expect("failed to spawn hardware thread");

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

fn hardware_event_loop(
    cfg: DaemonConfig,
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_rx: Receiver<DaemonCommand>,
    running: Arc<AtomicBool>,
    #[cfg(windows)] tray: Option<tray_win::TrayNotifier>,
) {
    let mut app_profiles = load_app_profiles(&cfg.profiles_dir);
    let mut device: Option<Bkr1xDevice> = None;
    let mut active_profile_name: Option<String> = None;
    let mut saved_desktop_baseline: Option<HardwareProfile> = None;
    let mut in_auto_game_profile = false;
    let mut last_foreground_exe = String::new();
    let fallback_state = HardwareStateMachine::default();

    let notify_tray = || {
        #[cfg(windows)]
        if let Some(ref t) = tray {
            t.notify_state_changed();
        }
    };

    poll_hardware_once(
        &mut device,
        &fallback_state,
        &shared_snapshot,
        active_profile_name.as_deref(),
    );
    notify_tray();

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
                if let Some(ref t) = tray {
                    t.request_quit();
                }
                break;
            }
            Ok(DaemonCommand::ReloadProfiles) => {
                app_profiles = load_app_profiles(&cfg.profiles_dir);
            }
            Ok(DaemonCommand::GetFullState { reply }) => {
                if device.is_none() {
                    poll_hardware_once(
                        &mut device,
                        &fallback_state,
                        &shared_snapshot,
                        active_profile_name.as_deref(),
                    );
                    notify_tray();
                }
                let traces = device.as_mut().map(collect_ipc_traces);
                let snap = shared_snapshot.read().ok().map(|g| g.clone());
                let _ = reply.send(IpcResponse::Ok {
                    snapshot: snap,
                    traces,
                    message: None,
                });
            }
            Ok(DaemonCommand::Refresh { reply }) => {
                if let Some(dev) = device.as_mut() {
                    let _ = dev.sync_all();
                    if let Ok(mut guard) = shared_snapshot.write() {
                        *guard = snapshot_from_device(
                            Some(dev),
                            &fallback_state,
                            active_profile_name.as_deref(),
                        );
                    }
                } else {
                    poll_hardware_once(
                        &mut device,
                        &fallback_state,
                        &shared_snapshot,
                        active_profile_name.as_deref(),
                    );
                }
                notify_tray();
                let traces = device.as_mut().map(collect_ipc_traces);
                let snap = shared_snapshot.read().ok().map(|g| g.clone());
                let _ = reply.send(IpcResponse::Ok {
                    snapshot: snap,
                    traces,
                    message: Some("Hardware telemetry refreshed".to_owned()),
                });
                elapsed_since_battery = Duration::ZERO;
            }
            Ok(DaemonCommand::SetQuickDpi { dpi, reply }) => {
                if device.is_none() {
                    device = Bkr1xDevice::open_auto().ok();
                    if let Some(dev) = device.as_mut() {
                        let _ = dev.sync_all();
                    }
                }
                let resp = match device.as_mut() {
                    Some(dev) => {
                        let mut mcfg = dev.state().config;
                        if let Err(e) = validate_dpi(mcfg.active_dpi_stage as usize, dpi) {
                            IpcResponse::Error {
                                message: e.to_string(),
                            }
                        } else {
                            if let Some(idx) = mcfg.dpi_stages[..mcfg.dpi_count as usize]
                                .iter()
                                .position(|&d| d == dpi)
                            {
                                mcfg.active_dpi_stage = idx as u8;
                            } else {
                                mcfg.dpi_stages[mcfg.active_dpi_stage as usize] = dpi;
                            }
                            match dev.write_config(&mcfg) {
                                Ok(()) => {
                                    update_shared(
                                        Some(dev),
                                        &fallback_state,
                                        &shared_snapshot,
                                        active_profile_name.as_deref(),
                                    );
                                    notify_tray();
                                    IpcResponse::Ok {
                                        snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
                                        traces: Some(collect_ipc_traces(dev)),
                                        message: Some(format!("Switched to {dpi} DPI")),
                                    }
                                }
                                Err(e) => IpcResponse::Error {
                                    message: e.to_string(),
                                },
                            }
                        }
                    }
                    None => IpcResponse::Error {
                        message: bkr1x_hal::DeviceError::NotFound.to_string(),
                    },
                };
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::SetQuickRate { rate_hz, reply }) => {
                if device.is_none() {
                    device = Bkr1xDevice::open_auto().ok();
                    if let Some(dev) = device.as_mut() {
                        let _ = dev.sync_all();
                    }
                }
                let resp = match (device.as_mut(), PollingRate::from_hz(rate_hz)) {
                    (Some(dev), Ok(rate)) => {
                        let mut mcfg = dev.state().config;
                        mcfg.polling_rate = rate;
                        match dev.write_config(&mcfg) {
                            Ok(()) => {
                                update_shared(
                                    Some(dev),
                                    &fallback_state,
                                    &shared_snapshot,
                                    active_profile_name.as_deref(),
                                );
                                notify_tray();
                                IpcResponse::Ok {
                                    snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
                                    traces: Some(collect_ipc_traces(dev)),
                                    message: Some(format!("Switched polling rate to {rate_hz} Hz")),
                                }
                            }
                            Err(e) => IpcResponse::Error {
                                message: e.to_string(),
                            },
                        }
                    }
                    (_, Err(e)) => IpcResponse::Error {
                        message: e.to_string(),
                    },
                    (None, _) => IpcResponse::Error {
                        message: bkr1x_hal::DeviceError::NotFound.to_string(),
                    },
                };
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::FlashButtons { raw_slots, reply }) => {
                if device.is_none() {
                    device = Bkr1xDevice::open_auto().ok();
                }
                let mapping = ButtonMapping {
                    slots: core::array::from_fn(|i| ButtonAction::from_bytes(raw_slots[i])),
                };
                let resp = match device.as_mut() {
                    Some(dev) => match dev.write_buttons(&mapping) {
                        Ok(()) => {
                            update_shared(
                                Some(dev),
                                &fallback_state,
                                &shared_snapshot,
                                active_profile_name.as_deref(),
                            );
                            IpcResponse::Ok {
                                snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
                                traces: Some(collect_ipc_traces(dev)),
                                message: Some("Flashed 8-slot button map to BK-R1X.".to_owned()),
                            }
                        }
                        Err(e) => IpcResponse::Error {
                            message: e.to_string(),
                        },
                    },
                    None => IpcResponse::Error {
                        message: bkr1x_hal::DeviceError::NotFound.to_string(),
                    },
                };
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::FlashMacros { macros, reply }) => {
                if device.is_none() {
                    device = Bkr1xDevice::open_auto().ok();
                }
                let resp = match device.as_mut() {
                    Some(dev) => match decode_ipc_macros(&macros) {
                        Ok(vecs) => {
                            let slices: Vec<&[MacroStep]> =
                                vecs.iter().map(Vec::as_slice).collect();
                            match dev.write_macros(&slices) {
                                Ok(bytes) => IpcResponse::Ok {
                                    snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
                                    traces: Some(collect_ipc_traces(dev)),
                                    message: Some(format!(
                                        "Flashed {bytes} bytes of hardware macros to MCU."
                                    )),
                                },
                                Err(e) => IpcResponse::Error {
                                    message: e.to_string(),
                                },
                            }
                        }
                        Err(msg) => IpcResponse::Error { message: msg },
                    },
                    None => IpcResponse::Error {
                        message: bkr1x_hal::DeviceError::NotFound.to_string(),
                    },
                };
                let _ = reply.send(resp);
            }
            Ok(DaemonCommand::ApplyProfile { profile, reply }) => {
                if device.is_none() {
                    device = Bkr1xDevice::open_auto().ok();
                }
                let resp = match device.as_mut() {
                    Some(dev) => match apply_profile_to_device(dev, &profile) {
                        Ok(()) => {
                            active_profile_name = Some(profile.name.clone());
                            update_shared(
                                Some(dev),
                                &fallback_state,
                                &shared_snapshot,
                                active_profile_name.as_deref(),
                            );
                            notify_tray();
                            IpcResponse::Ok {
                                snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
                                traces: Some(collect_ipc_traces(dev)),
                                message: Some(format!("Applied profile '{}'", profile.name)),
                            }
                        }
                        Err(e) => IpcResponse::Error {
                            message: e.to_string(),
                        },
                    },
                    None => IpcResponse::Error {
                        message: bkr1x_hal::DeviceError::NotFound.to_string(),
                    },
                };
                let _ = reply.send(resp);
            }
            Err(RecvTimeoutError::Timeout) => {
                elapsed_since_battery += wait_quantum;

                if !app_profiles.is_empty()
                    && let Some(fg) = query_foreground_process_name()
                    && !fg.eq_ignore_ascii_case(&last_foreground_exe)
                {
                    last_foreground_exe = fg.clone();
                    let matched = app_profiles.iter().find(|p| {
                        p.match_process.as_deref().is_some_and(|pat| {
                            fg.to_ascii_lowercase().contains(&pat.to_ascii_lowercase())
                        })
                    });

                    if let Some(game_prof) = matched {
                        if active_profile_name.as_deref() != Some(game_prof.name.as_str())
                            && let Some(dev) = device.as_mut()
                        {
                            if !in_auto_game_profile {
                                saved_desktop_baseline = Some(HardwareProfile::from_hardware(
                                    "Desktop Baseline",
                                    &dev.state().config,
                                    &dev.state().buttons,
                                ));
                            }
                            if apply_profile_to_device(dev, game_prof).is_ok() {
                                in_auto_game_profile = true;
                                active_profile_name = Some(game_prof.name.clone());
                                update_shared(
                                    Some(dev),
                                    &fallback_state,
                                    &shared_snapshot,
                                    active_profile_name.as_deref(),
                                );
                                notify_tray();
                            }
                        }
                    } else if in_auto_game_profile {
                        let revert = cfg
                            .default_profile
                            .as_ref()
                            .or(saved_desktop_baseline.as_ref());
                        if let Some(base_prof) = revert
                            && let Some(dev) = device.as_mut()
                            && apply_profile_to_device(dev, base_prof).is_ok()
                        {
                            in_auto_game_profile = false;
                            active_profile_name = cfg.default_profile.as_ref().map(|p| p.name.clone());
                            update_shared(
                                Some(dev),
                                &fallback_state,
                                &shared_snapshot,
                                active_profile_name.as_deref(),
                            );
                            notify_tray();
                        }
                    }
                }

                if elapsed_since_battery >= cfg.poll_interval {
                    poll_hardware_once(
                        &mut device,
                        &fallback_state,
                        &shared_snapshot,
                        active_profile_name.as_deref(),
                    );
                    notify_tray();
                    elapsed_since_battery = Duration::ZERO;
                } else if let Some(dev) = device.as_mut()
                    && dev.drain_async_events() > 0
                {
                    update_shared(
                        Some(dev),
                        &fallback_state,
                        &shared_snapshot,
                        active_profile_name.as_deref(),
                    );
                    notify_tray();
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

fn update_shared(
    dev: Option<&Bkr1xDevice>,
    fallback: &HardwareStateMachine,
    shared: &Arc<RwLock<DaemonStatusSnapshot>>,
    active_profile: Option<&str>,
) {
    if let Ok(mut guard) = shared.write() {
        *guard = snapshot_from_device(dev, fallback, active_profile);
    }
}

fn decode_ipc_macros(entries: &[MacroProfileEntry]) -> Result<Vec<Vec<MacroStep>>, String> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut seq = Vec::with_capacity(entry.steps.len());
        for s in &entry.steps {
            let input = if s.kind.eq_ignore_ascii_case("mouse") {
                let btn = MouseClickButton::from_mask(s.code)
                    .ok_or_else(|| format!("invalid mouse button code {}", s.code))?;
                MacroInput::MouseButton(btn)
            } else {
                MacroInput::KeyboardKey(s.code)
            };
            seq.push(MacroStep {
                input,
                pressed: s.pressed,
                delay_ms: s.delay_ms,
            });
        }
        out.push(seq);
    }
    Ok(out)
}

fn poll_hardware_once(
    device: &mut Option<Bkr1xDevice>,
    fallback_state: &HardwareStateMachine,
    shared_snapshot: &Arc<RwLock<DaemonStatusSnapshot>>,
    active_profile: Option<&str>,
) {
    if device.is_none() {
        match Bkr1xDevice::open_auto() {
            Ok(mut dev) => {
                let _ = dev.sync_all();
                update_shared(Some(&dev), fallback_state, shared_snapshot, active_profile);
                *device = Some(dev);
                return;
            }
            Err(_) => {
                if let Ok(mut guard) = shared_snapshot.write() {
                    guard.connected = false;
                    guard.online = false;
                    guard.connection_mode = "Disconnected".to_owned();
                }
                return;
            }
        }
    }

    let mut disconnected = false;
    if let Some(dev) = device.as_mut() {
        match dev.poll_battery(Duration::from_secs(5)) {
            Ok(_) => {
                update_shared(Some(dev), fallback_state, shared_snapshot, active_profile);
            }
            Err(bkr1x_hal::DeviceError::Hid(_)) => {
                disconnected = true;
            }
            Err(_) => {}
        }
    }

    if disconnected {
        *device = None;
        if let Ok(mut guard) = shared_snapshot.write() {
            guard.connected = false;
            guard.online = false;
            guard.connection_mode = "Disconnected".to_owned();
        }
    }
}

fn handle_ipc_line(
    line: &str,
    shared_snapshot: &Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: &Sender<DaemonCommand>,
    running: &Arc<AtomicBool>,
    #[cfg(windows)] tray: &tray_win::TrayNotifier,
) -> IpcResponse {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        let snap = shared_snapshot.read().ok().map(|g| g.clone());
        return IpcResponse::Ok {
            snapshot: snap,
            traces: None,
            message: None,
        };
    }

    let req: IpcRequest = match serde_json::from_str(trimmed) {
        Ok(r) => r,
        Err(e) => {
            return IpcResponse::Error {
                message: format!("invalid JSON request: {e}"),
            };
        }
    };

    let dispatch_sync = |make_cmd: Box<dyn FnOnce(SyncSender<IpcResponse>) -> DaemonCommand>| {
        let (tx, rx) = mpsc::sync_channel(1);
        if cmd_tx.send(make_cmd(tx)).is_err() {
            return IpcResponse::Error {
                message: "daemon shutting down".to_owned(),
            };
        }
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap_or(IpcResponse::Error {
                message: "hardware command timed out".to_owned(),
            })
    };

    match req {
        IpcRequest::Status => IpcResponse::Ok {
            snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
            traces: None,
            message: None,
        },
        IpcRequest::GetFullState => {
            dispatch_sync(Box::new(|reply| DaemonCommand::GetFullState { reply }))
        }
        IpcRequest::Refresh => dispatch_sync(Box::new(|reply| DaemonCommand::Refresh { reply })),
        IpcRequest::SetQuickDpi { dpi } => {
            dispatch_sync(Box::new(move |reply| DaemonCommand::SetQuickDpi {
                dpi,
                reply,
            }))
        }
        IpcRequest::SetQuickRate { rate_hz } => {
            dispatch_sync(Box::new(move |reply| DaemonCommand::SetQuickRate {
                rate_hz,
                reply,
            }))
        }
        IpcRequest::FlashButtons { raw_slots } => {
            dispatch_sync(Box::new(move |reply| DaemonCommand::FlashButtons {
                raw_slots,
                reply,
            }))
        }
        IpcRequest::FlashMacros { macros } => {
            dispatch_sync(Box::new(move |reply| DaemonCommand::FlashMacros {
                macros,
                reply,
            }))
        }
        IpcRequest::ApplyProfile { profile } => {
            dispatch_sync(Box::new(move |reply| DaemonCommand::ApplyProfile {
                profile,
                reply,
            }))
        }
        IpcRequest::ReloadProfiles => {
            let _ = cmd_tx.send(DaemonCommand::ReloadProfiles);
            IpcResponse::Ok {
                snapshot: None,
                traces: None,
                message: Some("reloaded profiles".to_owned()),
            }
        }
        IpcRequest::OpenGui => {
            #[cfg(windows)]
            tray.request_open_gui();
            IpcResponse::Ok {
                snapshot: None,
                traces: None,
                message: Some("opened configurator".to_owned()),
            }
        }
        IpcRequest::Shutdown => {
            running.store(false, Ordering::SeqCst);
            let _ = cmd_tx.send(DaemonCommand::Shutdown);
            #[cfg(windows)]
            tray.request_quit();
            IpcResponse::Ok {
                snapshot: None,
                traces: None,
                message: Some("shutting down".to_owned()),
            }
        }
    }
}

#[cfg(unix)]
fn run_ipc_listener(
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let sock_path = bkr1x_hal::unix_socket_path();
    let _ = fs::remove_file(&sock_path);
    let Ok(listener) = UnixListener::bind(&sock_path) else {
        return;
    };
    let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));

    for stream_res in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(mut stream) = stream_res {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut line = String::new();
            if let Ok(cloned) = stream.try_clone() {
                let mut reader = BufReader::new(cloned);
                let _ = reader.read_line(&mut line);
            }
            let resp = handle_ipc_line(&line, &shared_snapshot, &cmd_tx, &running);
            if let Ok(mut json) = serde_json::to_string(&resp) {
                json.push('\n');
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.flush();
            }
        }
    }
}

#[cfg(windows)]
fn run_ipc_listener(
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
    tray: tray_win::TrayNotifier,
) {
    use bkr1x_hal::ipc::WINDOWS_NAMED_PIPE;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = WINDOWS_NAMED_PIPE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    while running.load(Ordering::Relaxed) {
        let pipe = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                16384,
                16384,
                500,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            thread::sleep(Duration::from_secs(1));
            continue;
        }

        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 }
            || unsafe { GetLastError() == ERROR_PIPE_CONNECTED };

        if connected {
            let mut buf = [0u8; 16384];
            let mut bytes_read = 0u32;
            let ok = unsafe {
                ReadFile(
                    pipe,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                ) != 0
            };
            if ok || bytes_read > 0 {
                let req_str = String::from_utf8_lossy(&buf[..bytes_read as usize]);
                let first_line = req_str.lines().next().unwrap_or("");
                let resp = handle_ipc_line(first_line, &shared_snapshot, &cmd_tx, &running, &tray);
                if let Ok(mut out) = serde_json::to_string(&resp) {
                    out.push('\n');
                    let mut written = 0u32;
                    unsafe {
                        let _ = WriteFile(
                            pipe,
                            out.as_ptr(),
                            out.len() as u32,
                            &mut written,
                            std::ptr::null_mut(),
                        );
                        let _ = FlushFileBuffers(pipe);
                    }
                }
            }
        }

        unsafe {
            let _ = DisconnectNamedPipe(pipe);
            let _ = CloseHandle(pipe);
        }
    }
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
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&full)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
    }
}

#[cfg(not(windows))]
fn query_foreground_process_name() -> Option<String> {
    None
}
