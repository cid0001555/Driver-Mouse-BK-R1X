#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod keycodes;
mod panels;
mod worker;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use bkr1x_hal::{
    DeviceError, GUI_WINDOW_TITLE, HardwareProfile, TrayIconStatus, is_autostart_enabled,
    is_daemon_running, list_saved_profiles, render_mouse_icon_rgba, resolve_sibling_binary,
};
use bkr1x_protocol::{
    DEFAULT_STAGE_COLORS, HardwareStateMachine, MAX_DPI_STAGES, MacroInput, MacroStep,
    MouseClickButton,
};
use eframe::egui::{self, Color32, RichText};

use crate::panels::MacroLoopKind;
use crate::worker::{UiMacroSlot, UiTraceRow, WorkerCommand, WorkerEvent, spawn_hardware_worker};

pub(crate) const MAX_UI_TRACE_LOG: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveTab {
    Performance,
    ButtonsAndMacros,
    ProfilesAndStartup,
    TelemetryInspector,
}

/// Ensures `bkr1x-daemon` is active so closing the OpenGL window leaves the System Tray running.
pub(crate) fn ensure_tray_daemon_spawned() -> bool {
    if is_daemon_running() {
        return true;
    }
    let Some(daemon_exe) = resolve_sibling_binary("bkr1x-daemon") else {
        return false;
    };
    let mut cmd = std::process::Command::new(&daemon_exe);
    cmd.arg("--silent");
    if let Some(parent) = daemon_exe.parent() {
        cmd.current_dir(parent);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().is_ok()
}

pub(crate) struct Bkr1xGuiApp {
    pub(crate) tab: ActiveTab,
    pub(crate) hw_state: HardwareStateMachine,
    pub(crate) connected_device_label: Option<String>,
    pub(crate) connected_via_daemon: bool,
    pub(crate) tray_agent_running: bool,
    pub(crate) autostart_enabled: bool,
    pub(crate) last_error: Option<DeviceError>,
    pub(crate) status_banner: String,

    pub(crate) stage_colors: [[u8; 3]; MAX_DPI_STAGES],

    pub(crate) selected_slot: usize,
    pub(crate) edit_sc_ctrl: bool,
    pub(crate) edit_sc_shift: bool,
    pub(crate) edit_sc_alt: bool,
    pub(crate) edit_sc_win: bool,
    pub(crate) edit_sc_keycode: u8,
    pub(crate) edit_macro_slot: u8,
    pub(crate) edit_macro_repeat: u8,
    pub(crate) edit_macro_mode: MacroLoopKind,

    pub(crate) macros: Vec<UiMacroSlot>,
    pub(crate) selected_macro_idx: usize,
    pub(crate) new_step_is_mouse: bool,
    pub(crate) new_step_keycode: u8,
    pub(crate) new_step_mouse_btn: MouseClickButton,
    pub(crate) new_step_delay_ms: u16,

    pub(crate) saved_profiles: Vec<(PathBuf, HardwareProfile)>,
    pub(crate) new_profile_name: String,
    pub(crate) new_profile_process: String,

    pub(crate) show_raw_debugger_drawer: bool,
    pub(crate) trace_entries: VecDeque<UiTraceRow>,
    pub(crate) custom_hex_input: String,
    pub(crate) custom_hex_decoded: String,

    pub(crate) worker_tx: Sender<WorkerCommand>,
    pub(crate) worker_rx: Receiver<WorkerEvent>,
    pub(crate) worker_join: Option<JoinHandle<()>>,
}

impl Bkr1xGuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_dark_technical_theme(&cc.egui_ctx);
        let tray_started = ensure_tray_daemon_spawned();
        let saved_profiles = list_saved_profiles();
        let autostart_enabled = is_autostart_enabled();

        let (worker_tx, worker_rx, worker_join) = spawn_hardware_worker(cc.egui_ctx.clone());
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
            edit_macro_mode: MacroLoopKind::HoldToLoop,
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
            worker_join,
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
                        self.hw_state.connection.map_or("USB", |m| m.as_str()),
                        if via_daemon {
                            "(Tray Agent)"
                        } else {
                            "(Direct HID)"
                        },
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

    fn append_traces(&mut self, incoming: Vec<UiTraceRow>) {
        for entry in incoming {
            if self.trace_entries.len() >= MAX_UI_TRACE_LOG {
                self.trace_entries.pop_front();
            }
            self.trace_entries.push_back(entry);
        }
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

#[cfg(windows)]
fn focus_existing_gui_if_open() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    let wide: Vec<u16> = GUI_WINDOW_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer valid for the duration of `FindWindowW`.
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
