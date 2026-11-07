#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! `bkr1x-gui` — Reactive, zero-idle-CPU native configurator (`eframe` + `glow` OpenGL)
//! for the Fire Phoenix BK-R1X tri-mode wireless gaming mouse.

mod keycodes;

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use bkr1x_hal::{Bkr1xDevice, DeviceError, TraceDirection, WireTraceEntry};
use bkr1x_protocol::{
    BUTTON_SLOT_COUNT, ButtonAction, ButtonMapping, ButtonSlot, ChecksumStrategy,
    DEFAULT_STAGE_COLORS, HardwareStateMachine, KeyModifiers, LightMode, LodHeight,
    MACRO_FLASH_TOTAL_LEN, MAX_DPI_STAGES, MacroInput, MacroStep, MacroTrigger, MediaFunction,
    MouseClickButton, MouseConfig, PAW3311_DPI_MAX, PAW3311_DPI_MIN, PAW3311_DPI_STEP,
    PacketCodec, PollingRate, REPORT_PAYLOAD_LEN, ScrollDirection, VALID_SLEEP_MINUTES,
    compile_macro_bank,
};
use eframe::egui::{self, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::keycodes::{HID_KEY_TABLE, keycode_label};

const MAX_UI_TRACE_LOG: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Performance,
    ButtonsAndMacros,
    TelemetryInspector,
}

#[derive(Debug, Clone)]
struct UiMacroSlot {
    name: String,
    steps: Vec<MacroStep>,
}

enum WorkerCommand {
    ConnectAndSync,
    FlashConfig(MouseConfig),
    FlashButtons(ButtonMapping),
    FlashMacros(Vec<Vec<MacroStep>>),
    Shutdown,
}

