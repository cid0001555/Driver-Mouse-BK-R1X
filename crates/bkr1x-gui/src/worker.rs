use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use bkr1x_hal::profile::{MacroProfileEntry, MacroStepSpec, parse_light_mode};
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, DeviceError, HardwareProfile, IpcRequest, IpcResponse,
    IpcWireTrace, TraceDirection, WireTraceEntry, encode_hex_spaced, send_ipc_request,
};
use bkr1x_protocol::{
    BatteryTelemetry, ButtonAction, ButtonMapping, ChecksumStrategy, ConnectionMode,
    FirmwareVersion, HardwareStateMachine, LightMode, LodHeight, MAX_DPI_STAGES, MacroInput,
    MacroStep, MouseConfig, PollingRate, REPORT_PAYLOAD_LEN, ScrollDirection, SensorFlags,
};
use eframe::egui;

#[derive(Debug, Clone)]
pub struct UiMacroSlot {
    pub name: String,
    pub steps: Vec<MacroStep>,
}

/// Pre-formatted wire trace row so rendering the Raw HID Debugger performs zero per-frame allocations.
#[derive(Debug, Clone)]
pub struct UiTraceRow {
    pub direction: TraceDirection,
    pub report_id: u8,
    pub payload_hex: String,
    pub summary: String,
}

impl UiTraceRow {
    pub fn from_ipc(trace: IpcWireTrace) -> Self {
        Self {
            direction: if trace.is_out {
                TraceDirection::Out
            } else {
                TraceDirection::In
            },
            report_id: trace.report_id,
            payload_hex: trace.payload_hex,
            summary: trace.summary,
        }
    }

    pub fn from_hal(entry: &WireTraceEntry) -> Self {
        Self {
            direction: entry.direction,
            report_id: entry.report_id,
            payload_hex: encode_hex_spaced(&entry.payload),
            summary: entry.format_summary(ChecksumStrategy::None),
        }
    }

    pub fn preview(report_id: u8, payload: &[u8; REPORT_PAYLOAD_LEN], summary: String) -> Self {
        Self {
            direction: TraceDirection::Out,
            report_id,
            payload_hex: encode_hex_spaced(payload),
            summary,
        }
    }
}

pub enum WorkerCommand {
    ConnectAndSync { force_refresh: bool },
    FlashConfig(MouseConfig),
    FlashButtons(ButtonMapping),
    FlashMacros(Vec<UiMacroSlot>),
    ApplySavedProfile(Box<HardwareProfile>),
    Shutdown,
}

pub enum WorkerEvent {
    Connected {
        state: Box<HardwareStateMachine>,
        product_name: String,
        vid: u16,
        pid: u16,
        via_daemon: bool,
        traces: Vec<UiTraceRow>,
    },
    FlashSuccess {
        message: String,
        traces: Vec<UiTraceRow>,
    },
    ConnectionFailed {
        error: DeviceError,
    },
}

fn ipc_traces_to_ui(ipc_traces: Option<Vec<IpcWireTrace>>) -> Vec<UiTraceRow> {
    ipc_traces
        .unwrap_or_default()
        .into_iter()
        .map(UiTraceRow::from_ipc)
        .collect()
}

fn drain_device_traces(dev: &mut Bkr1xDevice) -> Vec<UiTraceRow> {
    let rows = dev.trace_log().iter().map(UiTraceRow::from_hal).collect();
    dev.clear_trace_log();
    rows
}

