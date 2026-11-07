#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! `bkr1x-gui` — On-demand reactive OpenGL (`glow`) configurator for the Fire Phoenix BK-R1X.
//!
//! Coordinates seamlessly with the `< 2 MB` `bkr1x-daemon` System Tray agent:
//! - Opening from the taskbar tray icon loads live telemetry & config in milliseconds over IPC.
//! - Closing the window (`X`) exits the OpenGL process cleanly to free 100% of GUI RAM while
//!   leaving the BK-R1X System Tray icon active in the taskbar notification area.

mod keycodes;

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use bkr1x_hal::profile::{MacroProfileEntry, MacroStepSpec, parse_light_mode};
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, DeviceError, HardwareProfile, IpcRequest, IpcResponse,
    IpcWireTrace, TraceDirection, TrayIconStatus, WireTraceEntry, ensure_user_profiles_dir,
    install_user_application, is_autostart_enabled, is_daemon_running, list_saved_profiles,
    render_mouse_icon_rgba, resolve_sibling_binary, send_ipc_request, set_autostart_enabled,
};
use bkr1x_protocol::{
    BUTTON_SLOT_COUNT, BatteryTelemetry, ButtonAction, ButtonMapping, ButtonSlot, ChecksumStrategy,
    ConnectionMode, DEFAULT_STAGE_COLORS, FirmwareVersion, HardwareStateMachine, KeyModifiers,
    LightMode, LodHeight, MACRO_FLASH_TOTAL_LEN, MAX_DPI_STAGES, MacroInput, MacroStep,
    MacroTrigger, MediaFunction, MouseClickButton, MouseConfig, PAW3311_DPI_MAX, PAW3311_DPI_MIN,
    PAW3311_DPI_STEP, PacketCodec, PollingRate, REPORT_PAYLOAD_LEN, ScrollDirection, SensorFlags,
    VALID_SLEEP_MINUTES, compile_macro_bank,
};
use eframe::egui::{self, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::keycodes::{HID_KEY_TABLE, keycode_label};

pub const GUI_WINDOW_TITLE: &str = "Fire Phoenix BK-R1X — Native PAW3311 Configurator";
const MAX_UI_TRACE_LOG: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Performance,
    ButtonsAndMacros,
    ProfilesAndStartup,
    TelemetryInspector,
}

#[derive(Debug, Clone)]
struct UiMacroSlot {
    name: String,
    steps: Vec<MacroStep>,
}

enum WorkerCommand {
    ConnectAndSync { force_refresh: bool },
    FlashConfig(MouseConfig),
    FlashButtons(ButtonMapping),
    FlashMacros(Vec<UiMacroSlot>),
    ApplySavedProfile(Box<HardwareProfile>),
    Shutdown,
}

enum WorkerEvent {
    Connected {
        state: Box<HardwareStateMachine>,
        product_name: String,
        vid: u16,
        pid: u16,
        via_daemon: bool,
        traces: Vec<WireTraceEntry>,
    },
    FlashSuccess {
        message: String,
        traces: Vec<WireTraceEntry>,
    },
    ConnectionFailed {
        error: DeviceError,
    },
}

fn ipc_traces_to_hal(ipc_traces: Option<Vec<IpcWireTrace>>) -> Vec<WireTraceEntry> {
    let Some(list) = ipc_traces else {
        return Vec::new();
    };
    list.into_iter()
        .map(|t| {
            let mut payload = [0u8; REPORT_PAYLOAD_LEN];
            for (idx, hex_b) in t.payload_hex.split_whitespace().take(64).enumerate() {
                payload[idx] = u8::from_str_radix(hex_b, 16).unwrap_or(0);
            }
            WireTraceEntry {
                timestamp_unix_ms: t.timestamp_unix_ms,
                direction: if t.is_out {
                    TraceDirection::Out
                } else {
                    TraceDirection::In
                },
                report_id: t.report_id,
                payload,
                summary: t.summary,
            }
        })
        .collect()
}

fn snapshot_to_hw_state(snap: &DaemonStatusSnapshot) -> HardwareStateMachine {
    let mut dpi_stages = MouseConfig::factory_default().dpi_stages;
    for (i, &d) in snap.dpi_stages.iter().take(MAX_DPI_STAGES).enumerate() {
        dpi_stages[i] = d;
    }
    let dpi_count = (snap.dpi_stages.len() as u8).clamp(1, MAX_DPI_STAGES as u8);
    let active_dpi_stage = snap.active_dpi_stage.saturating_sub(1).min(dpi_count - 1);

    let mut fw_parts = snap
        .firmware_version
        .split('.')
        .map(|s| s.parse::<u8>().unwrap_or(0));
    let fw = FirmwareVersion {
        major: fw_parts.next().unwrap_or(1),
        minor: fw_parts.next().unwrap_or(0),
        patch: fw_parts.next().unwrap_or(0),
    };

    let connection = if snap.connection_mode.contains("Wired") {
        Some(ConnectionMode::UsbWired)
    } else if snap.connected {
        Some(ConnectionMode::Wireless24Ghz)
    } else {
        None
    };

    let buttons = match snap.button_slots {
        Some(raw) => ButtonMapping {
            slots: core::array::from_fn(|i| ButtonAction::from_bytes(raw[i])),
        },
        None => ButtonMapping::factory_default(),
    };

    HardwareStateMachine {
        connection,
        online: snap.online,
        firmware: fw,
        battery: BatteryTelemetry {
            percent: snap.battery_percent,
            charging: snap.charging,
        },
        config: MouseConfig {
            dpi_stages,
            dpi_count,
            active_dpi_stage,
            polling_rate: PollingRate::from_hz(snap.polling_rate_hz).unwrap_or(PollingRate::Hz1000),
            lod: LodHeight::from_mm(snap.lod_mm).unwrap_or(LodHeight::Mm1),
            debounce_ms: snap.debounce_ms,
            sensor_flags: SensorFlags {
                ripple_control: snap.ripple_control,
                angle_snapping: snap.angle_snapping,
                motion_sync: snap.motion_sync,
                reserved_bits: SensorFlags::BIT_FACTORY_BASE,
            },
            sleep_minutes: snap.sleep_minutes,
            light_mode: parse_light_mode(&snap.light_mode).unwrap_or(LightMode::Off),
            scroll_direction: if snap.reverse_scroll {
                ScrollDirection::Reverse
            } else {
                ScrollDirection::Normal
            },
            highspeed_mode: snap.highspeed_mode,
            wakeup_on_move: snap.wakeup_on_move,
            move_lights_off: snap.move_lights_off,
        },
        buttons,
    }
}

