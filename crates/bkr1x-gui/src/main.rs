#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod keycodes;
mod panels;
mod worker;

use std::io::Read;
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
use eframe::egui::{self, CornerRadius, Stroke};
use flate2::read::DeflateDecoder;

use crate::panels::{
    AssignmentsSubTab, BG_CANVAS_BLACK, BG_CARD_CHARCOAL, BG_CONTROL_SLATE, BG_HOVER_SLATE,
    CYAN_ACCENT, MacroLoopKind, ProfilesSubTab, RailTab, SensitivitySubTab, TEXT_MUTED_GRAY,
    TEXT_WHITE,
};
use crate::worker::{UiMacroSlot, WorkerCommand, WorkerEvent, spawn_hardware_worker};

pub(crate) const MOUSE_PHOTO_WIDTH: usize = 300;
pub(crate) const MOUSE_PHOTO_HEIGHT: usize = 528;
const MOUSE_PHOTO_DEFLATE: &[u8] = include_bytes!("../assets/bkr1x_mouse_300x528.deflate");

fn load_mouse_photo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let expected_len = MOUSE_PHOTO_WIDTH * MOUSE_PHOTO_HEIGHT * 4;
    let mut rgba = Vec::with_capacity(expected_len);
    let mut decoder = DeflateDecoder::new(MOUSE_PHOTO_DEFLATE);
    let _ = decoder.read_to_end(&mut rgba);
    if rgba.len() != expected_len {
        rgba.resize(expected_len, 0);
    }
    let color_image =
        egui::ColorImage::from_rgba_unmultiplied([MOUSE_PHOTO_WIDTH, MOUSE_PHOTO_HEIGHT], &rgba);
    ctx.load_texture(
        "bkr1x_mouse_photo",
        color_image,
        egui::TextureOptions::LINEAR,
    )
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
    pub(crate) mouse_texture: egui::TextureHandle,
    pub(crate) rail_tab: RailTab,
    pub(crate) sensitivity_sub_tab: SensitivitySubTab,
    pub(crate) assignments_sub_tab: AssignmentsSubTab,
    pub(crate) profiles_sub_tab: ProfilesSubTab,
    pub(crate) active_profile_label: String,
    pub(crate) show_settings_modal: bool,

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

    pub(crate) worker_tx: Sender<WorkerCommand>,
    pub(crate) worker_rx: Receiver<WorkerEvent>,
    pub(crate) worker_join: Option<JoinHandle<()>>,
}

impl Bkr1xGuiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_ghub_theme(&cc.egui_ctx);
        let mouse_texture = load_mouse_photo_texture(&cc.egui_ctx);
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

        let mut stage_colors = DEFAULT_STAGE_COLORS;
        stage_colors[2] = [0x00, 0xB8, 0xFC];

        Self {
            mouse_texture,
            rail_tab: RailTab::Sensitivity,
            sensitivity_sub_tab: SensitivitySubTab::Dpi,
            assignments_sub_tab: AssignmentsSubTab::Buttons,
            profiles_sub_tab: ProfilesSubTab::Profiles,
            active_profile_label: "DEFAULT".to_owned(),
            show_settings_modal: false,
            hw_state: HardwareStateMachine::new(),
            connected_device_label: None,
            connected_via_daemon: false,
            tray_agent_running: tray_started,
            autostart_enabled,
            last_error: None,
            status_banner: "SYNCHRONIZING HARDWARE...".to_owned(),
            stage_colors,
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
            new_profile_name: "DEFAULT".to_owned(),
            new_profile_process: String::new(),
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
                } => {
                    self.hw_state = *state;
                    self.connected_via_daemon = via_daemon;
                    self.tray_agent_running = via_daemon || is_daemon_running();
                    self.connected_device_label =
                        Some(format!("{product_name} (0x{vid:04X}:0x{pid:04X})"));
                    self.last_error = None;
                    self.status_banner = format!(
                        "{} {} · FW v{}",
                        self.hw_state.connection.map_or("USB", |m| m.as_str()),
                        if via_daemon {
                            "TRAY AGENT"
                        } else {
                            "DIRECT HID"
                        },
                        self.hw_state.firmware
                    );
                }
                WorkerEvent::FlashSuccess { message } => {
                    self.last_error = None;
                    self.status_banner = message;
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
}

