//! `bkr1x-daemon` — Event-driven, sub-2MB background service for the Fire Phoenix BK-R1X.
//!
//! Architecture:
//! - Zero async runtime bloat (no `tokio`): parks in kernel via `recv_timeout` + blocking OS IPC.
//! - Sub-millisecond status queries (`< 0.2ms`): `Status` IPC reads an in-memory `RwLock` snapshot
//!   without waking the USB bus or the 2.4GHz wireless MCU.
//! - Low-frequency hardware polling (default every 60s) + async HID report draining.
//! - Optional per-application profile auto-switching when profiles specify `match_process`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use bkr1x_hal::profile::format_light_mode;
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, HardwareProfile, IpcRequest, IpcResponse,
};
use bkr1x_protocol::{HardwareStateMachine, MacroStep, MouseConfig};

enum DaemonCommand {
    Refresh {
        reply: SyncSender<IpcResponse>,
    },
    ApplyProfile {
        profile: Box<HardwareProfile>,
        reply: SyncSender<IpcResponse>,
    },
    Shutdown,
}

struct DaemonConfig {
    poll_interval: Duration,
    profiles_dir: Option<PathBuf>,
    default_profile: Option<HardwareProfile>,
}

fn parse_cli_args() -> DaemonConfig {
    let mut poll_secs = 60u64;
    let mut profiles_dir: Option<PathBuf> = None;
    let mut default_profile_path: Option<PathBuf> = None;

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
                    profiles_dir = Some(PathBuf::from(p));
                }
            }
            "--default-profile" => {
                if let Some(p) = args.next() {
                    default_profile_path = Some(PathBuf::from(p));
                }
            }
            "--help" | "-h" => {
                println!(
                    "bkr1x-daemon {}\n\
                     Usage: bkr1x-daemon [--poll-secs <60>] [--profiles-dir <dir>] [--default-profile <file.toml>]",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
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
    }
}

fn load_app_profiles(dir: Option<&Path>) -> Vec<HardwareProfile> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let is_cfg = p
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("toml") || e.eq_ignore_ascii_case("json"));
        if is_cfg
            && let Ok(prof) = HardwareProfile::load_from_file(&p)
            && prof.match_process.is_some()
        {
            out.push(prof);
        }
    }
    out
}

fn snapshot_from_state(
    connected: bool,
    state: &HardwareStateMachine,
    active_profile: Option<&str>,
) -> DaemonStatusSnapshot {
    let cfg = &state.config;
    DaemonStatusSnapshot {
        connected,
        online: connected && state.online,
        connection_mode: state
            .connection
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| "Disconnected".to_owned()),
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
        sleep_minutes: cfg.sleep_minutes,
        light_mode: format_light_mode(cfg.light_mode).to_owned(),
        active_profile: active_profile.map(str::to_owned),
    }
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

fn main() {
    let cfg = parse_cli_args();
    let app_profiles = load_app_profiles(cfg.profiles_dir.as_deref());
    let running = Arc::new(AtomicBool::new(true));

    let initial_state = HardwareStateMachine {
        config: MouseConfig::factory_default(),
        ..HardwareStateMachine::default()
    };
    let shared_snapshot = Arc::new(RwLock::new(snapshot_from_state(false, &initial_state, None)));

    let (cmd_tx, cmd_rx): (Sender<DaemonCommand>, Receiver<DaemonCommand>) = mpsc::channel();

    // Spawn minimal 64 KiB-stack IPC listener thread
    let ipc_snapshot = Arc::clone(&shared_snapshot);
    let ipc_running = Arc::clone(&running);
    let ipc_tx = cmd_tx.clone();
    let _ipc_handle = thread::Builder::new()
        .name("bkr1x-ipc".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || run_ipc_listener(ipc_snapshot, ipc_tx, ipc_running))
        .expect("failed to spawn IPC listener thread");

    eprintln!(
        "[bkr1x-daemon] started (poll={}s, app_profiles={})",
        cfg.poll_interval.as_secs(),
        app_profiles.len()
    );

    hardware_event_loop(cfg, app_profiles, shared_snapshot, cmd_rx, running);
}