fn spawn_hardware_worker(
    egui_ctx: egui::Context,
) -> (
    Sender<WorkerCommand>,
    Receiver<WorkerEvent>,
    JoinHandle<()>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
    let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();

    let handle = thread::Builder::new()
        .name("bkr1x-gui-hid".to_owned())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut direct_dev: Option<Bkr1xDevice> = None;

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    WorkerCommand::Shutdown => break,
                    WorkerCommand::ConnectAndSync { force_refresh } => {
                        let ipc_req = if force_refresh {
                            IpcRequest::Refresh
                        } else {
                            IpcRequest::GetFullState
                        };
                        if let Ok(IpcResponse::Ok {
                            snapshot: Some(snap),
                            traces,
                            ..
                        }) = send_ipc_request(&ipc_req)
                        {
                            direct_dev = None; // Release any direct handle when daemon owns HID
                            if snap.connected {
                                let state = Box::new(snapshot_to_hw_state(&snap));
                                let _ = evt_tx.send(WorkerEvent::Connected {
                                    state,
                                    product_name: if snap.product_name.is_empty() {
                                        "BK-R1X".to_owned()
                                    } else {
                                        snap.product_name
                                    },
                                    vid: snap.vendor_id,
                                    pid: snap.product_id,
                                    via_daemon: true,
                                    traces: ipc_traces_to_hal(traces),
                                });
                                egui_ctx.request_repaint();
                                continue;
                            }
                        }

                        // Fallback to direct HID if daemon isn't running
                        match Bkr1xDevice::open_auto() {
                            Ok(mut dev) => {
                                let _ = dev.sync_all();
                                let info = dev.info().clone();
                                let state = Box::new(*dev.state());
                                let traces: Vec<_> = dev.trace_log().iter().cloned().collect();
                                dev.clear_trace_log();
                                direct_dev = Some(dev);
                                let _ = evt_tx.send(WorkerEvent::Connected {
                                    state,
                                    product_name: info.product_name,
                                    vid: info.vendor_id,
                                    pid: info.product_id,
                                    via_daemon: false,
                                    traces,
                                });
                            }
                            Err(error) => {
                                direct_dev = None;
                                let _ = evt_tx.send(WorkerEvent::ConnectionFailed { error });
                            }
                        }
                        egui_ctx.request_repaint();
                    }
                    WorkerCommand::FlashConfig(cfg) => {
                        let prof = HardwareProfile::from_hardware(
                            "Active Config",
                            &cfg,
                            &ButtonMapping::factory_default(),
                        );
                        let mut prof_no_btns = prof;
                        prof_no_btns.buttons = None;

                        if let Ok(resp) = send_ipc_request(&IpcRequest::ApplyProfile {
                            profile: Box::new(prof_no_btns),
                        }) {
                            match resp {
                                IpcResponse::Ok { traces, .. } => {
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: format!(
                                            "Flashed PAW3311 via Tray Agent: {} DPI @ {} Hz (LOD {}mm, Debounce {}ms)",
                                            cfg.active_dpi(),
                                            cfg.polling_rate.as_hz(),
                                            cfg.lod.as_mm(),
                                            cfg.debounce_ms
                                        ),
                                        traces: ipc_traces_to_hal(traces),
                                    });
                                }
                                IpcResponse::Error { message } => {
                                    let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                        error: DeviceError::Ipc(message),
                                    });
                                }
                            }
                            egui_ctx.request_repaint();
                            continue;
                        }

                        if direct_dev.is_none() {
                            direct_dev = Bkr1xDevice::open_auto().ok();
                        }
                        match direct_dev.as_mut() {
                            Some(dev) => match dev
                                .write_config(&cfg)
                                .and_then(|()| dev.write_light_mode(cfg.light_mode))
                            {
                                Ok(()) => {
                                    let traces: Vec<_> = dev.trace_log().iter().cloned().collect();
                                    dev.clear_trace_log();
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: format!(
                                            "Flashed PAW3311: {} DPI @ {} Hz",
                                            cfg.active_dpi(),
                                            cfg.polling_rate.as_hz()
                                        ),
                                        traces,
                                    });
                                }
                                Err(error) => {
                                    let _ = evt_tx.send(WorkerEvent::ConnectionFailed { error });
                                }
                            },
                            None => {
                                let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                    error: DeviceError::NotFound,
                                });
                            }
                        }
                        egui_ctx.request_repaint();
                    }
                    WorkerCommand::FlashButtons(buttons) => {
                        let raw_slots: [[u8; 4]; 8] =
                            core::array::from_fn(|i| buttons.slots[i].to_bytes());
                        if let Ok(resp) = send_ipc_request(&IpcRequest::FlashButtons { raw_slots })
                        {
                            match resp {
                                IpcResponse::Ok {
                                    traces, message, ..
                                } => {
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: message.unwrap_or_else(|| {
                                            "Flashed button map via Tray Agent.".to_owned()
                                        }),
                                        traces: ipc_traces_to_hal(traces),
                                    });
                                }
                                IpcResponse::Error { message } => {
                                    let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                        error: DeviceError::Ipc(message),
                                    });
                                }
                            }
                            egui_ctx.request_repaint();
                            continue;
                        }

                        if direct_dev.is_none() {
                            direct_dev = Bkr1xDevice::open_auto().ok();
                        }
                        match direct_dev.as_mut() {
                            Some(dev) => match dev.write_buttons(&buttons) {
                                Ok(()) => {
                                    let traces: Vec<_> = dev.trace_log().iter().cloned().collect();
                                    dev.clear_trace_log();
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: "Flashed 8-slot button map to MCU.".to_owned(),
                                        traces,
                                    });
                                }
                                Err(error) => {
                                    let _ = evt_tx.send(WorkerEvent::ConnectionFailed { error });
                                }
                            },
                            None => {
                                let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                    error: DeviceError::NotFound,
                                });
                            }
                        }
                        egui_ctx.request_repaint();
                    }
                    WorkerCommand::FlashMacros(ui_macros) => {
                        let entries: Vec<MacroProfileEntry> = ui_macros
                            .iter()
                            .map(|m| MacroProfileEntry {
                                name: m.name.clone(),
                                steps: m
                                    .steps
                                    .iter()
                                    .map(|s| {
                                        let (kind, code) = match s.input {
                                            MacroInput::MouseButton(b) => {
                                                ("mouse".to_owned(), b as u8)
                                            }
                                            MacroInput::KeyboardKey(kc) => ("key".to_owned(), kc),
                                        };
                                        MacroStepSpec {
                                            kind,
                                            code,
                                            pressed: s.pressed,
                                            delay_ms: s.delay_ms,
                                        }
                                    })
                                    .collect(),
                            })
                            .collect();

                        if let Ok(resp) =
                            send_ipc_request(&IpcRequest::FlashMacros { macros: entries })
                        {
                            match resp {
                                IpcResponse::Ok {
                                    traces, message, ..
                                } => {
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: message.unwrap_or_else(|| {
                                            "Flashed macro bank via Tray Agent.".to_owned()
                                        }),
                                        traces: ipc_traces_to_hal(traces),
                                    });
                                }
                                IpcResponse::Error { message } => {
                                    let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                        error: DeviceError::Ipc(message),
                                    });
                                }
                            }
                            egui_ctx.request_repaint();
                            continue;
                        }

                        if direct_dev.is_none() {
                            direct_dev = Bkr1xDevice::open_auto().ok();
                        }
                        match direct_dev.as_mut() {
                            Some(dev) => {
                                let vecs: Vec<Vec<MacroStep>> =
                                    ui_macros.into_iter().map(|m| m.steps).collect();
                                let slices: Vec<&[MacroStep]> =
                                    vecs.iter().map(Vec::as_slice).collect();
                                match dev.write_macros(&slices) {
                                    Ok(bytes) => {
                                        let traces: Vec<_> =
                                            dev.trace_log().iter().cloned().collect();
                                        dev.clear_trace_log();
                                        let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                            message: format!(
                                                "Flashed {bytes} bytes of hardware macros to MCU."
                                            ),
                                            traces,
                                        });
                                    }
                                    Err(error) => {
                                        let _ =
                                            evt_tx.send(WorkerEvent::ConnectionFailed { error });
                                    }
                                }
                            }
                            None => {
                                let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                    error: DeviceError::NotFound,
                                });
                            }
                        }
                        egui_ctx.request_repaint();
                    }
                    WorkerCommand::ApplySavedProfile(prof) => {
                        if let Ok(resp) = send_ipc_request(&IpcRequest::ApplyProfile {
                            profile: prof.clone(),
                        }) {
                            match resp {
                                IpcResponse::Ok {
                                    traces, message, ..
                                } => {
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: message.unwrap_or_else(|| {
                                            format!("Applied profile '{}'", prof.name)
                                        }),
                                        traces: ipc_traces_to_hal(traces),
                                    });
                                }
                                IpcResponse::Error { message } => {
                                    let _ = evt_tx.send(WorkerEvent::ConnectionFailed {
                                        error: DeviceError::Ipc(message),
                                    });
                                }
                            }
                            egui_ctx.request_repaint();
                        }
                    }
                }
            }
        })
        .expect("failed spawning HID worker thread");

    (cmd_tx, evt_rx, handle)
}