impl eframe::App for Bkr1xGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_worker_events();

        egui::TopBottomPanel::top("ghub_top_bar")
            .exact_height(46.0)
            .frame(
                egui::Frame::NONE
                    .fill(BG_CANVAS_BLACK)
                    .inner_margin(egui::Margin::symmetric(18, 8)),
            )
            .show(ctx, |ui| {
                self.draw_top_bar(ui);
            });

        egui::TopBottomPanel::bottom("ghub_bottom_dock")
            .exact_height(68.0)
            .frame(
                egui::Frame::NONE
                    .fill(BG_CANVAS_BLACK)
                    .inner_margin(egui::Margin::symmetric(18, 6)),
            )
            .show(ctx, |ui| {
                self.draw_bottom_dock(ui);
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(BG_CANVAS_BLACK)
                    .inner_margin(egui::Margin::symmetric(18, 8)),
            )
            .show(ctx, |ui| {
                self.draw_ghub_workspace(ui);
            });

        if self.show_settings_modal {
            self.draw_settings_modal(ctx);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.worker_tx.send(WorkerCommand::Shutdown);
        if let Some(handle) = self.worker_join.take() {
            let _ = handle.join();
        }
    }
}

fn configure_ghub_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(16);

    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.panel_fill = BG_CANVAS_BLACK;
    visuals.window_fill = BG_CARD_CHARCOAL;
    visuals.extreme_bg_color = BG_CANVAS_BLACK;
    visuals.faint_bg_color = BG_CARD_CHARCOAL;
    visuals.override_text_color = None;

    let sharp = CornerRadius::same(2);
    visuals.window_corner_radius = sharp;
    visuals.menu_corner_radius = sharp;
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.popup_shadow = egui::Shadow::NONE;
    visuals.window_stroke = Stroke::new(1.0_f32, BG_CONTROL_SLATE);

    visuals.widgets.noninteractive.bg_fill = BG_CARD_CHARCOAL;
    visuals.widgets.noninteractive.weak_bg_fill = BG_CARD_CHARCOAL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::NONE;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_MUTED_GRAY);
    visuals.widgets.noninteractive.corner_radius = sharp;

    visuals.widgets.inactive.bg_fill = BG_CONTROL_SLATE;
    visuals.widgets.inactive.weak_bg_fill = BG_CONTROL_SLATE;
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT_WHITE);
    visuals.widgets.inactive.corner_radius = sharp;

    visuals.widgets.hovered.bg_fill = BG_HOVER_SLATE;
    visuals.widgets.hovered.weak_bg_fill = BG_HOVER_SLATE;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, CYAN_ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT_WHITE);
    visuals.widgets.hovered.corner_radius = sharp;

    visuals.widgets.active.bg_fill = CYAN_ACCENT;
    visuals.widgets.active.weak_bg_fill = CYAN_ACCENT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, TEXT_WHITE);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5_f32, TEXT_WHITE);
    visuals.widgets.active.corner_radius = sharp;

    visuals.widgets.open.bg_fill = BG_CONTROL_SLATE;
    visuals.widgets.open.weak_bg_fill = BG_CONTROL_SLATE;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, CYAN_ACCENT);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, TEXT_WHITE);
    visuals.widgets.open.corner_radius = sharp;

    visuals.selection.bg_fill = CYAN_ACCENT;
    visuals.selection.stroke = Stroke::new(1.0_f32, TEXT_WHITE);

    style.visuals = visuals;
    ctx.set_style(style);
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
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([920.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "bkr1x-gui",
        native_options,
        Box::new(|cc| Ok(Box::new(Bkr1xGuiApp::new(cc)))),
    )
}