fn snapshot_to_hw_state(snap: &DaemonStatusSnapshot) -> HardwareStateMachine {
    let mut dpi_stages = MouseConfig::factory_default().dpi_stages;
    for (idx, &dpi) in snap.dpi_stages.iter().take(MAX_DPI_STAGES).enumerate() {
        dpi_stages[idx] = dpi;
    }
    let max_stages = MAX_DPI_STAGES as u8;
    let dpi_count = u8::try_from(snap.dpi_stages.len())
        .unwrap_or(max_stages)
        .clamp(1, max_stages);
    let active_dpi_stage = snap.active_dpi_stage.saturating_sub(1).min(dpi_count - 1);

    let mut fw_parts = snap
        .firmware_version
        .split('.')
        .map(|part| part.parse::<u8>().unwrap_or(0));
    let firmware = FirmwareVersion {
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
        firmware,
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

fn execute_ipc_or_direct<F>(
    direct_dev: &mut Option<Bkr1xDevice>,
    ipc_request: IpcRequest,
    default_ipc_msg: impl FnOnce() -> String,
    direct_op: F,
) -> WorkerEvent
where
    F: FnOnce(&mut Bkr1xDevice) -> Result<String, DeviceError>,
{
    if let Ok(resp) = send_ipc_request(&ipc_request) {
        *direct_dev = None;
        return match resp {
            IpcResponse::Ok {
                traces, message, ..
            } => WorkerEvent::FlashSuccess {
                message: message.unwrap_or_else(default_ipc_msg),
                traces: ipc_traces_to_ui(traces),
            },
            IpcResponse::Error { message } => WorkerEvent::ConnectionFailed {
                error: DeviceError::Ipc(message),
            },
        };
    }

    if direct_dev.is_none() {
        match Bkr1xDevice::open_auto() {
            Ok(dev) => *direct_dev = Some(dev),
            Err(error) => return WorkerEvent::ConnectionFailed { error },
        }
    }

    let Some(dev) = direct_dev.as_mut() else {
        return WorkerEvent::ConnectionFailed {
            error: DeviceError::NotFound,
        };
    };

    match direct_op(dev) {
        Ok(message) => WorkerEvent::FlashSuccess {
            message,
            traces: drain_device_traces(dev),
        },
        Err(error) => WorkerEvent::ConnectionFailed { error },
    }
}

fn apply_profile_direct(
    dev: &mut Bkr1xDevice,
    prof: &HardwareProfile,
) -> Result<String, DeviceError> {
    let cfg = prof.to_mouse_config()?;
    dev.write_config(&cfg)?;
    dev.write_light_mode(cfg.light_mode)?;

    if prof.buttons.is_some() {
        let buttons = prof.to_button_mapping()?;
        dev.write_buttons(&buttons)?;
    }

    if !prof.macros.is_empty() {
        let macro_vecs = prof.to_macro_vecs()?;
        let slices: Vec<&[MacroStep]> = macro_vecs.iter().map(Vec::as_slice).collect();
        dev.write_macros(&slices)?;
    }

    Ok(format!(
        "Applied profile '{}' ({} DPI @ {} Hz)",
        prof.name,
        cfg.active_dpi(),
        cfg.polling_rate.as_hz()
    ))
}

fn handle_connect_and_sync(
    direct_dev: &mut Option<Bkr1xDevice>,
    force_refresh: bool,
) -> WorkerEvent {
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
        // Release any direct HID handle so `bkr1x-daemon` retains exclusive ownership.
        *direct_dev = None;
        if snap.connected {
            let state = Box::new(snapshot_to_hw_state(&snap));
            let product_name = if snap.product_name.is_empty() {
                "BK-R1X".to_owned()
            } else {
                snap.product_name
            };
            return WorkerEvent::Connected {
                state,
                product_name,
                vid: snap.vendor_id,
                pid: snap.product_id,
                via_daemon: true,
                traces: ipc_traces_to_ui(traces),
            };
        }
    }

    match Bkr1xDevice::open_auto() {
        Ok(mut dev) => {
            let _ = dev.sync_all();
            let info = dev.info().clone();
            let state = Box::new(*dev.state());
            let traces = drain_device_traces(&mut dev);
            *direct_dev = Some(dev);
            WorkerEvent::Connected {
                state,
                product_name: info.product_name,
                vid: info.vendor_id,
                pid: info.product_id,
                via_daemon: false,
                traces,
            }
        }
        Err(error) => {
            *direct_dev = None;
            WorkerEvent::ConnectionFailed { error }
        }
    }
}

pub fn spawn_hardware_worker(
    egui_ctx: egui::Context,
) -> (
    Sender<WorkerCommand>,
    Receiver<WorkerEvent>,
    Option<JoinHandle<()>>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
    let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();
    let fallback_evt_tx = evt_tx.clone();

    let handle = thread::Builder::new()
        .name("bkr1x-gui-hid".to_owned())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut direct_dev: Option<Bkr1xDevice> = None;

            while let Ok(cmd) = cmd_rx.recv() {
                let event = match cmd {
                    WorkerCommand::Shutdown => break,
                    WorkerCommand::ConnectAndSync { force_refresh } => {
                        handle_connect_and_sync(&mut direct_dev, force_refresh)
                    }
                    WorkerCommand::FlashConfig(cfg) => {
                        let mut prof = HardwareProfile::from_hardware(
                            "Active Config",
                            &cfg,
                            &ButtonMapping::factory_default(),
                        );
                        prof.buttons = None;

                        execute_ipc_or_direct(
                            &mut direct_dev,
                            IpcRequest::ApplyProfile {
                                profile: Box::new(prof),
                            },
                            || {
                                format!(
                                    "Flashed PAW3311 via Tray Agent: {} DPI @ {} Hz (LOD {}mm, Debounce {}ms)",
                                    cfg.active_dpi(),
                                    cfg.polling_rate.as_hz(),
                                    cfg.lod.as_mm(),
                                    cfg.debounce_ms
                                )
                            },
                            |dev| {
                                dev.write_config(&cfg)?;
                                dev.write_light_mode(cfg.light_mode)?;
                                Ok(format!(
                                    "Flashed PAW3311: {} DPI @ {} Hz",
                                    cfg.active_dpi(),
                                    cfg.polling_rate.as_hz()
                                ))
                            },
                        )
                    }
                    WorkerCommand::FlashButtons(buttons) => {
                        let raw_slots: [[u8; 4]; 8] =
                            core::array::from_fn(|i| buttons.slots[i].to_bytes());
                        execute_ipc_or_direct(
                            &mut direct_dev,
                            IpcRequest::FlashButtons { raw_slots },
                            || "Flashed button map via Tray Agent.".to_owned(),
                            |dev| {
                                dev.write_buttons(&buttons)?;
                                Ok("Flashed 8-slot button map to MCU.".to_owned())
                            },
                        )
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

                        execute_ipc_or_direct(
                            &mut direct_dev,
                            IpcRequest::FlashMacros { macros: entries },
                            || "Flashed macro bank via Tray Agent.".to_owned(),
                            move |dev| {
                                let slices: Vec<&[MacroStep]> =
                                    ui_macros.iter().map(|m| m.steps.as_slice()).collect();
                                let bytes = dev.write_macros(&slices)?;
                                Ok(format!("Flashed {bytes} bytes of hardware macros to MCU."))
                            },
                        )
                    }
                    WorkerCommand::ApplySavedProfile(prof) => {
                        let prof_name = prof.name.clone();
                        let direct_copy = prof.clone();
                        execute_ipc_or_direct(
                            &mut direct_dev,
                            IpcRequest::ApplyProfile { profile: prof },
                            move || format!("Applied profile '{prof_name}'"),
                            move |dev| apply_profile_direct(dev, &direct_copy),
                        )
                    }
                };

                let _ = evt_tx.send(event);
                egui_ctx.request_repaint();
            }
        })
        .ok();

    if handle.is_none() {
        let _ = fallback_evt_tx.send(WorkerEvent::ConnectionFailed {
            error: DeviceError::Ipc("failed spawning background HID worker thread".to_owned()),
        });
    }

    (cmd_tx, evt_rx, handle)
}