/// Ensures `bkr1x-daemon` is running in the system tray so closing the GUI keeps the tray icon active.
fn ensure_tray_daemon_spawned() -> bool {
    if is_daemon_running() {
        return true;
    }
    let Some(daemon_exe) = resolve_sibling_binary("bkr1x-daemon") else {
        return false;
    };
    let mut cmd = std::process::Command::new(&daemon_exe);
    cmd.arg("--silent");
    if let Some(p) = daemon_exe.parent() {
        cmd.current_dir(p);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().is_ok()
}

struct Bkr1xGuiApp {
    tab: ActiveTab,
    hw_state: HardwareStateMachine,
    connected_device_label: Option<String>,
    connected_via_daemon: bool,
    tray_agent_running: bool,
    autostart_enabled: bool,
    last_error: Option<DeviceError>,
    status_banner: String,

    stage_colors: [[u8; 3]; MAX_DPI_STAGES],

    // Button & macro builder state
    selected_slot: usize,
    edit_sc_ctrl: bool,
    edit_sc_shift: bool,
    edit_sc_alt: bool,
    edit_sc_win: bool,
    edit_sc_keycode: u8,
    edit_macro_slot: u8,
    edit_macro_repeat: u8,
    edit_macro_mode: u8,

    macros: Vec<UiMacroSlot>,
    selected_macro_idx: usize,
    new_step_is_mouse: bool,
    new_step_keycode: u8,
    new_step_mouse_btn: MouseClickButton,
    new_step_delay_ms: u16,

    // Profiles & Startup manager state
    saved_profiles: Vec<(PathBuf, HardwareProfile)>,
    new_profile_name: String,
    new_profile_process: String,

    // Raw HID Debugger state
    show_raw_debugger_drawer: bool,
    trace_entries: VecDeque<WireTraceEntry>,
    custom_hex_input: String,
    custom_hex_decoded: String,

    worker_tx: Sender<WorkerCommand>,
    worker_rx: Receiver<WorkerEvent>,
    worker_join: Option<JoinHandle<()>>,
}

impl Bkr1xGuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_dark_technical_theme(&cc.egui_ctx);
        let tray_started = ensure_tray_daemon_spawned();
        let saved_profiles = list_saved_profiles();
        let autostart_enabled = is_autostart_enabled();

        let (worker_tx, worker_rx, join) = spawn_hardware_worker(cc.egui_ctx.clone());
        let _ = worker_tx.send(WorkerCommand::ConnectAndSync {
            force_refresh: false,
        });

        let default_macro = UiMacroSlot {
            name: "Double Tap A".to_owned(),
            steps: vec![
                MacroStep {
                    input: MacroInput::KeyboardKey(0x04),
                    pressed: true,
                    delay_ms: 12,
                },
                MacroStep {
                    input: MacroInput::KeyboardKey(0x04),
                    pressed: false,
                    delay_ms: 20,
                },
            ],
        };

        Self {
            tab: ActiveTab::Performance,
            hw_state: HardwareStateMachine::new(),
            connected_device_label: None,
            connected_via_daemon: false,
            tray_agent_running: tray_started,
            autostart_enabled,
            last_error: None,
            status_banner: "Synchronizing with BK-R1X hardware...".to_owned(),
            stage_colors: DEFAULT_STAGE_COLORS,
            selected_slot: 0,
            edit_sc_ctrl: true,
            edit_sc_shift: false,
            edit_sc_alt: false,
            edit_sc_win: false,
            edit_sc_keycode: 0x06,
            edit_macro_slot: 0,
            edit_macro_repeat: 1,
            edit_macro_mode: 2,
            macros: vec![default_macro],
            selected_macro_idx: 0,
            new_step_is_mouse: false,
            new_step_keycode: 0x04,
            new_step_mouse_btn: MouseClickButton::Left,
            new_step_delay_ms: 15,
            saved_profiles,
            new_profile_name: "My Custom Profile".to_owned(),
            new_profile_process: String::new(),
            show_raw_debugger_drawer: false,
            trace_entries: VecDeque::with_capacity(MAX_UI_TRACE_LOG),
            custom_hex_input:
                "00 55 0f ae 0a 2f 01 01 00 00 02 04 06 03 20 03 b0 04 40 06 80 0c 88 13 50 46"
                    .to_owned(),
            custom_hex_decoded: String::new(),
            worker_tx,
            worker_rx,
            worker_join: Some(join),
        }
    }

    fn process_worker_events(&mut self) {
        while let Ok(evt) = self.worker_rx.try_recv() {
            match evt {
                WorkerEvent::Connected {
                    state,
                    product_name,
                    vid,
                    pid,
                    via_daemon,
                    traces,
                } => {
                    self.hw_state = *state;
                    self.connected_via_daemon = via_daemon;
                    self.tray_agent_running = via_daemon || is_daemon_running();
                    self.connected_device_label =
                        Some(format!("{product_name} (0x{vid:04X}:0x{pid:04X})"));
                    self.last_error = None;
                    self.status_banner = format!(
                        "Connected via {} {} — Firmware v{} · Closing window keeps BK-R1X active in System Tray",
                        self.hw_state
                            .connection
                            .map(|m| m.as_str())
                            .unwrap_or("USB"),
                        if via_daemon { "(Tray Agent)" } else { "(Direct HID)" },
                        self.hw_state.firmware
                    );
                    self.append_traces(traces);
                }
                WorkerEvent::FlashSuccess { message, traces } => {
                    self.last_error = None;
                    self.status_banner = message;
                    self.append_traces(traces);
                }
                WorkerEvent::ConnectionFailed { error } => {
                    self.status_banner = format!("Hardware notice: {error}");
                    if matches!(
                        error,
                        DeviceError::NotFound | DeviceError::BluetoothConfigUnsupported
                    ) {
                        self.connected_device_label = None;
                    }
                    self.last_error = Some(error);
                }
            }
        }
    }

    fn append_traces(&mut self, incoming: Vec<WireTraceEntry>) {
        for entry in incoming {
            if self.trace_entries.len() >= MAX_UI_TRACE_LOG {
                self.trace_entries.pop_front();
            }
            self.trace_entries.push_back(entry);
        }
    }

    fn record_preview_packet(
        &mut self,
        report_id: u8,
        payload: [u8; REPORT_PAYLOAD_LEN],
        note: &str,
    ) {
        if self.trace_entries.len() >= MAX_UI_TRACE_LOG {
            self.trace_entries.pop_front();
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.trace_entries.push_back(WireTraceEntry {
            timestamp_unix_ms: ts,
            direction: TraceDirection::Out,
            report_id,
            payload,
            summary: note.to_owned(),
        });
    }

    fn trigger_flash_config(&mut self) {
        let codec = PacketCodec::new(ChecksumStrategy::None);
        match codec.encode_set_mouse_config(&self.hw_state.config) {
            Ok(rpt) => {
                if self.connected_device_label.is_none() {
                    self.record_preview_packet(
                        rpt.report_id,
                        rpt.payload,
                        "PREVIEW TxSetMouseConfig (0x0F)",
                    );
                }
                let _ = self
                    .worker_tx
                    .send(WorkerCommand::FlashConfig(self.hw_state.config));
            }
            Err(e) => {
                self.status_banner = format!("Validation rejected: {e}");
            }
        }
    }

    fn trigger_flash_buttons(&mut self) {
        let codec = PacketCodec::new(ChecksumStrategy::None);
        match codec.encode_set_mouse_keys(&self.hw_state.buttons) {
            Ok(rpt) => {
                if self.connected_device_label.is_none() {
                    self.record_preview_packet(
                        rpt.report_id,
                        rpt.payload,
                        "PREVIEW TxSetMouseKeys (0x09)",
                    );
                }
                let _ = self
                    .worker_tx
                    .send(WorkerCommand::FlashButtons(self.hw_state.buttons));
            }
            Err(e) => {
                self.status_banner = format!("Safety check rejected: {e}");
            }
        }
    }

    fn trigger_flash_macros(&mut self) {
        let raw_vecs: Vec<Vec<MacroStep>> = self.macros.iter().map(|m| m.steps.clone()).collect();
        let slices: Vec<&[MacroStep]> = raw_vecs.iter().map(Vec::as_slice).collect();
        let mut flash = [0u8; MACRO_FLASH_TOTAL_LEN];
        match compile_macro_bank(&slices, &mut flash) {
            Ok(used) => {
                let codec = PacketCodec::new(ChecksumStrategy::None);
                let first_chunk = codec.encode_write_macro_chunk(0, &flash[..used.min(56)]);
                if self.connected_device_label.is_none() {
                    self.record_preview_packet(
                        first_chunk.report_id,
                        first_chunk.payload,
                        &format!("PREVIEW TxWriteMacroChunk ({used}B total)"),
                    );
                }
                let _ = self
                    .worker_tx
                    .send(WorkerCommand::FlashMacros(self.macros.clone()));
            }
            Err(e) => {
                self.status_banner = format!("Macro compilation failed: {e}");
            }
        }
    }

    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("FIRE PHOENIX BK-R1X")
                    .font(FontId::proportional(17.5))
                    .strong()
                    .color(Color32::from_rgb(122, 162, 247)),
            );

            ui.separator();

            for (tab, label) in [
                (ActiveTab::Performance, "⚡ Performance"),
                (ActiveTab::ButtonsAndMacros, "🖱 Buttons & Macros"),
                (ActiveTab::ProfilesAndStartup, "🗂 Profiles & Startup"),
                (ActiveTab::TelemetryInspector, "🔬 HID Inspector"),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("↻ Sync").clicked() {
                    let _ = self.worker_tx.send(WorkerCommand::ConnectAndSync {
                        force_refresh: true,
                    });
                }

                ui.toggle_value(&mut self.show_raw_debugger_drawer, "⌨ Raw HID");

                let pct = self.hw_state.battery.percent;
                let bat_color = if self.connected_device_label.is_none() {
                    Color32::from_rgb(120, 125, 140)
                } else if pct <= 20 {
                    Color32::from_rgb(247, 118, 142)
                } else {
                    Color32::from_rgb(158, 206, 106)
                };
                let bat_txt = if self.connected_device_label.is_some() {
                    format!(
                        "{}{}%",
                        if self.hw_state.battery.charging {
                            "⚡ "
                        } else {
                            "🔋 "
                        },
                        pct
                    )
                } else {
                    "🔋 --%".to_owned()
                };
                ui.label(RichText::new(bat_txt).strong().color(bat_color));

                let (mode_txt, mode_col) = if let Some(conn) = self.hw_state.connection
                    && self.connected_device_label.is_some()
                {
                    (conn.as_str(), Color32::from_rgb(125, 207, 255))
                } else if matches!(
                    self.last_error,
                    Some(DeviceError::BluetoothConfigUnsupported)
                ) {
                    ("Bluetooth (Read-Only)", Color32::from_rgb(224, 175, 104))
                } else {
                    ("Offline / Preview", Color32::from_rgb(169, 177, 214))
                };
                ui.label(RichText::new(format!("[{mode_txt}]")).color(mode_col));
            });
        });

        if matches!(
            self.last_error,
            Some(DeviceError::BluetoothConfigUnsupported)
        ) {
            ui.add_space(4.0);
            egui::Frame::NONE
                .fill(Color32::from_rgb(58, 42, 24))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(224, 175, 104)))
                .inner_margin(6.0)
                .corner_radius(CornerRadius::same(4))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(
                            "⚠ Bluetooth mode detected: GATT HID omits the vendor 0xFF01 configuration channel. Connect via USB-C cable or 2.4GHz wireless dongle to flash settings.",
                        )
                        .color(Color32::from_rgb(255, 215, 140)),
                    );
                });
        }
    }

    fn draw_performance_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.horizontal(|ui| {
                        ui.heading("PixArt PAW3311 — DPI Stages");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut count = self.hw_state.config.dpi_count;
                            ui.add(
                                egui::DragValue::new(&mut count)
                                    .range(1..=6)
                                    .prefix("Stages: "),
                            );
                            if count != self.hw_state.config.dpi_count {
                                self.hw_state.config.dpi_count = count;
                                if self.hw_state.config.active_dpi_stage >= count {
                                    self.hw_state.config.active_dpi_stage = count - 1;
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);

                    for idx in 0..(self.hw_state.config.dpi_count as usize) {
                        let is_active = self.hw_state.config.active_dpi_stage as usize == idx;
                        egui::Frame::NONE
                            .fill(if is_active {
                                Color32::from_rgb(36, 44, 66)
                            } else {
                                Color32::from_rgb(24, 26, 36)
                            })
                            .stroke(Stroke::new(
                                1.0_f32,
                                if is_active {
                                    Color32::from_rgb(122, 162, 247)
                                } else {
                                    Color32::from_rgb(45, 50, 70)
                                },
                            ))
                            .inner_margin(8.0)
                            .corner_radius(CornerRadius::same(5))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui.radio(is_active, format!("Stage {}", idx + 1)).clicked() {
                                        self.hw_state.config.active_dpi_stage = idx as u8;
                                    }

                                    ui.color_edit_button_srgb(&mut self.stage_colors[idx]);

                                    let mut val = self.hw_state.config.dpi_stages[idx];
                                    if ui
                                        .add(
                                            egui::Slider::new(
                                                &mut val,
                                                PAW3311_DPI_MIN..=PAW3311_DPI_MAX,
                                            )
                                            .step_by(PAW3311_DPI_STEP as f64)
                                            .suffix(" DPI"),
                                        )
                                        .changed()
                                    {
                                        let snapped = (val / PAW3311_DPI_STEP) * PAW3311_DPI_STEP;
                                        self.hw_state.config.dpi_stages[idx] =
                                            snapped.clamp(PAW3311_DPI_MIN, PAW3311_DPI_MAX);
                                    }
                                });
                            });
                        ui.add_space(4.0);
                    }
                });

                cols[1].group(|ui| {
                    ui.heading("Polling Rate & Sensor Registers");
                    ui.add_space(6.0);

                    ui.label(RichText::new("USB / 2.4GHz Report Rate").strong());
                    ui.horizontal(|ui| {
                        for rate in PollingRate::ALL {
                            ui.selectable_value(
                                &mut self.hw_state.config.polling_rate,
                                rate,
                                format!("{} Hz", rate.as_hz()),
                            );
                        }
                    });

                    ui.add_space(10.0);
                    ui.label(RichText::new("Lift-Off Distance (LOD)").strong());
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut self.hw_state.config.lod,
                            LodHeight::Mm1,
                            "1.0 mm (Low / Competitive)",
                        );
                        ui.selectable_value(
                            &mut self.hw_state.config.lod,
                            LodHeight::Mm2,
                            "2.0 mm (High)",
                        );
                    });

                    ui.add_space(10.0);
                    ui.label(RichText::new("Switch Debounce Delay (Key Response)").strong());
                    ui.add(
                        egui::Slider::new(&mut self.hw_state.config.debounce_ms, 0..=30)
                            .suffix(" ms"),
                    );

                    ui.separator();
                    ui.label(RichText::new("PAW3311 DSP & Linearity Flags").strong());
                    ui.checkbox(
                        &mut self.hw_state.config.sensor_flags.motion_sync,
                        "Motion Sync (Synchronize SPI sensor reads to USB SOF)",
                    );
                    ui.checkbox(
                        &mut self.hw_state.config.sensor_flags.angle_snapping,
                        "Angle Snapping / Linear Correction",
                    );
                    ui.checkbox(
                        &mut self.hw_state.config.sensor_flags.ripple_control,
                        "Ripple Control (High-DPI smoothing > 9000 DPI)",
                    );
                    ui.checkbox(
                        &mut self.hw_state.config.highspeed_mode,
                        "High-Speed Corded/RF Performance Mode",
                    );

                    ui.horizontal(|ui| {
                        ui.label("Scroll Wheel Direction:");
                        ui.selectable_value(
                            &mut self.hw_state.config.scroll_direction,
                            ScrollDirection::Normal,
                            "Normal",
                        );
                        ui.selectable_value(
                            &mut self.hw_state.config.scroll_direction,
                            ScrollDirection::Reverse,
                            "Reverse",
                        );
                    });

                    ui.separator();
                    ui.label(RichText::new("Power Management & LED Mode").strong());
                    ui.horizontal(|ui| {
                        ui.label("Deep Sleep Timeout:");
                        egui::ComboBox::from_id_salt("sleep_timeout_combo")
                            .selected_text(format!("{} min", self.hw_state.config.sleep_minutes))
                            .show_ui(ui, |ui| {
                                for m in VALID_SLEEP_MINUTES {
                                    ui.selectable_value(
                                        &mut self.hw_state.config.sleep_minutes,
                                        m,
                                        format!("{m} min"),
                                    );
                                }
                            });

                        ui.label("LED Effect:");
                        egui::ComboBox::from_id_salt("led_mode_combo")
                            .selected_text(self.hw_state.config.light_mode.label())
                            .show_ui(ui, |ui| {
                                for mode in LightMode::ALL {
                                    ui.selectable_value(
                                        &mut self.hw_state.config.light_mode,
                                        mode,
                                        mode.label(),
                                    );
                                }
                            });
                    });

                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.hw_state.config.wakeup_on_move, "Wake on Movement");
                        ui.checkbox(
                            &mut self.hw_state.config.move_lights_off,
                            "Turn Off LED While Moving (Battery Saver)",
                        );
                    });

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                RichText::new("⚡ Apply & Save to Mouse")
                                    .strong()
                                    .color(Color32::from_rgb(158, 206, 106)),
                            )
                            .clicked()
                        {
                            self.trigger_flash_config();
                        }
                        if ui.button("Factory Defaults").clicked() {
                            self.hw_state.config = MouseConfig::factory_default();
                        }
                    });
                });
            });
        });
    }

    fn draw_buttons_and_macros_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.heading("7-Button & Wheel Remapper");
                    ui.add_space(4.0);

                    self.draw_mouse_diagram(ui);
                    ui.add_space(8.0);

                    for slot in ButtonSlot::ALL {
                        let idx = slot as usize;
                        let selected = self.selected_slot == idx;
                        let summary = describe_button_action(self.hw_state.buttons.slots[idx]);
                        if ui
                            .selectable_label(
                                selected,
                                format!("{}. {:<22} → {}", idx + 1, slot.label(), summary),
                            )
                            .clicked()
                        {
                            self.selected_slot = idx;
                        }
                    }

                    ui.separator();
                    let slot_enum = ButtonSlot::ALL[self.selected_slot];
                    ui.label(
                        RichText::new(format!("Assign Action for: {}", slot_enum.label()))
                            .strong()
                            .color(Color32::from_rgb(125, 207, 255)),
                    );

                    ui.horizontal_wrapped(|ui| {
                        for (label, act) in [
                            ("Left Click", ButtonAction::MouseClick(MouseClickButton::Left)),
                            ("Right Click", ButtonAction::MouseClick(MouseClickButton::Right)),
                            (
                                "Middle Click",
                                ButtonAction::MouseClick(MouseClickButton::Middle),
                            ),
                            ("Side Back", ButtonAction::MouseClick(MouseClickButton::Back)),
                            (
                                "Side Forward",
                                ButtonAction::MouseClick(MouseClickButton::Forward),
                            ),
                            ("DPI Loop", ButtonAction::DpiLoop),
                            ("Scroll Up", ButtonAction::ScrollUp),
                            ("Scroll Down", ButtonAction::ScrollDown),
                            ("Disable", ButtonAction::Disabled),
                        ] {
                            if ui.button(label).clicked() {
                                self.assign_slot_with_safety(self.selected_slot, act);
                            }
                        }
                    });

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Multimedia:");
                        egui::ComboBox::from_id_salt("media_assign_combo")
                            .selected_text("Choose Media Key...")
                            .show_ui(ui, |ui| {
                                for m in MediaFunction::ALL {
                                    if ui.selectable_label(false, m.label()).clicked() {
                                        self.assign_slot_with_safety(
                                            self.selected_slot,
                                            ButtonAction::Media(m),
                                        );
                                    }
                                }
                            });
                    });

                    ui.add_space(6.0);
                    ui.label("Keyboard Shortcut (Modifiers + HID Key):");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.edit_sc_ctrl, "Ctrl");
                        ui.checkbox(&mut self.edit_sc_shift, "Shift");
                        ui.checkbox(&mut self.edit_sc_alt, "Alt");
                        ui.checkbox(&mut self.edit_sc_win, "Win");
                        egui::ComboBox::from_id_salt("shortcut_key_combo")
                            .selected_text(keycode_label(self.edit_sc_keycode))
                            .show_ui(ui, |ui| {
                                for entry in HID_KEY_TABLE {
                                    ui.selectable_value(
                                        &mut self.edit_sc_keycode,
                                        entry.code,
                                        entry.label,
                                    );
                                }
                            });
                        if ui.button("Bind Shortcut").clicked() {
                            let mut mask = 0u8;
                            if self.edit_sc_ctrl {
                                mask |= KeyModifiers::LCTRL;
                            }
                            if self.edit_sc_shift {
                                mask |= KeyModifiers::LSHIFT;
                            }
                            if self.edit_sc_alt {
                                mask |= KeyModifiers::LALT;
                            }
                            if self.edit_sc_win {
                                mask |= KeyModifiers::LWIN;
                            }
                            self.assign_slot_with_safety(
                                self.selected_slot,
                                ButtonAction::KeyboardShortcut {
                                    modifiers: KeyModifiers(mask),
                                    keycode: self.edit_sc_keycode,
                                },
                            );
                        }
                    });

                    ui.add_space(6.0);
                    ui.label("Hardware Macro Slot:");
                    ui.horizontal(|ui| {
                        let max_slot = self.macros.len().saturating_sub(1) as u8;
                        ui.add(
                            egui::DragValue::new(&mut self.edit_macro_slot)
                                .range(0..=max_slot)
                                .prefix("Slot #"),
                        );
                        egui::ComboBox::from_id_salt("macro_mode_combo")
                            .selected_text(match self.edit_macro_mode {
                                2 => "Hold to Loop",
                                3 => "Toggle Loop",
                                _ => "Fixed Repeat",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.edit_macro_mode, 0, "Fixed Repeat");
                                ui.selectable_value(&mut self.edit_macro_mode, 2, "Hold to Loop");
                                ui.selectable_value(&mut self.edit_macro_mode, 3, "Toggle Loop");
                            });
                        if self.edit_macro_mode == 0 {
                            ui.add(
                                egui::DragValue::new(&mut self.edit_macro_repeat)
                                    .range(1..=255)
                                    .suffix("x"),
                            );
                        }
                        if ui.button("Bind Macro").clicked() {
                            let trigger = match self.edit_macro_mode {
                                2 => MacroTrigger::HoldToLoop,
                                3 => MacroTrigger::ToggleLoop,
                                _ => MacroTrigger::FixedCount(self.edit_macro_repeat.max(1)),
                            };
                            self.assign_slot_with_safety(
                                self.selected_slot,
                                ButtonAction::Macro {
                                    slot: self.edit_macro_slot,
                                    trigger,
                                },
                            );
                        }
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                RichText::new("⚡ Flash Button Map to Mouse")
                                    .strong()
                                    .color(Color32::from_rgb(158, 206, 106)),
                            )
                            .clicked()
                        {
                            self.trigger_flash_buttons();
                        }
                        if ui.button("Reset All Buttons").clicked() {
                            self.hw_state.buttons = ButtonMapping::factory_default();
                        }
                    });
                });

                cols[1].group(|ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Hardware Macro Sequence Builder");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let used_bytes = self.estimate_macro_flash_bytes();
                            ui.label(
                                RichText::new(format!(
                                    "Flash: {used_bytes} / {MACRO_FLASH_TOTAL_LEN} B"
                                ))
                                .color(Color32::from_rgb(187, 154, 247)),
                            );
                        });
                    });
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("select_macro_slot")
                            .selected_text(
                                self.macros
                                    .get(self.selected_macro_idx)
                                    .map(|m| format!("#{} {}", self.selected_macro_idx, m.name))
                                    .unwrap_or_else(|| "No Macro".to_owned()),
                            )
                            .show_ui(ui, |ui| {
                                for (i, m) in self.macros.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.selected_macro_idx,
                                        i,
                                        format!("#{i} {}", m.name),
                                    );
                                }
                            });

                        if ui.button("+ New Macro").clicked() && self.macros.len() < 32 {
                            let idx = self.macros.len();
                            self.macros.push(UiMacroSlot {
                                name: format!("Macro {}", idx + 1),
                                steps: Vec::new(),
                            });
                            self.selected_macro_idx = idx;
                        }
                    });

                    if let Some(mac) = self.macros.get_mut(self.selected_macro_idx) {
                        ui.horizontal(|ui| {
                            ui.label("Name:");
                            ui.text_edit_singleline(&mut mac.name);
                            if ui.button("Clear Steps").clicked() {
                                mac.steps.clear();
                            }
                        });

                        ui.separator();
                        let mut remove_idx: Option<usize> = None;
                        egui::ScrollArea::vertical()
                            .id_salt("macro_steps_scroll")
                            .max_height(210.0)
                            .show(ui, |ui| {
                                for (idx, step) in mac.steps.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        let kind_lbl = match step.input {
                                            MacroInput::KeyboardKey(kc) => {
                                                format!("Key [{}]", keycode_label(kc))
                                            }
                                            MacroInput::MouseButton(b) => {
                                                format!("Mouse [{}]", b.label())
                                            }
                                        };
                                        let dir_lbl =
                                            if step.pressed { "↓ DOWN" } else { "↑ UP  " };
                                        ui.monospace(format!(
                                            "{:>2}. {dir_lbl} {kind_lbl:<18}",
                                            idx + 1
                                        ));
                                        ui.add(
                                            egui::DragValue::new(&mut step.delay_ms)
                                                .range(0..=10_000)
                                                .suffix(" ms"),
                                        );
                                        if ui.small_button("✕").clicked() {
                                            remove_idx = Some(idx);
                                        }
                                    });
                                }
                            });
                        if let Some(r) = remove_idx {
                            mac.steps.remove(r);
                        }

                        ui.separator();
                        ui.label(RichText::new("Append Press + Release Pair:").strong());
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.new_step_is_mouse, false, "Keyboard");
                            ui.selectable_value(&mut self.new_step_is_mouse, true, "Mouse");

                            if self.new_step_is_mouse {
                                egui::ComboBox::from_id_salt("new_step_mouse_combo")
                                    .selected_text(self.new_step_mouse_btn.label())
                                    .show_ui(ui, |ui| {
                                        for b in MouseClickButton::ALL {
                                            ui.selectable_value(
                                                &mut self.new_step_mouse_btn,
                                                b,
                                                b.label(),
                                            );
                                        }
                                    });
                            } else {
                                egui::ComboBox::from_id_salt("new_step_key_combo")
                                    .selected_text(keycode_label(self.new_step_keycode))
                                    .show_ui(ui, |ui| {
                                        for e in HID_KEY_TABLE {
                                            ui.selectable_value(
                                                &mut self.new_step_keycode,
                                                e.code,
                                                e.label,
                                            );
                                        }
                                    });
                            }

                            ui.add(
                                egui::DragValue::new(&mut self.new_step_delay_ms)
                                    .range(1..=5000)
                                    .suffix(" ms"),
                            );

                            if ui.button("+ Add Tap").clicked() {
                                let input = if self.new_step_is_mouse {
                                    MacroInput::MouseButton(self.new_step_mouse_btn)
                                } else {
                                    MacroInput::KeyboardKey(self.new_step_keycode)
                                };
                                mac.steps.push(MacroStep {
                                    input,
                                    pressed: true,
                                    delay_ms: self.new_step_delay_ms,
                                });
                                mac.steps.push(MacroStep {
                                    input,
                                    pressed: false,
                                    delay_ms: self.new_step_delay_ms,
                                });
                            }
                        });
                    }

                    ui.add_space(10.0);
                    if ui
                        .button(
                            RichText::new("⚡ Flash Macro Bank to MCU")
                                .strong()
                                .color(Color32::from_rgb(158, 206, 106)),
                        )
                        .clicked()
                    {
                        self.trigger_flash_macros();
                    }
                });
            });
        });
    }

    fn draw_profiles_and_startup_panel(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |cols| {
            // LEFT COLUMN: Saved Profiles + G HUB Per-Game Auto-Switching
            cols[0].group(|ui| {
                ui.heading("Hardware Profiles & Game Auto-Switch");
                ui.label(
                    RichText::new(
                        "Save profiles to switch from the System Tray menu or automatically when a game launches.",
                    )
                    .size(12.0)
                    .color(Color32::from_rgb(150, 160, 190)),
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label("Profile Name:");
                    ui.text_edit_singleline(&mut self.new_profile_name);
                });
                ui.horizontal(|ui| {
                    ui.label("Auto-Switch Process (optional, e.g. cs2.exe):");
                    ui.text_edit_singleline(&mut self.new_profile_process);
                });

                if ui
                    .button(
                        RichText::new("💾 Save Current Settings as Profile")
                            .strong()
                            .color(Color32::from_rgb(125, 207, 255)),
                    )
                    .clicked()
                {
                    let mut prof = HardwareProfile::from_hardware(
                        self.new_profile_name.trim(),
                        &self.hw_state.config,
                        &self.hw_state.buttons,
                    );
                    let proc_trimmed = self.new_profile_process.trim();
                    if !proc_trimmed.is_empty() {
                        prof.match_process = Some(proc_trimmed.to_owned());
                    }
                    let slug: String = prof
                        .name
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() {
                                c.to_ascii_lowercase()
                            } else {
                                '-'
                            }
                        })
                        .collect();
                    let dest = ensure_user_profiles_dir().join(format!("{slug}.toml"));
                    match prof.save_to_file(&dest) {
                        Ok(()) => {
                            let _ = send_ipc_request(&IpcRequest::ReloadProfiles);
                            self.saved_profiles = list_saved_profiles();
                            self.status_banner =
                                format!("Saved profile '{}' -> {}", prof.name, dest.display());
                        }
                        Err(e) => {
                            self.status_banner = format!("Failed saving profile: {e}");
                        }
                    }
                }

                ui.separator();
                ui.label(RichText::new("Saved Profiles:").strong());

                let mut apply_prof: Option<HardwareProfile> = None;
                let mut delete_path: Option<PathBuf> = None;

                egui::ScrollArea::vertical()
                    .id_salt("saved_profiles_scroll")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (path, prof) in &self.saved_profiles {
                            egui::Frame::NONE
                                .fill(Color32::from_rgb(26, 29, 41))
                                .inner_margin(8.0)
                                .corner_radius(CornerRadius::same(4))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(RichText::new(&prof.name).strong());
                                            let sub = format!(
                                                "{} Hz · LOD {}mm · Stages {:?}{}",
                                                prof.sensor.polling_rate_hz,
                                                prof.sensor.lod_mm,
                                                prof.sensor.dpi_stages,
                                                prof.match_process
                                                    .as_deref()
                                                    .map(|p| format!(" · Game: {p}"))
                                                    .unwrap_or_default()
                                            );
                                            ui.label(
                                                RichText::new(sub)
                                                    .size(11.5)
                                                    .color(Color32::from_rgb(145, 155, 185)),
                                            );
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui.small_button("🗑").clicked() {
                                                    delete_path = Some(path.clone());
                                                }
                                                if ui.button("Apply").clicked() {
                                                    apply_prof = Some(prof.clone());
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(4.0);
                        }
                    });

                if let Some(prof) = apply_prof {
                    if let Ok(cfg) = prof.to_mouse_config() {
                        self.hw_state.config = cfg;
                    }
                    if let Ok(btns) = prof.to_button_mapping() {
                        self.hw_state.buttons = btns;
                    }
                    self.new_profile_name = prof.name.clone();
                    self.new_profile_process = prof.match_process.clone().unwrap_or_default();
                    let _ = self
                        .worker_tx
                        .send(WorkerCommand::ApplySavedProfile(Box::new(prof)));
                }

                if let Some(del_p) = delete_path {
                    let _ = fs::remove_file(&del_p);
                    let _ = send_ipc_request(&IpcRequest::ReloadProfiles);
                    self.saved_profiles = list_saved_profiles();
                }
            });

            // RIGHT COLUMN: Windows Startup, System Tray & Desktop Installation
            cols[1].group(|ui| {
                ui.heading("Windows Startup & System Tray");
                ui.add_space(6.0);

                egui::Frame::NONE
                    .fill(Color32::from_rgb(24, 34, 30))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(158, 206, 106)))
                    .inner_margin(10.0)
                    .corner_radius(CornerRadius::same(5))
                    .show(ui, |ui| {
                        let tray_txt = if self.tray_agent_running {
                            "● System Tray Agent is ACTIVE in your taskbar notification area (< 2 MB RAM, 0% idle CPU)"
                        } else {
                            "○ System Tray Agent is currently stopped"
                        };
                        ui.label(
                            RichText::new(tray_txt)
                                .strong()
                                .color(Color32::from_rgb(158, 206, 106)),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            "• Left-click the BK-R1X tray icon anytime to open this configurator.\n\
                             • Right-click the tray icon to switch DPI, Polling Rate, or Profiles in 1 click.\n\
                             • Closing this window (X) frees 100% of GUI memory while keeping the tray icon running.",
                        );
                    });

                ui.add_space(12.0);
                let mut auto = self.autostart_enabled;
                if ui
                    .checkbox(
                        &mut auto,
                        "Launch BK-R1X automatically when I turn on my computer (Windows Login)",
                    )
                    .changed()
                {
                    match set_autostart_enabled(auto, None) {
                        Ok(()) => {
                            self.autostart_enabled = auto;
                            self.status_banner = if auto {
                                "Enabled automatic launch at Windows login.".to_owned()
                            } else {
                                "Disabled automatic launch at Windows login.".to_owned()
                            };
                        }
                        Err(e) => {
                            self.status_banner = format!("Startup toggle error: {e}");
                        }
                    }
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if !self.tray_agent_running && ui.button("▶ Start System Tray Agent").clicked() {
                        self.tray_agent_running = ensure_tray_daemon_spawned();
                    }

                    if ui
                        .button("📦 Install / Update Start Menu App")
                        .on_hover_text(
                            "Copies binaries to %LOCALAPPDATA%\\Programs\\BKR1X, creates Start Menu shortcut, and registers Windows Startup",
                        )
                        .clicked()
                    {
                        match install_user_application(true) {
                            Ok(summary) => {
                                self.autostart_enabled = summary.autostart_registered;
                                self.tray_agent_running = ensure_tray_daemon_spawned();
                                self.status_banner = format!(
                                    "Installed to {} & created Start Menu shortcut!",
                                    summary.install_dir.display()
                                );
                            }
                            Err(e) => {
                                self.status_banner = format!("Install error: {e}");
                            }
                        }
                    }

                    if self.tray_agent_running
                        && ui
                            .button(
                                RichText::new("⏹ Quit Tray Agent & Exit")
                                    .color(Color32::from_rgb(247, 118, 142)),
                            )
                            .clicked()
                    {
                        let _ = send_ipc_request(&IpcRequest::Shutdown);
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });
    }

    fn draw_mouse_diagram(&mut self, ui: &mut egui::Ui) {
        let desired = Vec2::new(ui.available_width().min(360.0), 145.0);
        let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 6.0, Color32::from_rgb(18, 20, 28));
        let c = rect.center();

        let body = Rect::from_center_size(c, Vec2::new(92.0, 128.0));
        painter.rect(
            body,
            CornerRadius::same(38),
            Color32::from_rgb(32, 36, 50),
            Stroke::new(1.5_f32, Color32::from_rgb(95, 110, 150)),
            egui::StrokeKind::Inside,
        );

        let hotspots: [(usize, &str, Vec2); 6] = [
            (0, "LMB", Vec2::new(-22.0, -36.0)),
            (1, "RMB", Vec2::new(22.0, -36.0)),
            (2, "MMB", Vec2::new(0.0, -34.0)),
            (5, "DPI", Vec2::new(0.0, -6.0)),
            (4, "FWD", Vec2::new(-46.0, -8.0)),
            (3, "BCK", Vec2::new(-46.0, 18.0)),
        ];

        for (slot_idx, tag, offset) in hotspots {
            let btn_rect = Rect::from_center_size(c + offset, Vec2::new(32.0, 20.0));
            let resp = ui.interact(
                btn_rect,
                egui::Id::new(("mouse_hotspot", slot_idx)),
                Sense::click(),
            );
            if resp.clicked() {
                self.selected_slot = slot_idx;
            }
            let active = self.selected_slot == slot_idx;
            painter.rect(
                btn_rect,
                CornerRadius::same(4),
                if active {
                    Color32::from_rgb(65, 105, 205)
                } else if resp.hovered() {
                    Color32::from_rgb(50, 58, 82)
                } else {
                    Color32::from_rgb(40, 45, 62)
                },
                Stroke::new(1.0_f32, Color32::from_rgb(122, 162, 247)),
                egui::StrokeKind::Inside,
            );
            painter.text(
                Pos2::new(btn_rect.center().x, btn_rect.center().y),
                egui::Align2::CENTER_CENTER,
                tag,
                FontId::monospace(10.0),
                Color32::WHITE,
            );
        }
    }

    fn assign_slot_with_safety(&mut self, slot: usize, action: ButtonAction) {
        if slot >= BUTTON_SLOT_COUNT {
            return;
        }
        let mut candidate = self.hw_state.buttons;
        candidate.slots[slot] = action;
        match candidate.validate() {
            Ok(()) => {
                self.hw_state.buttons = candidate;
                self.status_banner = format!(
                    "Updated {} -> {} (click Flash Button Map to commit).",
                    ButtonSlot::ALL[slot].label(),
                    describe_button_action(action)
                );
            }
            Err(e) => {
                self.status_banner = format!("Safety guard: {e}");
            }
        }
    }

    fn estimate_macro_flash_bytes(&self) -> usize {
        let raw_vecs: Vec<Vec<MacroStep>> = self.macros.iter().map(|m| m.steps.clone()).collect();
        let slices: Vec<&[MacroStep]> = raw_vecs.iter().map(Vec::as_slice).collect();
        let mut scratch = [0u8; MACRO_FLASH_TOTAL_LEN];
        compile_macro_bank(&slices, &mut scratch).unwrap_or(MACRO_FLASH_TOTAL_LEN)
    }

    fn draw_telemetry_and_inspector_panel(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |cols| {
            cols[0].group(|ui| {
                ui.heading("Hardware Telemetry");
                ui.add_space(6.0);
                let dev_str = self
                    .connected_device_label
                    .as_deref()
                    .unwrap_or("No Vendor 0xFF01 Device Opened (Preview Mode)");
                ui.label(format!("HID Endpoint : {dev_str}"));
                ui.label(format!(
                    "Transport    : {}",
                    if self.connected_via_daemon {
                        "Shared IPC via System Tray Agent (Zero HID Contention)"
                    } else {
                        "Direct HIDraw / hid.dll"
                    }
                ));
                ui.label(format!(
                    "Topology     : {}",
                    self.hw_state
                        .connection
                        .map(|m| m.as_str())
                        .unwrap_or("Offline")
                ));
                ui.label(format!(
                    "MCU State    : {}",
                    if self.hw_state.online {
                        "Awake / Active"
                    } else {
                        "Deep Sleep / Offline"
                    }
                ));
                ui.label(format!("Firmware     : v{}", self.hw_state.firmware));
                ui.add_space(6.0);
                let frac = f32::from(self.hw_state.battery.percent.min(100)) / 100.0;
                ui.add(
                    egui::ProgressBar::new(frac).text(format!(
                        "500mAh LiPo Battery: {}% {}",
                        self.hw_state.battery.percent,
                        if self.hw_state.battery.charging {
                            "(Charging via USB-C)"
                        } else {
                            ""
                        }
                    )),
                );
            });

            cols[1].group(|ui| {
                ui.heading("Interactive Hex Packet Verifier");
                ui.label("Paste any 64B/65B WebHID or USBPcap hex frame to decode:");
                ui.text_edit_singleline(&mut self.custom_hex_input);
                if ui.button("Decode & Verify Frame").clicked() {
                    self.custom_hex_decoded = verify_hex_string(&self.custom_hex_input);
                }
                if !self.custom_hex_decoded.is_empty() {
                    ui.monospace(&self.custom_hex_decoded);
                }
            });
        });

        ui.add_space(8.0);
        self.draw_raw_debugger_table(ui, 320.0);
    }

    fn draw_raw_debugger_table(&mut self, ui: &mut egui::Ui, max_h: f32) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Raw HID Wire Debugger (64-Byte Vendor Reports)")
                        .strong()
                        .color(Color32::from_rgb(125, 207, 255)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Clear Log").clicked() {
                        self.trace_entries.clear();
                    }
                    ui.label(format!("{} frame(s)", self.trace_entries.len()));
                });
            });

            egui::ScrollArea::vertical()
                .id_salt("raw_hid_trace_scroll")
                .max_height(max_h)
                .show(ui, |ui| {
                    if self.trace_entries.is_empty() {
                        ui.label(
                            RichText::new(
                                "No HID frames captured yet. Click Apply or Sync to inspect exact wire bytes.",
                            )
                            .italics(),
                        );
                    }
                    for entry in self.trace_entries.iter().rev() {
                        let (badge, col) = match entry.direction {
                            TraceDirection::Out => ("OUT", Color32::from_rgb(255, 158, 100)),
                            TraceDirection::In => ("IN ", Color32::from_rgb(125, 207, 255)),
                        };
                        let hex: String = entry
                            .payload
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("[{badge}]")).monospace().color(col));
                            ui.label(
                                RichText::new(format!("rpt=0x{:02X}", entry.report_id)).monospace(),
                            );
                            ui.label(
                                RichText::new(&entry.summary)
                                    .monospace()
                                    .color(Color32::from_rgb(158, 206, 106)),
                            );
                            if ui.small_button("Copy Hex").clicked() {
                                ui.ctx().copy_text(hex.clone());
                            }
                        });
                        ui.monospace(hex);
                        ui.separator();
                    }
                });
        });
    }
}