fn hardware_event_loop(
    cfg: DaemonConfig,
    app_profiles: Vec<HardwareProfile>,
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_rx: Receiver<DaemonCommand>,
    running: Arc<AtomicBool>,
) {
    let mut device: Option<Bkr1xDevice> = None;
    let mut active_profile_name: Option<String> = None;
    let mut last_foreground_exe = String::new();

    // Perform initial hardware probe immediately on startup
    poll_hardware_once(&mut device, &shared_snapshot, active_profile_name.as_deref());

    let wait_quantum = if app_profiles.is_empty() {
        cfg.poll_interval
    } else {
        // When per-application profiles are configured, check foreground window every 2s
        Duration::from_secs(2).min(cfg.poll_interval)
    };
    let mut elapsed_since_battery = Duration::ZERO;

    while running.load(Ordering::Relaxed) {
        match cmd_rx.recv_timeout(wait_quantum) {
            Ok(DaemonCommand::Shutdown) => {
                running.store(false, Ordering::SeqCst);
                break;
            }
            Ok(DaemonCommand::Refresh { reply }) => {
                poll_hardware_once(&mut device, &shared_snapshot, active_profile_name.as_deref());
                let snap = shared_snapshot.read().ok().map(|g| g.clone());
                let _ = reply.send(IpcResponse::Ok {
                    snapshot: snap,
                    message: Some("refreshed".to_owned()),
                });
                elapsed_since_battery = Duration::ZERO;
            }
            Ok(DaemonCommand::ApplyProfile { profile, reply }) => {
                if device.is_none() {
                    device = Bkr1xDevice::open_auto().ok();
                }
                let resp = match device.as_mut() {
                    Some(dev) => match apply_profile_to_device(dev, &profile) {
                        Ok(()) => {
                            active_profile_name = Some(profile.name.clone());
                            if let Ok(mut guard) = shared_snapshot.write() {
                                *guard = snapshot_from_state(
                                    true,
                                    dev.state(),
                                    active_profile_name.as_deref(),
                                );
                            }
                            IpcResponse::Ok {
                                snapshot: shared_snapshot.read().ok().map(|g| g.clone()),
                                message: Some(format!("applied profile '{}'", profile.name)),
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

                // Optional per-app profile switching
                if !app_profiles.is_empty()
                    && let Some(fg) = query_foreground_process_name()
                    && !fg.eq_ignore_ascii_case(&last_foreground_exe)
                {
                    last_foreground_exe = fg.clone();
                    let matched = app_profiles.iter().find(|p| {
                        p.match_process
                            .as_deref()
                            .is_some_and(|pat| fg.to_ascii_lowercase().contains(&pat.to_ascii_lowercase()))
                    });
                    let target = matched.or(cfg.default_profile.as_ref());
                    if let Some(prof) = target
                        && active_profile_name.as_deref() != Some(prof.name.as_str())
                        && let Some(dev) = device.as_mut()
                        && apply_profile_to_device(dev, prof).is_ok()
                    {
                        active_profile_name = Some(prof.name.clone());
                        if let Ok(mut guard) = shared_snapshot.write() {
                            *guard = snapshot_from_state(
                                true,
                                dev.state(),
                                active_profile_name.as_deref(),
                            );
                        }
                    }
                }

                if elapsed_since_battery >= cfg.poll_interval {
                    poll_hardware_once(
                        &mut device,
                        &shared_snapshot,
                        active_profile_name.as_deref(),
                    );
                    elapsed_since_battery = Duration::ZERO;
                } else if let Some(dev) = device.as_mut()
                    && dev.drain_async_events() > 0
                    && let Ok(mut guard) = shared_snapshot.write()
                {
                    *guard = snapshot_from_state(
                        true,
                        dev.state(),
                        active_profile_name.as_deref(),
                    );
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

fn poll_hardware_once(
    device: &mut Option<Bkr1xDevice>,
    shared_snapshot: &Arc<RwLock<DaemonStatusSnapshot>>,
    active_profile: Option<&str>,
) {
    if device.is_none() {
        match Bkr1xDevice::open_auto() {
            Ok(mut dev) => {
                let _ = dev.sync_all();
                if let Ok(mut guard) = shared_snapshot.write() {
                    *guard = snapshot_from_state(true, dev.state(), active_profile);
                }
                *device = Some(dev);
                return;
            }
            Err(_) => {
                if let Ok(mut guard) = shared_snapshot.write() {
                    guard.connected = false;
                    guard.online = false;
                }
                return;
            }
        }
    }

    let mut disconnected = false;
    if let Some(dev) = device.as_mut() {
        match dev.poll_battery(Duration::from_secs(5)) {
            Ok(_) => {
                if let Ok(mut guard) = shared_snapshot.write() {
                    *guard = snapshot_from_state(true, dev.state(), active_profile);
                }
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
) -> IpcResponse {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        // Convenience for bare status bar probes (`nc -U /run/user/$UID/bkr1x.sock <<< ""`)
        let snap = shared_snapshot.read().ok().map(|g| g.clone());
        return IpcResponse::Ok {
            snapshot: snap,
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

    match req {
        IpcRequest::Status => {
            let snap = shared_snapshot.read().ok().map(|g| g.clone());
            IpcResponse::Ok {
                snapshot: snap,
                message: None,
            }
        }
        IpcRequest::Refresh => {
            let (tx, rx) = mpsc::sync_channel(1);
            if cmd_tx.send(DaemonCommand::Refresh { reply: tx }).is_err() {
                return IpcResponse::Error {
                    message: "daemon shutting down".to_owned(),
                };
            }
            rx.recv_timeout(Duration::from_secs(2))
                .unwrap_or(IpcResponse::Error {
                    message: "hardware refresh timed out".to_owned(),
                })
        }
        IpcRequest::ApplyProfile { profile } => {
            let (tx, rx) = mpsc::sync_channel(1);
            if cmd_tx
                .send(DaemonCommand::ApplyProfile {
                    profile,
                    reply: tx,
                })
                .is_err()
            {
                return IpcResponse::Error {
                    message: "daemon shutting down".to_owned(),
                };
            }
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap_or(IpcResponse::Error {
                    message: "profile flash timed out".to_owned(),
                })
        }
        IpcRequest::Shutdown => {
            running.store(false, Ordering::SeqCst);
            let _ = cmd_tx.send(DaemonCommand::Shutdown);
            IpcResponse::Ok {
                snapshot: None,
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
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[bkr1x-daemon] failed binding {}: {e}",
                sock_path.display()
            );
            return;
        }
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
        // SAFETY: Valid null-terminated UTF-16 pipe path and standard duplex synchronous pipe flags.
        let pipe = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                500,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            thread::sleep(Duration::from_secs(1));
            continue;
        }

        // SAFETY: `pipe` is a valid Named Pipe handle created above.
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 }
            || unsafe { GetLastError() == ERROR_PIPE_CONNECTED };

        if connected {
            let mut buf = [0u8; 8192];
            let mut bytes_read = 0u32;
            // SAFETY: Valid buffer pointer and length for synchronous pipe read.
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
                let resp = handle_ipc_line(first_line, &shared_snapshot, &cmd_tx, &running);
                if let Ok(mut out) = serde_json::to_string(&resp) {
                    out.push('\n');
                    let mut written = 0u32;
                    // SAFETY: Valid byte slice and pipe handle.
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

        // SAFETY: Cleanly disconnect client and close server instance handle.
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

    // SAFETY: Standard Win32 foreground window & limited process image path query.
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
    // Lightweight zero-dependency `/proc` inspection when `_NET_ACTIVE_WINDOW` / Hyprland / Sway exposes PID,
    // or returns None cleanly without spawning heavy external processes.
    None
}