enum WorkerEvent {
    Connected {
        state: Box<HardwareStateMachine>,
        product_name: String,
        vid: u16,
        pid: u16,
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

fn spawn_hardware_worker(
    egui_ctx: egui::Context,
) -> (Sender<WorkerCommand>, Receiver<WorkerEvent>, JoinHandle<()>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
    let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();

    let handle = thread::Builder::new()
        .name("bkr1x-gui-hid".to_owned())
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut dev_opt: Option<Bkr1xDevice> = None;

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    WorkerCommand::Shutdown => break,
                    WorkerCommand::ConnectAndSync => {
                        match Bkr1xDevice::open_auto() {
                            Ok(mut dev) => {
                                let _ = dev.sync_all();
                                let info = dev.info().clone();
                                let state = Box::new(*dev.state());
                                let traces: Vec<_> = dev.trace_log().iter().cloned().collect();
                                dev.clear_trace_log();
                                dev_opt = Some(dev);
                                let _ = evt_tx.send(WorkerEvent::Connected {
                                    state,
                                    product_name: info.product_name,
                                    vid: info.vendor_id,
                                    pid: info.product_id,
                                    traces,
                                });
                            }
                            Err(error) => {
                                dev_opt = None;
                                let _ = evt_tx.send(WorkerEvent::ConnectionFailed { error });
                            }
                        }
                        // Wake the sleeping reactive GUI event loop!
                        egui_ctx.request_repaint();
                    }
                    WorkerCommand::FlashConfig(cfg) => {
                        if dev_opt.is_none() {
                            dev_opt = Bkr1xDevice::open_auto().ok();
                        }
                        match dev_opt.as_mut() {
                            Some(dev) => match dev.write_config(&cfg).and_then(|()| {
                                dev.write_light_mode(cfg.light_mode)
                            }) {
                                Ok(()) => {
                                    let traces: Vec<_> = dev.trace_log().iter().cloned().collect();
                                    dev.clear_trace_log();
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: format!(
                                            "Flashed PAW3311: {} DPI @ {} Hz (LOD {}mm, Debounce {}ms)",
                                            cfg.active_dpi(),
                                            cfg.polling_rate.as_hz(),
                                            cfg.lod.as_mm(),
                                            cfg.debounce_ms
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
                        if dev_opt.is_none() {
                            dev_opt = Bkr1xDevice::open_auto().ok();
                        }
                        match dev_opt.as_mut() {
                            Some(dev) => match dev.write_buttons(&buttons) {
                                Ok(()) => {
                                    let traces: Vec<_> = dev.trace_log().iter().cloned().collect();
                                    dev.clear_trace_log();
                                    let _ = evt_tx.send(WorkerEvent::FlashSuccess {
                                        message: "Flashed 8-slot button mapping table to MCU."
                                            .to_owned(),
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
                    WorkerCommand::FlashMacros(macros) => {
                        if dev_opt.is_none() {
                            dev_opt = Bkr1xDevice::open_auto().ok();
                        }
                        match dev_opt.as_mut() {
                            Some(dev) => {
                                let slices: Vec<&[MacroStep]> =
                                    macros.iter().map(Vec::as_slice).collect();
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
                }
            }
        })
        .expect("failed spawning HID worker thread");

    (cmd_tx, evt_rx, handle)
}

struct Bkr1xGuiApp {
    tab: ActiveTab,
    hw_state: HardwareStateMachine,
    connected_device_label: Option<String>,
    last_error: Option<DeviceError>,
    status_banner: String,

    // Editable performance & stage colors
    stage_colors: [[u8; 3]; MAX_DPI_STAGES],

    // Editable button & macro builder state
    selected_slot: usize,
    edit_sc_ctrl: bool,
    edit_sc_shift: bool,
    edit_sc_alt: bool,
    edit_sc_win: bool,
    edit_sc_keycode: u8,
    edit_macro_slot: u8,
    edit_macro_repeat: u8,
    edit_macro_mode: u8, // 0=Fixed, 2=Hold, 3=Toggle

    macros: Vec<UiMacroSlot>,
    selected_macro_idx: usize,
    new_step_is_mouse: bool,
    new_step_keycode: u8,
    new_step_mouse_btn: MouseClickButton,
    new_step_delay_ms: u16,

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
        let (worker_tx, worker_rx, join) = spawn_hardware_worker(cc.egui_ctx.clone());
        let _ = worker_tx.send(WorkerCommand::ConnectAndSync);

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
            last_error: None,
            status_banner: "Probing USB-C (0xA8A4:0x2255) & 2.4GHz (0xA8A5:0x2255)...".to_owned(),
            stage_colors: DEFAULT_STAGE_COLORS,
            selected_slot: 0,
            edit_sc_ctrl: true,
            edit_sc_shift: false,
            edit_sc_alt: false,
            edit_sc_win: false,
            edit_sc_keycode: 0x06, // 'C'
            edit_macro_slot: 0,
            edit_macro_repeat: 1,
            edit_macro_mode: 2,
            macros: vec![default_macro],
            selected_macro_idx: 0,
            new_step_is_mouse: false,
            new_step_keycode: 0x04,
            new_step_mouse_btn: MouseClickButton::Left,
            new_step_delay_ms: 15,
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
                    traces,
                } => {
                    self.hw_state = *state;
                    self.connected_device_label =
                        Some(format!("{product_name} (0x{vid:04X}:0x{pid:04X})"));
                    self.last_error = None;
                    self.status_banner = format!(
                        "Connected via {} — Firmware v{}",
                        self.hw_state
                            .connection
                            .map(|m| m.as_str())
                            .unwrap_or("USB"),
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

    fn record_preview_packet(&mut self, report_id: u8, payload: [u8; REPORT_PAYLOAD_LEN], note: &str) {
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
                let first_chunk =
                    codec.encode_write_macro_chunk(0, &flash[..used.min(56)]);
                if self.connected_device_label.is_none() {
                    self.record_preview_packet(
                        first_chunk.report_id,
                        first_chunk.payload,
                        &format!("PREVIEW TxWriteMacroChunk ({used}B total)"),
                    );
                }
                let _ = self.worker_tx.send(WorkerCommand::FlashMacros(raw_vecs));
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
                    .font(FontId::proportional(18.0))
                    .strong()
                    .color(Color32::from_rgb(122, 162, 247)),
            );
            ui.label(
                RichText::new("PAW3311 Native Driver")
                    .size(12.5)
                    .color(Color32::from_rgb(140, 148, 175)),
            );

            ui.separator();

            for (tab, label) in [
                (ActiveTab::Performance, "⚡ Performance (PAW3311)"),
                (ActiveTab::ButtonsAndMacros, "🖱 Buttons & Macros"),
                (ActiveTab::TelemetryInspector, "🔬 Telemetry & HID Inspector"),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("↻ Rescan HID").clicked() {
                    let _ = self.worker_tx.send(WorkerCommand::ConnectAndSync);
                }

                ui.toggle_value(&mut self.show_raw_debugger_drawer, "⌨ Raw HID Drawer");

                // Battery badge
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

                // Connection mode badge
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
                // LEFT COLUMN: 6-Stage PAW3311 DPI Matrix
                cols[0].group(|ui| {
                    ui.horizontal(|ui| {
                        ui.heading("PixArt PAW3311 — DPI Stages");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut count = self.hw_state.config.dpi_count;
                            ui.add(egui::DragValue::new(&mut count).range(1..=6).prefix("Stages: "));
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
                                    if ui
                                        .radio(is_active, format!("Stage {}", idx + 1))
                                        .clicked()
                                    {
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
                                        let snapped =
                                            (val / PAW3311_DPI_STEP) * PAW3311_DPI_STEP;
                                        self.hw_state.config.dpi_stages[idx] =
                                            snapped.clamp(PAW3311_DPI_MIN, PAW3311_DPI_MAX);
                                    }
                                });
                            });
                        ui.add_space(4.0);
                    }
                });

                // RIGHT COLUMN: Polling Rate, LOD, Debounce, Linearity & Power
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
                                RichText::new("⚡ Flash Performance to BK-R1X")
                                    .strong()
                                    .color(Color32::from_rgb(158, 206, 106)),
                            )
                            .clicked()
                        {
                            self.trigger_flash_config();
                        }
                        if ui.button("Reset Defaults").clicked() {
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
                // LEFT COLUMN: 7-Button + Wheel Visual Diagram & Action Remapper
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
                            ("Middle Click", ButtonAction::MouseClick(MouseClickButton::Middle)),
                            ("Side Back", ButtonAction::MouseClick(MouseClickButton::Back)),
                            ("Side Forward", ButtonAction::MouseClick(MouseClickButton::Forward)),
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

                // RIGHT COLUMN: Hardware Macro Sequence Builder
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
                                        let dir_lbl = if step.pressed { "↓ DOWN" } else { "↑ UP  " };
                                        ui.monospace(format!("{:>2}. {dir_lbl} {kind_lbl:<18}", idx + 1));
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

    fn draw_mouse_diagram(&mut self, ui: &mut egui::Ui) {
        let desired = Vec2::new(ui.available_width().min(360.0), 145.0);
        let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
        let painter = ui.painter_at(rect);

        // Background card
        painter.rect_filled(rect, 6.0, Color32::from_rgb(18, 20, 28));
        let c = rect.center();

        // Stylized top-down BK-R1X mouse silhouette
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
                                "No HID frames captured yet. Click any Flash button or Rescan HID to inspect exact wire bytes.",
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
        // Reactive rendering: Drain queued hardware worker events; NEVER call request_repaint()
        // unconditionally, ensuring 0.0% idle CPU when the window is untouched!
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
                            RichText::new("Reactive OpenGL (glow) · 0% Idle CPU")
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
                ActiveTab::TelemetryInspector => self.draw_telemetry_and_inspector_panel(ui),
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Cleanly terminate worker thread & release HID handle so 100% of resources are freed
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

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        vsync: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("Fire Phoenix BK-R1X — Native PAW3311 Configurator")
            .with_inner_size([1020.0, 680.0])
            .with_min_inner_size([840.0, 540.0]),
        ..Default::default()
    };

    eframe::run_native(
        "bkr1x-gui",
        native_options,
        Box::new(|cc| Ok(Box::new(Bkr1xGuiApp::new(cc)))),
    )
}