impl eframe::App for Bkr1xGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_worker_events();

        egui::TopBottomPanel::top("bkr1x_top_bar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(8.0))
            .show(ctx, |ui| {
                self.draw_top_bar(ui);
            });

        egui::TopBottomPanel::bottom("bkr1x_status_footer")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(6.0))
            .show(ctx, |ui| {
                if self.show_raw_debugger_drawer && self.tab != ActiveTab::TelemetryInspector {
                    self.draw_raw_debugger_table(ui, 150.0);
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&self.status_banner)
                            .size(12.0)
                            .color(Color32::from_rgb(169, 177, 214)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Tray Background < 2 MB · Reactive OpenGL 0% Idle CPU")
                                .size(11.0)
                                .color(Color32::from_rgb(100, 108, 135)),
                        );
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(Color32::from_rgb(16, 18, 25))
                    .inner_margin(10.0),
            )
            .show(ctx, |ui| match self.tab {
                ActiveTab::Performance => self.draw_performance_panel(ui),
                ActiveTab::ButtonsAndMacros => self.draw_buttons_and_macros_panel(ui),
                ActiveTab::ProfilesAndStartup => self.draw_profiles_and_startup_panel(ui),
                ActiveTab::TelemetryInspector => self.draw_telemetry_and_inspector_panel(ui),
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.worker_tx.send(WorkerCommand::Shutdown);
        if let Some(handle) = self.worker_join.take() {
            let _ = handle.join();
        }
    }
}

fn configure_dark_technical_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(20, 22, 31);
    visuals.window_fill = Color32::from_rgb(20, 22, 31);
    visuals.extreme_bg_color = Color32::from_rgb(13, 14, 20);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(26, 29, 41);
    visuals.selection.bg_fill = Color32::from_rgb(56, 92, 178);
    ctx.set_visuals(visuals);
}

fn describe_button_action(action: ButtonAction) -> String {
    match action {
        ButtonAction::MouseClick(b) => b.label().to_owned(),
        ButtonAction::Disabled => "Disabled".to_owned(),
        ButtonAction::ScrollUp => "Scroll Up".to_owned(),
        ButtonAction::ScrollDown => "Scroll Down".to_owned(),
        ButtonAction::DpiLoop => "DPI Stage Loop".to_owned(),
        ButtonAction::Media(m) => format!("Media: {}", m.label()),
        ButtonAction::KeyboardShortcut { modifiers, keycode } => {
            let mut parts = Vec::new();
            if modifiers.has_ctrl() {
                parts.push("Ctrl");
            }
            if modifiers.has_shift() {
                parts.push("Shift");
            }
            if modifiers.has_alt() {
                parts.push("Alt");
            }
            if modifiers.has_win() {
                parts.push("Win");
            }
            parts.push(keycode_label(keycode));
            parts.join("+")
        }
        ButtonAction::Macro { slot, trigger } => match trigger {
            MacroTrigger::FixedCount(n) => format!("Macro #{slot} ({n}x)"),
            MacroTrigger::HoldToLoop => format!("Macro #{slot} (Hold Loop)"),
            MacroTrigger::ToggleLoop => format!("Macro #{slot} (Toggle Loop)"),
        },
        ButtonAction::Raw(b) => format!("Raw {:02X?}", b),
    }
}

fn verify_hex_string(raw: &str) -> String {
    let hex_digits: String = raw
        .replace("0x", " ")
        .replace("0X", " ")
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex_digits.is_empty() || !hex_digits.len().is_multiple_of(2) {
        return "Invalid hex input (must contain an even number of hex digits).".to_owned();
    }
    let mut bytes = Vec::with_capacity(hex_digits.len() / 2);
    let mut i = 0;
    while i < hex_digits.len() {
        if let Ok(b) = u8::from_str_radix(&hex_digits[i..i + 2], 16) {
            bytes.push(b);
        }
        i += 2;
    }
    let codec = PacketCodec::new(ChecksumStrategy::None);
    match codec.decode(&bytes) {
        Ok(pkt) => format!("{pkt:#?}"),
        Err(e) => format!("Decode Error: {e}"),
    }
}

#[cfg(windows)]
fn focus_existing_gui_if_open() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    let wide: Vec<u16> = GUI_WINDOW_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
        if !hwnd.is_null() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            return true;
        }
    }
    false
}

fn main() -> eframe::Result {
    #[cfg(windows)]
    if focus_existing_gui_if_open() {
        return Ok(());
    }

    let icon_rgba = render_mouse_icon_rgba(64, TrayIconStatus::default());
    let app_icon = egui::IconData {
        rgba: icon_rgba,
        width: 64,
        height: 64,
    };

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        vsync: true,
        viewport: egui::ViewportBuilder::default()
            .with_title(GUI_WINDOW_TITLE)
            .with_icon(app_icon)
            .with_inner_size([1060.0, 690.0])
            .with_min_inner_size([880.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "bkr1x-gui",
        native_options,
        Box::new(|cc| Ok(Box::new(Bkr1xGuiApp::new(cc)))),
    )
}
