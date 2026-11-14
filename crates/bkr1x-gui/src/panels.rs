use std::f32::consts::TAU;
use std::fs;
use std::path::PathBuf;

use bkr1x_hal::{
    DeviceError, HardwareProfile, IpcRequest, TraceDirection, decode_flexible_hex,
    ensure_user_profiles_dir, install_user_application, list_saved_profiles, send_ipc_request,
    set_autostart_enabled,
};
use bkr1x_protocol::{
    BUTTON_SLOT_COUNT, ButtonAction, ButtonMapping, ButtonSlot, ChecksumStrategy, KeyModifiers,
    LightMode, LodHeight, MACRO_CHUNK_DATA_LEN, MACRO_DIRECTORY_LEN, MACRO_FLASH_TOTAL_LEN,
    MAX_MACRO_SLOTS, MacroInput, MacroStep, MacroTrigger, MediaFunction, MouseClickButton,
    MouseConfig, PAW3311_DPI_MAX, PAW3311_DPI_MIN, PAW3311_DPI_STEP, PacketCodec, PollingRate,
    REPORT_PAYLOAD_LEN, ScrollDirection, VALID_SLEEP_MINUTES, compile_macro_bank,
};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Vec2,
};

use crate::keycodes::{HID_KEY_TABLE, keycode_label};
use crate::worker::{UiMacroSlot, UiTraceRow, WorkerCommand};
use crate::{Bkr1xGuiApp, MAX_UI_TRACE_LOG, ensure_tray_daemon_spawned};

pub const BG_CANVAS_BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
pub const BG_CARD_CHARCOAL: Color32 = Color32::from_rgb(0x18, 0x19, 0x1C);
pub const BG_CONTROL_SLATE: Color32 = Color32::from_rgb(0x22, 0x24, 0x28);
pub const BG_HOVER_SLATE: Color32 = Color32::from_rgb(0x2B, 0x2E, 0x33);
pub const CYAN_ACCENT: Color32 = Color32::from_rgb(0x00, 0xB8, 0xFC);
pub const TEXT_WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const TEXT_MUTED_GRAY: Color32 = Color32::from_rgb(0x8E, 0x92, 0x97);

const PRESET_SWATCHES: [[u8; 3]; 12] = [
    [0x00, 0xB8, 0xFC],
    [0x29, 0x79, 0xFF],
    [0xAA, 0x00, 0xFF],
    [0xFF, 0x00, 0x6E],
    [0xFF, 0x17, 0x44],
    [0xFF, 0x6D, 0x00],
    [0xFF, 0xD6, 0x00],
    [0x76, 0xFF, 0x03],
    [0x00, 0xE6, 0x76],
    [0x1D, 0xE9, 0xB6],
    [0xFF, 0xFF, 0xFF],
    [0x8E, 0x92, 0x97],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailTab {
    Lighting,
    Assignments,
    Sensitivity,
    Profiles,
    Telemetry,
}

impl RailTab {
    pub const ALL: [Self; 5] = [
        Self::Lighting,
        Self::Assignments,
        Self::Sensitivity,
        Self::Profiles,
        Self::Telemetry,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Lighting => "LIGHTING",
            Self::Assignments => "ASSIGNMENTS",
            Self::Sensitivity => "SENSITIVITY",
            Self::Profiles => "PROFILES & STARTUP",
            Self::Telemetry => "TELEMETRY & HID",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingZone {
    Primary,
    Logo,
}

impl LightingZone {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Primary => "PRIMARY",
            Self::Logo => "LOGO",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitivitySubTab {
    Dpi,
    Polling,
}

impl SensitivitySubTab {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dpi => "DPI STAGES",
            Self::Polling => "POLLING & DSP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentsSubTab {
    Buttons,
    Keys,
    Macros,
}

impl AssignmentsSubTab {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Buttons => "BUTTONS",
            Self::Keys => "KEYS",
            Self::Macros => "MACROS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfilesSubTab {
    Profiles,
    Startup,
}

impl ProfilesSubTab {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Profiles => "PROFILES",
            Self::Startup => "STARTUP & TRAY",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetrySubTab {
    Telemetry,
    HexVerifier,
}

impl TelemetrySubTab {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Telemetry => "TELEMETRY",
            Self::HexVerifier => "HEX VERIFIER",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroLoopKind {
    FixedCount,
    HoldToLoop,
    ToggleLoop,
}

impl MacroLoopKind {
    pub const ALL: [Self; 3] = [Self::FixedCount, Self::HoldToLoop, Self::ToggleLoop];

    pub const fn label(self) -> &'static str {
        match self {
            Self::FixedCount => "FIXED REPEAT",
            Self::HoldToLoop => "HOLD TO LOOP",
            Self::ToggleLoop => "TOGGLE LOOP",
        }
    }

    pub fn to_trigger(self, repeat_count: u8) -> MacroTrigger {
        match self {
            Self::FixedCount => MacroTrigger::FixedCount(repeat_count.max(1)),
            Self::HoldToLoop => MacroTrigger::HoldToLoop,
            Self::ToggleLoop => MacroTrigger::ToggleLoop,
        }
    }
}

const fn ghub_effect_label(mode: LightMode) -> &'static str {
    match mode {
        LightMode::Off => "OFF",
        LightMode::Wave => "WAVE",
        LightMode::Neon => "FIXED / NEON",
        LightMode::CyclingFlash => "CYCLE FLASH",
        LightMode::YoyoBall => "YOYO BALL",
        LightMode::SingleFlash => "SINGLE FLASH",
        LightMode::BreathingLoop => "BREATHING",
    }
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);

    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;

    let (r1, g1, b1) = if h < 1.0 {
        (c, x, 0.0)
    } else if h < 2.0 {
        (x, c, 0.0)
    } else if h < 3.0 {
        (0.0, c, x)
    } else if h < 4.0 {
        (0.0, x, c)
    } else if h < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    [
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    ]
}

pub fn rgb_to_hsv(rgb: [u8; 3]) -> [f32; 3] {
    let r = f32::from(rgb[0]) / 255.0;
    let g = f32::from(rgb[1]) / 255.0;
    let b = f32::from(rgb[2]) / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta <= f32::EPSILON {
        0.0
    } else if (max - r).abs() <= f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0) / 6.0
    } else if (max - g).abs() <= f32::EPSILON {
        (((b - r) / delta) + 2.0) / 6.0
    } else {
        (((r - g) / delta) + 4.0) / 6.0
    };

    let s = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    [h, s, max]
}

pub fn sanitize_profile_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "custom-profile".to_owned()
    } else {
        slug
    }
}

pub fn describe_button_action(action: ButtonAction) -> String {
    match action {
        ButtonAction::MouseClick(btn) => btn.label().to_owned(),
        ButtonAction::Disabled => "Disabled".to_owned(),
        ButtonAction::ScrollUp => "Scroll Up".to_owned(),
        ButtonAction::ScrollDown => "Scroll Down".to_owned(),
        ButtonAction::DpiLoop => "DPI Stage Loop".to_owned(),
        ButtonAction::Media(media) => format!("Media: {}", media.label()),
        ButtonAction::KeyboardShortcut { modifiers, keycode } => {
            let mut out = String::with_capacity(24);
            for (present, label) in [
                (modifiers.has_ctrl(), "Ctrl+"),
                (modifiers.has_shift(), "Shift+"),
                (modifiers.has_alt(), "Alt+"),
                (modifiers.has_win(), "Win+"),
            ] {
                if present {
                    out.push_str(label);
                }
            }
            out.push_str(keycode_label(keycode));
            out
        }
        ButtonAction::Macro { slot, trigger } => match trigger {
            MacroTrigger::FixedCount(n) => format!("Macro #{slot} ({n}x)"),
            MacroTrigger::HoldToLoop => format!("Macro #{slot} (Hold Loop)"),
            MacroTrigger::ToggleLoop => format!("Macro #{slot} (Toggle Loop)"),
        },
        ButtonAction::Raw(bytes) => format!("Raw {bytes:02X?}"),
    }
}

pub fn verify_hex_string(raw: &str) -> String {
    let bytes = match decode_flexible_hex(raw) {
        Ok(bytes) => bytes,
        Err(err) => return format!("Invalid hex input: {err}"),
    };
    let codec = PacketCodec::new(ChecksumStrategy::None);
    match codec.decode(&bytes) {
        Ok(pkt) => format!("{pkt:#?}"),
        Err(err) => format!("Decode Error: {err}"),
    }
}

fn draw_sub_tab_bar<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    active: &mut T,
    tabs: &[(T, &'static str)],
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 22.0;
        for &(tab_val, label) in tabs {
            let is_selected = *active == tab_val;
            let text_col = if is_selected {
                TEXT_WHITE
            } else {
                TEXT_MUTED_GRAY
            };
            let galley =
                ui.painter()
                    .layout_no_wrap(label.to_owned(), FontId::proportional(12.5), text_col);
            let desired = Vec2::new(galley.size().x + 4.0, 26.0);
            let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
            if resp.clicked() {
                *active = tab_val;
            }
            let draw_col = if is_selected || resp.hovered() {
                TEXT_WHITE
            } else {
                TEXT_MUTED_GRAY
            };
            ui.painter().text(
                Pos2::new(rect.left(), rect.center().y - 2.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(12.5),
                draw_col,
            );
            if is_selected {
                let underline = Rect::from_min_max(
                    Pos2::new(rect.left(), rect.bottom() - 2.0),
                    Pos2::new(rect.right(), rect.bottom()),
                );
                ui.painter()
                    .rect_filled(underline, CornerRadius::ZERO, TEXT_WHITE);
            }
        }
    });
}

fn ghub_pill_button(ui: &mut egui::Ui, label: &str, selected: bool, min_width: f32) -> bool {
    let fill = if selected {
        CYAN_ACCENT
    } else {
        BG_CONTROL_SLATE
    };
    let fg = if selected {
        TEXT_WHITE
    } else {
        TEXT_MUTED_GRAY
    };
    let btn = egui::Button::new(RichText::new(label).size(11.5).strong().color(fg))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(2))
        .min_size(Vec2::new(min_width, 28.0));
    ui.add(btn).clicked()
}

fn ghub_action_button(ui: &mut egui::Ui, label: &str) -> bool {
    let btn = egui::Button::new(RichText::new(label).size(12.0).strong().color(TEXT_WHITE))
        .fill(CYAN_ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(2))
        .min_size(Vec2::new(ui.available_width(), 32.0));
    ui.add(btn).clicked()
}

impl Bkr1xGuiApp {
    pub(crate) fn active_zone_rgb(&self) -> [u8; 3] {
        match self.lighting_zone {
            LightingZone::Primary => self.primary_rgb,
            LightingZone::Logo => self.logo_rgb,
        }
    }

    pub(crate) fn set_active_zone_rgb(&mut self, rgb: [u8; 3]) {
        match self.lighting_zone {
            LightingZone::Primary => {
                self.primary_rgb = rgb;
                let active_idx = usize::from(self.hw_state.config.active_dpi_stage)
                    .min(self.stage_colors.len() - 1);
                self.stage_colors[active_idx] = rgb;
            }
            LightingZone::Logo => {
                self.logo_rgb = rgb;
            }
        }
        self.lighting_hsv = rgb_to_hsv(rgb);
    }

    pub(crate) fn record_preview_packet(
        &mut self,
        report_id: u8,
        payload: &[u8; REPORT_PAYLOAD_LEN],
        note: String,
    ) {
        if self.trace_entries.len() >= MAX_UI_TRACE_LOG {
            self.trace_entries.pop_front();
        }
        self.trace_entries
            .push_back(UiTraceRow::preview(report_id, payload, note));
    }

    pub(crate) fn trigger_flash_config(&mut self) {
        let codec = PacketCodec::new(ChecksumStrategy::None);
        match codec.encode_set_mouse_config(&self.hw_state.config) {
            Ok(rpt) => {
                if self.connected_device_label.is_none() {
                    self.record_preview_packet(
                        rpt.report_id,
                        &rpt.payload,
                        "PREVIEW TxSetMouseConfig (0x0F)".to_owned(),
                    );
                }
                let _ = self
                    .worker_tx
                    .send(WorkerCommand::FlashConfig(self.hw_state.config));
            }
            Err(err) => {
                self.status_banner = format!("Validation rejected: {err}");
            }
        }
    }

    pub(crate) fn trigger_flash_buttons(&mut self) {
        let codec = PacketCodec::new(ChecksumStrategy::None);
        match codec.encode_set_mouse_keys(&self.hw_state.buttons) {
            Ok(rpt) => {
                if self.connected_device_label.is_none() {
                    self.record_preview_packet(
                        rpt.report_id,
                        &rpt.payload,
                        "PREVIEW TxSetMouseKeys (0x09)".to_owned(),
                    );
                }
                let _ = self
                    .worker_tx
                    .send(WorkerCommand::FlashButtons(self.hw_state.buttons));
            }
            Err(err) => {
                self.status_banner = format!("Safety check rejected: {err}");
            }
        }
    }

    pub(crate) fn trigger_flash_macros(&mut self) {
        let slices: Vec<&[MacroStep]> = self.macros.iter().map(|m| m.steps.as_slice()).collect();
        let mut flash = [0u8; MACRO_FLASH_TOTAL_LEN];
        match compile_macro_bank(&slices, &mut flash) {
            Ok(used) => {
                if self.connected_device_label.is_none() {
                    let codec = PacketCodec::new(ChecksumStrategy::None);
                    let first_chunk =
                        codec.encode_write_macro_chunk(0, &flash[..used.min(MACRO_CHUNK_DATA_LEN)]);
                    self.record_preview_packet(
                        first_chunk.report_id,
                        &first_chunk.payload,
                        format!("PREVIEW TxWriteMacroChunk ({used}B total)"),
                    );
                }
                let _ = self
                    .worker_tx
                    .send(WorkerCommand::FlashMacros(self.macros.clone()));
            }
            Err(err) => {
                self.status_banner = format!("Macro compilation failed: {err}");
            }
        }
    }

    pub(crate) fn estimate_macro_flash_bytes(&self) -> usize {
        let event_bytes: usize = self
            .macros
            .iter()
            .map(|m| m.steps.len().saturating_mul(4))
            .sum();
        MACRO_DIRECTORY_LEN.saturating_add(event_bytes)
    }

    pub(crate) fn assign_slot_with_safety(&mut self, slot: usize, action: ButtonAction) {
        if slot >= BUTTON_SLOT_COUNT {
            return;
        }
        let mut candidate = self.hw_state.buttons;
        candidate.slots[slot] = action;
        match candidate.validate() {
            Ok(()) => {
                self.hw_state.buttons = candidate;
                self.status_banner = format!(
                    "BOUND {} -> {}",
                    ButtonSlot::ALL[slot].label().to_uppercase(),
                    describe_button_action(action).to_uppercase()
                );
            }
            Err(err) => {
                self.status_banner = format!("Safety guard: {err}");
            }
        }
    }

    pub(crate) fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let (home_rect, home_resp) =
                ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
            let home_col = if home_resp.hovered() {
                CYAN_ACCENT
            } else {
                TEXT_MUTED_GRAY
            };
            if home_resp
                .on_hover_text("Sync with BK-R1X Hardware / Return to Main View")
                .clicked()
            {
                self.show_settings_modal = false;
                let _ = self.worker_tx.send(WorkerCommand::ConnectAndSync {
                    force_refresh: true,
                });
            }
            paint_home_back_icon(ui.painter(), home_rect.center(), home_col);

            ui.add_space(6.0);

            let profile_header = format!("PROFILE: {} ⌄", self.active_profile_label.to_uppercase());
            ui.menu_button(
                RichText::new(profile_header)
                    .size(13.0)
                    .strong()
                    .color(TEXT_WHITE),
                |ui| {
                    ui.set_min_width(220.0);
                    if ui
                        .selectable_label(
                            self.active_profile_label.eq_ignore_ascii_case("DEFAULT"),
                            "DEFAULT (FACTORY BASELINE)",
                        )
                        .clicked()
                    {
                        self.hw_state.config = MouseConfig::factory_default();
                        self.hw_state.buttons = ButtonMapping::factory_default();
                        self.active_profile_label = "DEFAULT".to_owned();
                        self.trigger_flash_config();
                        ui.close_menu();
                    }

                    let mut apply_prof: Option<HardwareProfile> = None;
                    for (_, prof) in &self.saved_profiles {
                        let selected = self.active_profile_label.eq_ignore_ascii_case(&prof.name);
                        let label = format!(
                            "{} ({} HZ · {} DPI)",
                            prof.name.to_uppercase(),
                            prof.sensor.polling_rate_hz,
                            prof.sensor.dpi_stages.first().copied().unwrap_or(1600)
                        );
                        if ui.selectable_label(selected, label).clicked() {
                            apply_prof = Some(prof.clone());
                            ui.close_menu();
                        }
                    }

                    if let Some(prof) = apply_prof {
                        self.apply_loaded_profile(prof);
                    }

                    ui.separator();
                    if ui.button("⚙ MANAGE / SAVE PROFILES...").clicked() {
                        self.show_settings_modal = true;
                        ui.close_menu();
                    }
                },
            );

            if matches!(
                self.last_error,
                Some(DeviceError::BluetoothConfigUnsupported)
            ) {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("⚠ BLUETOOTH READ-ONLY — USE USB-C OR 2.4GHZ DONGLE")
                        .size(11.0)
                        .strong()
                        .color(Color32::from_rgb(0xFF, 0xB3, 0x00)),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (gear_rect, gear_resp) =
                    ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
                let gear_col = if self.show_settings_modal || gear_resp.hovered() {
                    CYAN_ACCENT
                } else {
                    TEXT_MUTED_GRAY
                };
                if gear_resp
                    .on_hover_text(
                        "Device Settings, Profiles, Windows Startup & HID Wire Inspector",
                    )
                    .clicked()
                {
                    self.show_settings_modal = !self.show_settings_modal;
                }
                paint_gear_icon(ui.painter(), gear_rect.center(), gear_col);

                ui.add_space(4.0);
                if ghub_pill_button(ui, "⌨ RAW HID", self.show_raw_debugger_drawer, 78.0) {
                    self.show_raw_debugger_drawer = !self.show_raw_debugger_drawer;
                }

                if ghub_pill_button(ui, "↻ SYNC", false, 64.0) {
                    let _ = self.worker_tx.send(WorkerCommand::ConnectAndSync {
                        force_refresh: true,
                    });
                }

                let pct = self.hw_state.battery.percent;
                let bat_txt = if self.connected_device_label.is_some() {
                    let prefix = if self.hw_state.battery.charging {
                        "⚡ "
                    } else {
                        "🔋 "
                    };
                    format!("{prefix}{pct}%")
                } else {
                    "🔋 --%".to_owned()
                };
                ui.label(RichText::new(bat_txt).size(11.5).strong().color(
                    if self.connected_device_label.is_some() {
                        CYAN_ACCENT
                    } else {
                        TEXT_MUTED_GRAY
                    },
                ));

                ui.add_space(6.0);
                ui.label(
                    RichText::new(self.status_banner.to_uppercase())
                        .size(11.0)
                        .color(TEXT_MUTED_GRAY),
                );
            });
        });
    }

    fn apply_loaded_profile(&mut self, prof: HardwareProfile) {
        if let Ok(cfg) = prof.to_mouse_config() {
            self.hw_state.config = cfg;
        }
        if prof.buttons.is_some()
            && let Ok(btns) = prof.to_button_mapping()
        {
            self.hw_state.buttons = btns;
        }
        self.active_profile_label = prof.name.to_uppercase();
        self.new_profile_name.clone_from(&prof.name);
        self.new_profile_process = prof.match_process.clone().unwrap_or_default();
        let _ = self
            .worker_tx
            .send(WorkerCommand::ApplySavedProfile(Box::new(prof)));
    }

    pub(crate) fn draw_ghub_workspace(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let card_height = (avail.height() - 16.0).max(420.0);
        let top_y = avail.top() + 8.0;

        let rail_width = 48.0;
        let card_width = 336.0;

        let rail_rect = Rect::from_min_size(
            Pos2::new(avail.left(), top_y),
            Vec2::new(rail_width, rail_width * 5.0 + 8.0),
        );
        let card_rect = Rect::from_min_size(
            Pos2::new(rail_rect.right(), top_y),
            Vec2::new(card_width, card_height),
        );
        let viewport_rect =
            Rect::from_min_max(Pos2::new(card_rect.right() + 16.0, avail.top()), avail.max);

        self.draw_left_icon_rail(ui, rail_rect);
        self.draw_floating_control_card(ui, card_rect);
        self.draw_interactive_mouse_viewport(ui, viewport_rect);
    }

    fn draw_left_icon_rail(&mut self, ui: &mut egui::Ui, rail_rect: Rect) {
        for (idx, tab) in RailTab::ALL.iter().copied().enumerate() {
            let y = rail_rect.top() + (idx as f32) * 50.0;
            let tile_rect =
                Rect::from_min_size(Pos2::new(rail_rect.left(), y), Vec2::new(48.0, 48.0));
            let resp = ui.interact(
                tile_rect,
                egui::Id::new(("ghub_rail_tab", idx)),
                Sense::click(),
            );
            if resp.clicked() {
                self.rail_tab = tab;
            }

            let is_active = self.rail_tab == tab;
            let bg = if is_active {
                CYAN_ACCENT
            } else if resp.hovered() {
                BG_HOVER_SLATE
            } else {
                BG_CONTROL_SLATE
            };
            let fg = if is_active || resp.hovered() {
                TEXT_WHITE
            } else {
                TEXT_MUTED_GRAY
            };

            ui.painter()
                .rect_filled(tile_rect, CornerRadius::same(2), bg);

            match tab {
                RailTab::Lighting => paint_sunburst_icon(ui.painter(), tile_rect.center(), fg),
                RailTab::Assignments => {
                    paint_assignments_plus_icon(ui.painter(), tile_rect.center(), fg);
                }
                RailTab::Sensitivity => {
                    paint_sensitivity_crosshair_icon(ui.painter(), tile_rect.center(), fg);
                }
                RailTab::Profiles => {
                    paint_profiles_sliders_icon(ui.painter(), tile_rect.center(), fg);
                }
                RailTab::Telemetry => {
                    paint_telemetry_chip_icon(ui.painter(), tile_rect.center(), fg);
                }
            }

            resp.on_hover_text(tab.title());
        }
    }

    fn draw_floating_control_card(&mut self, ui: &mut egui::Ui, card_rect: Rect) {
        ui.painter()
            .rect_filled(card_rect, CornerRadius::same(2), BG_CARD_CHARCOAL);

        let inner = card_rect.shrink2(Vec2::new(20.0, 18.0));
        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );

        child_ui.label(
            RichText::new(self.rail_tab.title())
                .font(FontId::proportional(20.0))
                .strong()
                .color(TEXT_WHITE),
        );
        child_ui.add_space(6.0);

        match self.rail_tab {
            RailTab::Lighting => {
                let prev_zone = self.lighting_zone;
                draw_sub_tab_bar(
                    &mut child_ui,
                    &mut self.lighting_zone,
                    &[
                        (LightingZone::Primary, LightingZone::Primary.label()),
                        (LightingZone::Logo, LightingZone::Logo.label()),
                    ],
                );
                if self.lighting_zone != prev_zone {
                    self.lighting_hsv = rgb_to_hsv(self.active_zone_rgb());
                }
                child_ui.add_space(10.0);
                self.draw_lighting_card_contents(&mut child_ui);
            }
            RailTab::Assignments => {
                draw_sub_tab_bar(
                    &mut child_ui,
                    &mut self.assignments_sub_tab,
                    &[
                        (
                            AssignmentsSubTab::Buttons,
                            AssignmentsSubTab::Buttons.label(),
                        ),
                        (AssignmentsSubTab::Keys, AssignmentsSubTab::Keys.label()),
                        (AssignmentsSubTab::Macros, AssignmentsSubTab::Macros.label()),
                    ],
                );
                child_ui.add_space(10.0);
                self.draw_assignments_card_contents(&mut child_ui);
            }
            RailTab::Sensitivity => {
                draw_sub_tab_bar(
                    &mut child_ui,
                    &mut self.sensitivity_sub_tab,
                    &[
                        (SensitivitySubTab::Dpi, SensitivitySubTab::Dpi.label()),
                        (
                            SensitivitySubTab::Polling,
                            SensitivitySubTab::Polling.label(),
                        ),
                    ],
                );
                child_ui.add_space(10.0);
                self.draw_sensitivity_card_contents(&mut child_ui);
            }
            RailTab::Profiles => {
                draw_sub_tab_bar(
                    &mut child_ui,
                    &mut self.profiles_sub_tab,
                    &[
                        (ProfilesSubTab::Profiles, ProfilesSubTab::Profiles.label()),
                        (ProfilesSubTab::Startup, ProfilesSubTab::Startup.label()),
                    ],
                );
                child_ui.add_space(10.0);
                match self.profiles_sub_tab {
                    ProfilesSubTab::Profiles => self.draw_saved_profiles_card(&mut child_ui),
                    ProfilesSubTab::Startup => self.draw_startup_and_tray_card(&mut child_ui),
                }
            }
            RailTab::Telemetry => {
                draw_sub_tab_bar(
                    &mut child_ui,
                    &mut self.telemetry_sub_tab,
                    &[
                        (
                            TelemetrySubTab::Telemetry,
                            TelemetrySubTab::Telemetry.label(),
                        ),
                        (
                            TelemetrySubTab::HexVerifier,
                            TelemetrySubTab::HexVerifier.label(),
                        ),
                    ],
                );
                child_ui.add_space(10.0);
                self.draw_telemetry_card_contents(&mut child_ui);
            }
        }
    }

    fn draw_lighting_card_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("EFFECT")
                    .size(11.5)
                    .strong()
                    .color(TEXT_MUTED_GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let current_label =
                    format!("{} ⌄", ghub_effect_label(self.hw_state.config.light_mode));
                egui::ComboBox::from_id_salt("ghub_effect_combo")
                    .width(150.0)
                    .selected_text(
                        RichText::new(current_label)
                            .size(12.0)
                            .strong()
                            .color(TEXT_WHITE),
                    )
                    .show_ui(ui, |ui| {
                        for mode in LightMode::ALL {
                            if ui
                                .selectable_value(
                                    &mut self.hw_state.config.light_mode,
                                    mode,
                                    ghub_effect_label(mode),
                                )
                                .clicked()
                            {
                                self.trigger_flash_config();
                            }
                        }
                    });
            });
        });

        ui.add_space(10.0);

        self.draw_hsv_color_wheel_and_rgb(ui);

        ui.add_space(10.0);
        ui.label(
            RichText::new("BRIGHTNESS")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        ui.add_space(2.0);
        self.draw_brightness_gradient_slider(ui);

        ui.add_space(12.0);
        ui.label(
            RichText::new("PRESET SWATCHES")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        ui.add_space(4.0);
        self.draw_preset_color_swatches(ui);

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("DEEP SLEEP TIMEOUT")
                    .size(11.0)
                    .strong()
                    .color(TEXT_MUTED_GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("ghub_lighting_sleep_combo")
                    .width(95.0)
                    .selected_text(format!("{} MIN ⌄", self.hw_state.config.sleep_minutes))
                    .show_ui(ui, |ui| {
                        for mins in VALID_SLEEP_MINUTES {
                            ui.selectable_value(
                                &mut self.hw_state.config.sleep_minutes,
                                mins,
                                format!("{mins} MIN"),
                            );
                        }
                    });
            });
        });

        ui.checkbox(
            &mut self.hw_state.config.move_lights_off,
            RichText::new("BATTERY SAVER (DIM WHILE MOVING)")
                .size(11.0)
                .color(TEXT_MUTED_GRAY),
        );
        ui.checkbox(
            &mut self.hw_state.config.wakeup_on_move,
            RichText::new("WAKE SENSOR ON MOVEMENT")
                .size(11.0)
                .color(TEXT_MUTED_GRAY),
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    [92.0, 32.0],
                    egui::Button::new(
                        RichText::new("DEFAULT")
                            .size(11.0)
                            .strong()
                            .color(TEXT_MUTED_GRAY),
                    )
                    .fill(BG_CONTROL_SLATE),
                )
                .clicked()
            {
                self.hw_state.config = MouseConfig::factory_default();
                self.trigger_flash_config();
            }
            if ghub_action_button(ui, "APPLY LIGHTING") {
                self.trigger_flash_config();
            }
        });
    }

    fn draw_hsv_color_wheel_and_rgb(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let wheel_size = 152.0;
            let (wheel_rect, wheel_resp) =
                ui.allocate_exact_size(Vec2::splat(wheel_size), Sense::click_and_drag());
            let center = wheel_rect.center();
            let outer_r = wheel_size * 0.46;
            let inner_r = wheel_size * 0.31;
            let mid_r = (outer_r + inner_r) * 0.5;
            let ring_thickness = outer_r - inner_r;

            if (wheel_resp.clicked() || wheel_resp.dragged())
                && let Some(pos) = wheel_resp.interact_pointer_pos()
            {
                let delta = pos - center;
                let dist = delta.length();
                if dist >= inner_r * 0.5 {
                    let angle = delta.y.atan2(delta.x);
                    let hue = (angle / TAU).rem_euclid(1.0);
                    self.lighting_hsv[0] = hue;
                    if self.lighting_hsv[1] < 0.15 {
                        self.lighting_hsv[1] = 1.0;
                    }
                    let new_rgb = hsv_to_rgb(
                        self.lighting_hsv[0],
                        self.lighting_hsv[1],
                        self.lighting_hsv[2],
                    );
                    self.set_active_zone_rgb(new_rgb);
                }
            }

            let painter = ui.painter();
            let segments = 72;
            for i in 0..segments {
                let t0 = (i as f32) / (segments as f32);
                let t1 = ((i + 1) as f32) / (segments as f32);
                let a0 = t0 * TAU;
                let a1 = t1 * TAU;
                let p0 = center + Vec2::new(a0.cos(), a0.sin()) * mid_r;
                let p1 = center + Vec2::new(a1.cos(), a1.sin()) * mid_r;
                let [r, g, b] = hsv_to_rgb(t0, 1.0, 1.0);
                painter.line_segment(
                    [p0, p1],
                    Stroke::new(ring_thickness, Color32::from_rgb(r, g, b)),
                );
            }

            let cur_rgb = self.active_zone_rgb();
            let cur_col = Color32::from_rgb(cur_rgb[0], cur_rgb[1], cur_rgb[2]);
            painter.circle_filled(center, inner_r - 8.0, cur_col);
            painter.circle_stroke(
                center,
                inner_r - 8.0,
                Stroke::new(1.5_f32, BG_CONTROL_SLATE),
            );

            let sel_angle = self.lighting_hsv[0] * TAU;
            let thumb_pos = center + Vec2::new(sel_angle.cos(), sel_angle.sin()) * mid_r;
            painter.circle_filled(thumb_pos, ring_thickness * 0.55, cur_col);
            painter.circle_stroke(
                thumb_pos,
                ring_thickness * 0.55,
                Stroke::new(2.0_f32, TEXT_WHITE),
            );

            ui.add_space(10.0);

            ui.vertical(|ui| {
                ui.add_space(14.0);
                let mut rgb = self.active_zone_rgb();
                let mut changed = false;
                for (chan_idx, label) in [(0usize, "R"), (1usize, "G"), (2usize, "B")] {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(label)
                                .size(12.0)
                                .strong()
                                .color(TEXT_MUTED_GRAY),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_sized(
                                    [64.0, 24.0],
                                    egui::DragValue::new(&mut rgb[chan_idx]).range(0..=255),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
                if changed {
                    self.set_active_zone_rgb(rgb);
                }
            });
        });
    }

    fn draw_brightness_gradient_slider(&mut self, ui: &mut egui::Ui) {
        let desired = Vec2::new(ui.available_width(), 18.0);
        let (rect, resp) = ui.allocate_exact_size(desired, Sense::click_and_drag());

        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.05, 1.0);
            self.lighting_hsv[2] = t;
            let rgb = hsv_to_rgb(
                self.lighting_hsv[0],
                self.lighting_hsv[1],
                self.lighting_hsv[2],
            );
            self.set_active_zone_rgb(rgb);
        }

        let painter = ui.painter();
        let steps = 28;
        let step_w = rect.width() / (steps as f32);
        let track_h = 8.0;
        let track_y = rect.center().y;

        for i in 0..steps {
            let v = (i as f32) / ((steps - 1) as f32);
            let [r, g, b] = hsv_to_rgb(self.lighting_hsv[0], self.lighting_hsv[1], v);
            let seg_rect = Rect::from_min_max(
                Pos2::new(rect.left() + (i as f32) * step_w, track_y - track_h * 0.5),
                Pos2::new(
                    rect.left() + ((i + 1) as f32) * step_w + 0.5,
                    track_y + track_h * 0.5,
                ),
            );
            painter.rect_filled(seg_rect, CornerRadius::same(2), Color32::from_rgb(r, g, b));
        }

        let handle_x = rect.left() + self.lighting_hsv[2].clamp(0.0, 1.0) * rect.width();
        let handle_center = Pos2::new(handle_x, track_y);
        painter.circle_filled(handle_center, 6.5, CYAN_ACCENT);
        painter.circle_stroke(handle_center, 6.5, Stroke::new(1.8_f32, TEXT_WHITE));
    }

    fn draw_preset_color_swatches(&mut self, ui: &mut egui::Ui) {
        let current = self.active_zone_rgb();
        for row in PRESET_SWATCHES.chunks(6) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                for &swatch in row {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                    if resp.clicked() {
                        self.set_active_zone_rgb(swatch);
                        if self.hw_state.config.light_mode == LightMode::Off {
                            self.hw_state.config.light_mode = LightMode::Neon;
                        }
                    }
                    let col = Color32::from_rgb(swatch[0], swatch[1], swatch[2]);
                    ui.painter().circle_filled(rect.center(), 10.0, col);
                    if swatch == current {
                        ui.painter().circle_stroke(
                            rect.center(),
                            12.5,
                            Stroke::new(1.8_f32, TEXT_WHITE),
                        );
                    } else if resp.hovered() {
                        ui.painter().circle_stroke(
                            rect.center(),
                            12.0,
                            Stroke::new(1.0_f32, TEXT_MUTED_GRAY),
                        );
                    }
                }
            });
            ui.add_space(4.0);
        }
    }

    fn draw_sensitivity_card_contents(&mut self, ui: &mut egui::Ui) {
        match self.sensitivity_sub_tab {
            SensitivitySubTab::Dpi => self.draw_sensitivity_dpi_tab(ui),
            SensitivitySubTab::Polling => self.draw_sensitivity_polling_tab(ui),
        }
    }

    fn draw_sensitivity_dpi_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("DPI STAGES")
                    .size(11.0)
                    .strong()
                    .color(TEXT_MUTED_GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut count = self.hw_state.config.dpi_count;
                if ui
                    .add(
                        egui::DragValue::new(&mut count)
                            .range(1..=6)
                            .prefix("COUNT: "),
                    )
                    .changed()
                {
                    self.hw_state.config.dpi_count = count;
                    if self.hw_state.config.active_dpi_stage >= count {
                        self.hw_state.config.active_dpi_stage = count - 1;
                    }
                }
            });
        });
        ui.add_space(6.0);

        for idx in 0..usize::from(self.hw_state.config.dpi_count) {
            self.draw_dpi_stage_bar(ui, idx);
            ui.add_space(6.0);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    [138.0, 30.0],
                    egui::Button::new(
                        RichText::new("FACTORY DEFAULT")
                            .size(11.0)
                            .strong()
                            .color(TEXT_MUTED_GRAY),
                    )
                    .fill(BG_CONTROL_SLATE),
                )
                .clicked()
            {
                self.hw_state.config = MouseConfig::factory_default();
                self.trigger_flash_config();
            }

            if ghub_action_button(ui, "APPLY DPI") {
                self.trigger_flash_config();
            }
        });
    }

    fn draw_dpi_stage_bar(&mut self, ui: &mut egui::Ui, idx: usize) {
        let is_active = usize::from(self.hw_state.config.active_dpi_stage) == idx;
        let stage_rgb = self.stage_colors[idx];
        let stage_col = if is_active {
            CYAN_ACCENT
        } else {
            Color32::from_rgb(stage_rgb[0], stage_rgb[1], stage_rgb[2])
        };

        ui.horizontal(|ui| {
            let (pip_rect, pip_resp) =
                ui.allocate_exact_size(Vec2::new(56.0, 22.0), Sense::click());
            if pip_resp.clicked() {
                self.hw_state.config.active_dpi_stage = idx as u8;
                self.primary_rgb = self.stage_colors[idx];
                self.lighting_hsv = rgb_to_hsv(self.primary_rgb);
            }
            let pip_center = Pos2::new(pip_rect.left() + 8.0, pip_rect.center().y);
            paint_diamond(
                ui.painter(),
                pip_center,
                5.5,
                if is_active { CYAN_ACCENT } else { stage_col },
            );
            ui.painter().text(
                Pos2::new(pip_rect.left() + 19.0, pip_rect.center().y),
                Align2::LEFT_CENTER,
                format!("S{}", idx + 1),
                FontId::proportional(11.5),
                if is_active {
                    TEXT_WHITE
                } else {
                    TEXT_MUTED_GRAY
                },
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut val = self.hw_state.config.dpi_stages[idx];
                if ui
                    .add_sized(
                        [78.0, 22.0],
                        egui::DragValue::new(&mut val)
                            .range(PAW3311_DPI_MIN..=PAW3311_DPI_MAX)
                            .speed(f64::from(PAW3311_DPI_STEP))
                            .suffix(" DPI"),
                    )
                    .changed()
                {
                    let snapped =
                        ((val + PAW3311_DPI_STEP / 2) / PAW3311_DPI_STEP) * PAW3311_DPI_STEP;
                    self.hw_state.config.dpi_stages[idx] =
                        snapped.clamp(PAW3311_DPI_MIN, PAW3311_DPI_MAX);
                    self.hw_state.config.active_dpi_stage = idx as u8;
                }
                if ui
                    .color_edit_button_srgb(&mut self.stage_colors[idx])
                    .changed()
                    && is_active
                {
                    self.primary_rgb = self.stage_colors[idx];
                    self.lighting_hsv = rgb_to_hsv(self.primary_rgb);
                }
            });
        });

        let bar_w = ui.available_width();
        let (bar_rect, bar_resp) =
            ui.allocate_exact_size(Vec2::new(bar_w, 12.0), Sense::click_and_drag());
        if (bar_resp.clicked() || bar_resp.dragged())
            && let Some(pos) = bar_resp.interact_pointer_pos()
        {
            let norm = ((pos.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
            let span = f32::from(PAW3311_DPI_MAX - PAW3311_DPI_MIN);
            let raw_dpi = f32::from(PAW3311_DPI_MIN) + norm * span;
            let snapped =
                ((raw_dpi as u16 + PAW3311_DPI_STEP / 2) / PAW3311_DPI_STEP) * PAW3311_DPI_STEP;
            self.hw_state.config.dpi_stages[idx] = snapped.clamp(PAW3311_DPI_MIN, PAW3311_DPI_MAX);
            self.hw_state.config.active_dpi_stage = idx as u8;
        }
        if bar_resp.drag_stopped() {
            self.trigger_flash_config();
        }

        let painter = ui.painter();
        let track = Rect::from_center_size(bar_rect.center(), Vec2::new(bar_w, 4.0));
        painter.rect_filled(track, CornerRadius::same(2), BG_CONTROL_SLATE);

        let frac = f32::from(self.hw_state.config.dpi_stages[idx].saturating_sub(PAW3311_DPI_MIN))
            / f32::from(PAW3311_DPI_MAX - PAW3311_DPI_MIN);
        let fill_w = (bar_w * frac.clamp(0.0, 1.0)).max(4.0);
        let fill_rect = Rect::from_min_size(track.min, Vec2::new(fill_w, 4.0));
        painter.rect_filled(
            fill_rect,
            CornerRadius::same(2),
            if is_active {
                CYAN_ACCENT
            } else {
                TEXT_MUTED_GRAY
            },
        );

        let thumb_x = (track.left() + fill_w).clamp(track.left(), track.right());
        let thumb_pos = Pos2::new(thumb_x, track.center().y);
        if is_active {
            paint_diamond(painter, thumb_pos, 5.5, CYAN_ACCENT);
            painter.circle_filled(thumb_pos, 2.0, TEXT_WHITE);
        } else {
            painter.circle_filled(thumb_pos, 4.0, TEXT_WHITE);
        }
    }

    fn draw_sensitivity_polling_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("REPORT RATE (HZ)")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for rate in PollingRate::ALL {
                let selected = self.hw_state.config.polling_rate == rate;
                if ghub_pill_button(ui, &format!("{}", rate.as_hz()), selected, 66.0) {
                    self.hw_state.config.polling_rate = rate;
                    self.trigger_flash_config();
                }
            }
        });

        ui.add_space(12.0);
        ui.label(
            RichText::new("LIFT-OFF DISTANCE (LOD)")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (lod, label) in [
                (LodHeight::Mm1, "1.0 MM (LOW)"),
                (LodHeight::Mm2, "2.0 MM (HIGH)"),
            ] {
                let selected = self.hw_state.config.lod == lod;
                if ghub_pill_button(ui, label, selected, 140.0) {
                    self.hw_state.config.lod = lod;
                    self.trigger_flash_config();
                }
            }
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SWITCH DEBOUNCE")
                    .size(11.0)
                    .strong()
                    .color(TEXT_MUTED_GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.hw_state.config.debounce_ms)
                        .range(0..=30)
                        .suffix(" MS"),
                );
            });
        });
        ui.add(egui::Slider::new(&mut self.hw_state.config.debounce_ms, 0..=30).show_value(false));

        ui.add_space(8.0);
        ui.label(
            RichText::new("PAW3311 DSP & POWER")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        ui.checkbox(
            &mut self.hw_state.config.sensor_flags.motion_sync,
            RichText::new("MOTION SYNC").size(11.5).color(TEXT_WHITE),
        );
        ui.checkbox(
            &mut self.hw_state.config.sensor_flags.angle_snapping,
            RichText::new("ANGLE SNAPPING / LINEARITY")
                .size(11.5)
                .color(TEXT_WHITE),
        );
        ui.checkbox(
            &mut self.hw_state.config.sensor_flags.ripple_control,
            RichText::new("RIPPLE CONTROL (>9000 DPI)")
                .size(11.5)
                .color(TEXT_WHITE),
        );
        ui.checkbox(
            &mut self.hw_state.config.highspeed_mode,
            RichText::new("HIGH-SPEED CORDED / RF MODE")
                .size(11.5)
                .color(TEXT_WHITE),
        );

        let mut rev_scroll = self.hw_state.config.scroll_direction == ScrollDirection::Reverse;
        if ui
            .checkbox(
                &mut rev_scroll,
                RichText::new("REVERSE SCROLL DIRECTION")
                    .size(11.5)
                    .color(TEXT_WHITE),
            )
            .changed()
        {
            self.hw_state.config.scroll_direction = if rev_scroll {
                ScrollDirection::Reverse
            } else {
                ScrollDirection::Normal
            };
        }

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SLEEP TIMEOUT")
                    .size(11.0)
                    .strong()
                    .color(TEXT_MUTED_GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("ghub_sleep_combo")
                    .width(95.0)
                    .selected_text(format!("{} MIN ⌄", self.hw_state.config.sleep_minutes))
                    .show_ui(ui, |ui| {
                        for mins in VALID_SLEEP_MINUTES {
                            ui.selectable_value(
                                &mut self.hw_state.config.sleep_minutes,
                                mins,
                                format!("{mins} MIN"),
                            );
                        }
                    });
            });
        });

        ui.add_space(10.0);
        if ghub_action_button(ui, "APPLY SENSITIVITY") {
            self.trigger_flash_config();
        }
    }

    fn draw_assignments_card_contents(&mut self, ui: &mut egui::Ui) {
        match self.assignments_sub_tab {
            AssignmentsSubTab::Buttons => self.draw_assignments_buttons_tab(ui),
            AssignmentsSubTab::Keys => self.draw_assignments_keys_tab(ui),
            AssignmentsSubTab::Macros => self.draw_assignments_macros_tab(ui),
        }
    }

    fn draw_assignments_buttons_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("ghub_buttons_scroll")
            .max_height(ui.available_height() - 48.0)
            .show(ui, |ui| {
                for slot in ButtonSlot::ALL {
                    let idx = slot as usize;
                    let is_selected = self.selected_slot == idx;
                    let current_action = self.hw_state.buttons.slots[idx];
                    let summary = describe_button_action(current_action).to_uppercase();

                    let row_bg = if is_selected {
                        BG_HOVER_SLATE
                    } else {
                        BG_CONTROL_SLATE
                    };
                    egui::Frame::NONE
                        .fill(row_bg)
                        .stroke(if is_selected {
                            Stroke::new(1.0_f32, CYAN_ACCENT)
                        } else {
                            Stroke::NONE
                        })
                        .corner_radius(CornerRadius::same(2))
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let slot_title =
                                    format!("{}. {}", idx + 1, slot.label().to_uppercase());
                                if ui
                                    .selectable_label(
                                        is_selected,
                                        RichText::new(slot_title).size(11.0).strong().color(
                                            if is_selected { CYAN_ACCENT } else { TEXT_WHITE },
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.selected_slot = idx;
                                }

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        egui::ComboBox::from_id_salt(("ghub_slot_bind", idx))
                                            .width(136.0)
                                            .selected_text(
                                                RichText::new(format!("{summary} ⌄"))
                                                    .size(10.5)
                                                    .color(TEXT_WHITE),
                                            )
                                            .show_ui(ui, |ui| {
                                                self.selected_slot = idx;
                                                for (label, act) in [
                                                    (
                                                        "LEFT CLICK",
                                                        ButtonAction::MouseClick(
                                                            MouseClickButton::Left,
                                                        ),
                                                    ),
                                                    (
                                                        "RIGHT CLICK",
                                                        ButtonAction::MouseClick(
                                                            MouseClickButton::Right,
                                                        ),
                                                    ),
                                                    (
                                                        "MIDDLE CLICK",
                                                        ButtonAction::MouseClick(
                                                            MouseClickButton::Middle,
                                                        ),
                                                    ),
                                                    (
                                                        "SIDE BACK",
                                                        ButtonAction::MouseClick(
                                                            MouseClickButton::Back,
                                                        ),
                                                    ),
                                                    (
                                                        "SIDE FORWARD",
                                                        ButtonAction::MouseClick(
                                                            MouseClickButton::Forward,
                                                        ),
                                                    ),
                                                    ("DPI LOOP", ButtonAction::DpiLoop),
                                                    ("SCROLL UP", ButtonAction::ScrollUp),
                                                    ("SCROLL DOWN", ButtonAction::ScrollDown),
                                                    ("DISABLED", ButtonAction::Disabled),
                                                ] {
                                                    if ui.selectable_label(false, label).clicked() {
                                                        self.assign_slot_with_safety(idx, act);
                                                    }
                                                }
                                                ui.separator();
                                                for media in MediaFunction::ALL {
                                                    let m_lbl = format!(
                                                        "MEDIA: {}",
                                                        media.label().to_uppercase()
                                                    );
                                                    if ui.selectable_label(false, m_lbl).clicked() {
                                                        self.assign_slot_with_safety(
                                                            idx,
                                                            ButtonAction::Media(media),
                                                        );
                                                    }
                                                }
                                            });
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }

                ui.add_space(6.0);
                let slot_enum = ButtonSlot::ALL[self.selected_slot.min(BUTTON_SLOT_COUNT - 1)];
                ui.label(
                    RichText::new(format!(
                        "QUICK BIND FOR: {}",
                        slot_enum.label().to_uppercase()
                    ))
                    .size(10.5)
                    .strong()
                    .color(CYAN_ACCENT),
                );
                ui.horizontal_wrapped(|ui| {
                    for (label, act) in [
                        (
                            "LEFT CLICK",
                            ButtonAction::MouseClick(MouseClickButton::Left),
                        ),
                        (
                            "RIGHT CLICK",
                            ButtonAction::MouseClick(MouseClickButton::Right),
                        ),
                        (
                            "MIDDLE CLICK",
                            ButtonAction::MouseClick(MouseClickButton::Middle),
                        ),
                        (
                            "SIDE BACK",
                            ButtonAction::MouseClick(MouseClickButton::Back),
                        ),
                        (
                            "SIDE FORWARD",
                            ButtonAction::MouseClick(MouseClickButton::Forward),
                        ),
                        ("DPI LOOP", ButtonAction::DpiLoop),
                        ("SCROLL UP", ButtonAction::ScrollUp),
                        ("SCROLL DOWN", ButtonAction::ScrollDown),
                        ("DISABLE", ButtonAction::Disabled),
                    ] {
                        if ui.small_button(label).clicked() {
                            self.assign_slot_with_safety(self.selected_slot, act);
                        }
                    }
                });
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    [92.0, 32.0],
                    egui::Button::new(
                        RichText::new("DEFAULT")
                            .size(11.0)
                            .strong()
                            .color(TEXT_MUTED_GRAY),
                    )
                    .fill(BG_CONTROL_SLATE),
                )
                .clicked()
            {
                self.hw_state.buttons = ButtonMapping::factory_default();
                self.trigger_flash_buttons();
            }
            if ghub_action_button(ui, "APPLY ASSIGNMENTS") {
                self.trigger_flash_buttons();
            }
        });
    }

    fn draw_assignments_keys_tab(&mut self, ui: &mut egui::Ui) {
        let slot_idx = self.selected_slot.min(BUTTON_SLOT_COUNT - 1);
        let slot_enum = ButtonSlot::ALL[slot_idx];

        ui.label(
            RichText::new(format!(
                "TARGET BUTTON: {}. {}",
                slot_idx + 1,
                slot_enum.label().to_uppercase()
            ))
            .size(11.5)
            .strong()
            .color(CYAN_ACCENT),
        );
        ui.label(
            RichText::new(format!(
                "CURRENT: {}",
                describe_button_action(self.hw_state.buttons.slots[slot_idx]).to_uppercase()
            ))
            .size(11.0)
            .color(TEXT_MUTED_GRAY),
        );

        ui.add_space(10.0);
        ui.label(
            RichText::new("MODIFIER KEYS")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        ui.horizontal(|ui| {
            for (flag, label) in [
                (&mut self.edit_sc_ctrl, "CTRL"),
                (&mut self.edit_sc_shift, "SHIFT"),
                (&mut self.edit_sc_alt, "ALT"),
                (&mut self.edit_sc_win, "WIN"),
            ] {
                if ghub_pill_button(ui, label, *flag, 64.0) {
                    *flag = !*flag;
                }
            }
        });

        ui.add_space(10.0);
        ui.label(
            RichText::new("HID KEYCODE")
                .size(11.0)
                .strong()
                .color(TEXT_MUTED_GRAY),
        );
        egui::ComboBox::from_id_salt("ghub_shortcut_key_combo")
            .width(ui.available_width())
            .selected_text(
                RichText::new(format!("{} ⌄", keycode_label(self.edit_sc_keycode)))
                    .size(12.0)
                    .strong()
                    .color(TEXT_WHITE),
            )
            .show_ui(ui, |ui| {
                for entry in HID_KEY_TABLE {
                    ui.selectable_value(&mut self.edit_sc_keycode, entry.code, entry.label);
                }
            });

        ui.add_space(12.0);
        if ghub_action_button(ui, "BIND KEYBOARD SHORTCUT") {
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
                slot_idx,
                ButtonAction::KeyboardShortcut {
                    modifiers: KeyModifiers(mask),
                    keycode: self.edit_sc_keycode,
                },
            );
            self.trigger_flash_buttons();
        }
    }

    fn draw_assignments_macros_tab(&mut self, ui: &mut egui::Ui) {
        let slot_idx = self.selected_slot.min(BUTTON_SLOT_COUNT - 1);
        ui.horizontal(|ui| {
            let max_slot = u8::try_from(self.macros.len().saturating_sub(1)).unwrap_or(0);
            ui.add(
                egui::DragValue::new(&mut self.edit_macro_slot)
                    .range(0..=max_slot)
                    .prefix("SLOT #"),
            );
            egui::ComboBox::from_id_salt("ghub_macro_mode_combo")
                .width(122.0)
                .selected_text(format!("{} ⌄", self.edit_macro_mode.label()))
                .show_ui(ui, |ui| {
                    for kind in MacroLoopKind::ALL {
                        ui.selectable_value(&mut self.edit_macro_mode, kind, kind.label());
                    }
                });
            if self.edit_macro_mode == MacroLoopKind::FixedCount {
                ui.add(
                    egui::DragValue::new(&mut self.edit_macro_repeat)
                        .range(1..=255)
                        .suffix("X"),
                );
            }
        });

        if ui
            .add_sized(
                [ui.available_width(), 28.0],
                egui::Button::new(
                    RichText::new(format!(
                        "BIND MACRO #{} TO {}",
                        self.edit_macro_slot,
                        ButtonSlot::ALL[slot_idx].label().to_uppercase()
                    ))
                    .size(11.0)
                    .strong()
                    .color(TEXT_WHITE),
                )
                .fill(BG_CONTROL_SLATE),
            )
            .clicked()
        {
            let trigger = self.edit_macro_mode.to_trigger(self.edit_macro_repeat);
            self.assign_slot_with_safety(
                slot_idx,
                ButtonAction::Macro {
                    slot: self.edit_macro_slot,
                    trigger,
                },
            );
            self.trigger_flash_buttons();
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ghub_select_macro_slot")
                .width(160.0)
                .selected_text(self.macros.get(self.selected_macro_idx).map_or_else(
                    || "NO MACRO".to_owned(),
                    |m| format!("#{} {}", self.selected_macro_idx, m.name.to_uppercase()),
                ))
                .show_ui(ui, |ui| {
                    for (idx, macro_slot) in self.macros.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.selected_macro_idx,
                            idx,
                            format!("#{idx} {}", macro_slot.name),
                        );
                    }
                });

            if ui.button("+ NEW").clicked() && self.macros.len() < MAX_MACRO_SLOTS {
                let idx = self.macros.len();
                self.macros.push(UiMacroSlot {
                    name: format!("Macro {}", idx + 1),
                    steps: Vec::new(),
                });
                self.selected_macro_idx = idx;
            }

            let used_bytes = self.estimate_macro_flash_bytes();
            ui.label(
                RichText::new(format!("{used_bytes}/{MACRO_FLASH_TOTAL_LEN}B"))
                    .size(10.5)
                    .color(TEXT_MUTED_GRAY),
            );
        });

        if let Some(mac) = self.macros.get_mut(self.selected_macro_idx) {
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut mac.name);
                if ui.small_button("CLEAR").clicked() {
                    mac.steps.clear();
                }
            });

            let mut remove_idx: Option<usize> = None;
            egui::ScrollArea::vertical()
                .id_salt("ghub_macro_steps_scroll")
                .max_height(130.0)
                .show(ui, |ui| {
                    for (idx, step) in mac.steps.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let kind_lbl = match step.input {
                                MacroInput::KeyboardKey(kc) => {
                                    format!("KEY {}", keycode_label(kc))
                                }
                                MacroInput::MouseButton(btn) => {
                                    format!("BTN {}", btn.label())
                                }
                            };
                            let dir_lbl = if step.pressed { "↓" } else { "↑" };
                            ui.monospace(format!("{:>2}.{dir_lbl} {kind_lbl:<12}", idx + 1));
                            ui.add(
                                egui::DragValue::new(&mut step.delay_ms)
                                    .range(0..=10_000)
                                    .suffix("ms"),
                            );
                            if ui.small_button("✕").clicked() {
                                remove_idx = Some(idx);
                            }
                        });
                    }
                });
            if let Some(idx) = remove_idx
                && idx < mac.steps.len()
            {
                mac.steps.remove(idx);
            }

            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.new_step_is_mouse, false, "KEY");
                ui.selectable_value(&mut self.new_step_is_mouse, true, "MOUSE");
                if self.new_step_is_mouse {
                    egui::ComboBox::from_id_salt("ghub_new_step_mouse")
                        .width(85.0)
                        .selected_text(self.new_step_mouse_btn.label())
                        .show_ui(ui, |ui| {
                            for btn in MouseClickButton::ALL {
                                ui.selectable_value(&mut self.new_step_mouse_btn, btn, btn.label());
                            }
                        });
                } else {
                    egui::ComboBox::from_id_salt("ghub_new_step_key")
                        .width(85.0)
                        .selected_text(keycode_label(self.new_step_keycode))
                        .show_ui(ui, |ui| {
                            for entry in HID_KEY_TABLE {
                                ui.selectable_value(
                                    &mut self.new_step_keycode,
                                    entry.code,
                                    entry.label,
                                );
                            }
                        });
                }
                ui.add(
                    egui::DragValue::new(&mut self.new_step_delay_ms)
                        .range(1..=5000)
                        .suffix("ms"),
                );
                if ui.button("+ TAP").clicked() {
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

        ui.add_space(6.0);
        if ghub_action_button(ui, "FLASH MACROS TO MCU") {
            self.trigger_flash_macros();
        }
    }

    fn draw_interactive_mouse_viewport(&mut self, ui: &mut egui::Ui, viewport_rect: Rect) {
        let painter = ui.painter_at(viewport_rect);
        let center = Pos2::new(viewport_rect.center().x, viewport_rect.center().y - 14.0);

        let body_w = 216.0;
        let body_h = 380.0;
        let body_rect = Rect::from_center_size(center, Vec2::new(body_w, body_h));

        let wheel_pos = Pos2::new(
            body_rect.left() + body_w * 0.503,
            body_rect.top() + body_h * 0.188,
        );
        let dpi_btn_pos = Pos2::new(
            body_rect.left() + body_w * 0.503,
            body_rect.top() + body_h * 0.373,
        );
        let palm_logo_pos = Pos2::new(
            body_rect.left() + body_w * 0.503,
            body_rect.top() + body_h * 0.700,
        );

        // Ambient underglow behind the real mouse photo when Lighting is active
        if self.hw_state.config.light_mode != LightMode::Off {
            let glow_rgb = self.primary_rgb;
            let logo_rgb = self.logo_rgb;
            let boost = if self.rail_tab == RailTab::Lighting {
                1.0
            } else {
                0.45
            };
            paint_radial_glow(&painter, wheel_pos, 92.0, glow_rgb, 0.32 * boost);
            paint_radial_glow(&painter, palm_logo_pos, 118.0, logo_rgb, 0.34 * boost);
            paint_radial_glow(
                &painter,
                Pos2::new(center.x, body_rect.bottom() - 12.0),
                96.0,
                glow_rgb,
                0.26 * boost,
            );
        }

        // Render the actual Fire Phoenix BK-R1X top-down studio photograph
        let full_uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(self.mouse_texture.id(), body_rect, full_uv, Color32::WHITE);

        // Live LED tint over the center `<<oo` DPI indicator button & wheel rim
        if self.hw_state.config.light_mode != LightMode::Off {
            let p_rgb = self.primary_rgb;
            let l_rgb = self.logo_rgb;
            paint_radial_glow(&painter, dpi_btn_pos, 22.0, p_rgb, 0.55);
            paint_radial_glow(&painter, wheel_pos, 32.0, p_rgb, 0.25);
            if self.rail_tab == RailTab::Lighting {
                paint_radial_glow(&painter, palm_logo_pos, 46.0, l_rgb, 0.30);
            }
        }

        match self.rail_tab {
            RailTab::Assignments => {
                self.draw_ghub_assignment_callouts(ui, &painter, center, body_rect);
            }
            RailTab::Sensitivity => {
                self.draw_ghub_sensitivity_overlay(ui, &painter, center, body_rect);
            }
            RailTab::Lighting => {
                self.draw_ghub_lighting_overlay(&painter, center, body_rect);
            }
            RailTab::Profiles | RailTab::Telemetry => {
                self.draw_ghub_status_overlay(&painter, center, body_rect);
            }
        }
    }

    fn draw_ghub_assignment_callouts(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        center: Pos2,
        body_rect: Rect,
    ) {
        let w = body_rect.width();
        let h = body_rect.height();
        let pt = |u: f32, v: f32| Pos2::new(body_rect.left() + w * u, body_rect.top() + h * v);

        // Exact physical button coordinates mapped onto the 300x528 BK-R1X photograph
        let callouts: [(usize, Pos2, Pos2, bool); 8] = [
            (
                0,
                pt(0.310, 0.188),
                Pos2::new(body_rect.left() - 155.0, center.y - 122.0),
                true,
            ),
            (
                4,
                pt(0.060, 0.377),
                Pos2::new(body_rect.left() - 155.0, center.y - 44.0),
                true,
            ),
            (
                3,
                pt(0.073, 0.523),
                Pos2::new(body_rect.left() - 155.0, center.y + 28.0),
                true,
            ),
            (
                6,
                pt(0.503, 0.130),
                Pos2::new(body_rect.left() - 155.0, center.y + 98.0),
                true,
            ),
            (
                1,
                pt(0.697, 0.188),
                Pos2::new(body_rect.right() + 35.0, center.y - 122.0),
                false,
            ),
            (
                2,
                pt(0.503, 0.188),
                Pos2::new(body_rect.right() + 35.0, center.y - 52.0),
                false,
            ),
            (
                5,
                pt(0.503, 0.373),
                Pos2::new(body_rect.right() + 35.0, center.y + 18.0),
                false,
            ),
            (
                7,
                pt(0.503, 0.246),
                Pos2::new(body_rect.right() + 35.0, center.y + 88.0),
                false,
            ),
        ];

        for (slot_idx, anchor, label_pos, is_left) in callouts {
            let is_selected = self.selected_slot == slot_idx;
            let badge_w = 138.0;
            let badge_h = 28.0;
            let badge_rect = Rect::from_min_size(label_pos, Vec2::new(badge_w, badge_h));
            let line_end = if is_left {
                Pos2::new(badge_rect.right(), badge_rect.center().y)
            } else {
                Pos2::new(badge_rect.left(), badge_rect.center().y)
            };
            let elbow = Pos2::new(
                if is_left {
                    line_end.x + 18.0
                } else {
                    line_end.x - 18.0
                },
                line_end.y,
            );

            let line_col = if is_selected {
                CYAN_ACCENT
            } else {
                Color32::from_rgb(0x5A, 0x62, 0x70)
            };
            painter.line_segment([anchor, elbow], Stroke::new(1.3_f32, line_col));
            painter.line_segment([elbow, line_end], Stroke::new(1.3_f32, line_col));
            painter.circle_filled(
                anchor,
                4.5,
                if is_selected {
                    CYAN_ACCENT
                } else {
                    BG_CARD_CHARCOAL
                },
            );
            painter.circle_stroke(anchor, 4.5, Stroke::new(1.5_f32, TEXT_WHITE));

            let hotspot_rect = Rect::from_center_size(anchor, Vec2::splat(22.0));
            let hotspot_resp = ui.interact(
                hotspot_rect,
                egui::Id::new(("ghub_photo_hotspot", slot_idx)),
                Sense::click(),
            );
            let resp = ui.interact(
                badge_rect,
                egui::Id::new(("ghub_callout_badge", slot_idx)),
                Sense::click(),
            );
            if resp.clicked() || hotspot_resp.clicked() {
                self.selected_slot = slot_idx;
            }

            let fill = if is_selected {
                BG_HOVER_SLATE
            } else if resp.hovered() || hotspot_resp.hovered() {
                BG_CONTROL_SLATE
            } else {
                BG_CARD_CHARCOAL
            };
            let stroke = if is_selected {
                Stroke::new(1.2_f32, CYAN_ACCENT)
            } else {
                Stroke::new(1.0_f32, BG_CONTROL_SLATE)
            };
            painter.rect(
                badge_rect,
                CornerRadius::same(2),
                fill,
                stroke,
                StrokeKind::Inside,
            );

            let act_text =
                describe_button_action(self.hw_state.buttons.slots[slot_idx]).to_uppercase();
            painter.text(
                Pos2::new(badge_rect.left() + 8.0, badge_rect.center().y),
                Align2::LEFT_CENTER,
                format!("{}. {act_text}", slot_idx + 1),
                FontId::proportional(11.0),
                if is_selected {
                    TEXT_WHITE
                } else {
                    TEXT_MUTED_GRAY
                },
            );
        }
    }

    fn draw_ghub_sensitivity_overlay(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        center: Pos2,
        body_rect: Rect,
    ) {
        let reticle = Pos2::new(center.x, body_rect.top() + body_rect.height() * 0.54);
        let stroke = Stroke::new(1.3_f32, CYAN_ACCENT);
        painter.circle_stroke(reticle, 18.0, stroke);
        painter.circle_filled(reticle, 3.0, CYAN_ACCENT);
        painter.line_segment(
            [
                Pos2::new(reticle.x - 28.0, reticle.y),
                Pos2::new(reticle.x - 10.0, reticle.y),
            ],
            stroke,
        );
        painter.line_segment(
            [
                Pos2::new(reticle.x + 10.0, reticle.y),
                Pos2::new(reticle.x + 28.0, reticle.y),
            ],
            stroke,
        );
        painter.line_segment(
            [
                Pos2::new(reticle.x, reticle.y - 28.0),
                Pos2::new(reticle.x, reticle.y - 10.0),
            ],
            stroke,
        );
        painter.line_segment(
            [
                Pos2::new(reticle.x, reticle.y + 10.0),
                Pos2::new(reticle.x, reticle.y + 28.0),
            ],
            stroke,
        );

        // Interactive DPI stage pills under the mouse photo
        let count = usize::from(self.hw_state.config.dpi_count).max(1);
        let pill_w = 64.0;
        let spacing = 6.0;
        let total_w = (count as f32) * pill_w + ((count - 1) as f32) * spacing;
        let start_x = center.x - total_w * 0.5;
        let pill_y = body_rect.bottom() + 8.0;

        for idx in 0..count {
            let is_active = usize::from(self.hw_state.config.active_dpi_stage) == idx;
            let p_rect = Rect::from_min_size(
                Pos2::new(start_x + (idx as f32) * (pill_w + spacing), pill_y),
                Vec2::new(pill_w, 22.0),
            );
            let resp = ui.interact(
                p_rect,
                egui::Id::new(("ghub_viewport_dpi_pip", idx)),
                Sense::click(),
            );
            if resp.clicked() {
                self.hw_state.config.active_dpi_stage = idx as u8;
                self.trigger_flash_config();
            }
            let stage_rgb = self.stage_colors[idx];
            let stage_col = Color32::from_rgb(stage_rgb[0], stage_rgb[1], stage_rgb[2]);
            painter.rect(
                p_rect,
                CornerRadius::same(2),
                if is_active {
                    BG_HOVER_SLATE
                } else {
                    BG_CARD_CHARCOAL
                },
                Stroke::new(
                    1.0_f32,
                    if is_active {
                        CYAN_ACCENT
                    } else {
                        BG_CONTROL_SLATE
                    },
                ),
                StrokeKind::Inside,
            );
            paint_diamond(
                painter,
                Pos2::new(p_rect.left() + 9.0, p_rect.center().y),
                4.0,
                stage_col,
            );
            painter.text(
                Pos2::new(p_rect.left() + 18.0, p_rect.center().y),
                Align2::LEFT_CENTER,
                format!("{}", self.hw_state.config.dpi_stages[idx]),
                FontId::proportional(10.5),
                if is_active {
                    TEXT_WHITE
                } else {
                    TEXT_MUTED_GRAY
                },
            );
        }

        let readout_y = body_rect.bottom() + 42.0;
        painter.text(
            Pos2::new(center.x, readout_y),
            Align2::CENTER_CENTER,
            format!(
                "PAW3311 OPTICAL · {} DPI · {} HZ · LOD {} MM · DEBOUNCE {} MS",
                self.hw_state.config.active_dpi(),
                self.hw_state.config.polling_rate.as_hz(),
                self.hw_state.config.lod.as_mm(),
                self.hw_state.config.debounce_ms
            ),
            FontId::proportional(11.5),
            CYAN_ACCENT,
        );
    }

    fn draw_ghub_lighting_overlay(&self, painter: &egui::Painter, center: Pos2, body_rect: Rect) {
        let readout_y = body_rect.bottom() + 24.0;
        let rgb = self.active_zone_rgb();
        painter.text(
            Pos2::new(center.x, readout_y),
            Align2::CENTER_CENTER,
            format!(
                "ZONE: {} · EFFECT: {} · RGB({}, {}, {})",
                self.lighting_zone.label(),
                ghub_effect_label(self.hw_state.config.light_mode),
                rgb[0],
                rgb[1],
                rgb[2]
            ),
            FontId::proportional(11.5),
            TEXT_MUTED_GRAY,
        );
    }

    fn draw_ghub_status_overlay(&self, painter: &egui::Painter, center: Pos2, body_rect: Rect) {
        let readout_y = body_rect.bottom() + 24.0;
        painter.text(
            Pos2::new(center.x, readout_y),
            Align2::CENTER_CENTER,
            format!(
                "PROFILE: {} · FW v{} · {} DPI · {} HZ",
                self.active_profile_label,
                self.hw_state.firmware,
                self.hw_state.config.active_dpi(),
                self.hw_state.config.polling_rate.as_hz()
            ),
            FontId::proportional(11.5),
            CYAN_ACCENT,
        );
    }

    pub(crate) fn draw_bottom_dock(&mut self, ui: &mut egui::Ui) {
        if self.show_raw_debugger_drawer {
            self.draw_raw_debugger_table(ui, 145.0);
            ui.add_space(4.0);
        }

        let avail = ui.available_rect_before_wrap();
        let dock_w = 248.0;
        let dock_h = 52.0;
        let dock_rect = Rect::from_center_size(avail.center(), Vec2::new(dock_w, dock_h));

        let painter = ui.painter();
        painter.rect_filled(dock_rect, CornerRadius::same(2), BG_CARD_CHARCOAL);
        let bottom_bar = Rect::from_min_max(
            Pos2::new(dock_rect.left(), dock_rect.bottom() - 2.0),
            dock_rect.max,
        );
        painter.rect_filled(bottom_bar, CornerRadius::ZERO, CYAN_ACCENT);

        // Mini real BK-R1X mouse photo thumbnail on left of dock
        let thumb_center = Pos2::new(dock_rect.left() + 26.0, dock_rect.center().y - 1.0);
        let thumb_rect = Rect::from_center_size(thumb_center, Vec2::new(22.0, 39.0));
        let full_uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(self.mouse_texture.id(), thumb_rect, full_uv, Color32::WHITE);

        painter.text(
            Pos2::new(dock_rect.left() + 50.0, dock_rect.top() + 17.0),
            Align2::LEFT_CENTER,
            "BK-R1X",
            FontId::proportional(13.0),
            TEXT_WHITE,
        );

        let conn_status = if let Some(conn) = self.hw_state.connection
            && self.connected_device_label.is_some()
        {
            let bat = self.hw_state.battery.percent;
            let icon = if self.hw_state.battery.charging {
                "⚡"
            } else {
                "🔋"
            };
            format!("{} · {icon} {bat}%", conn.as_str().to_uppercase())
        } else {
            "OFFLINE / PREVIEW MODE".to_owned()
        };

        painter.text(
            Pos2::new(dock_rect.left() + 50.0, dock_rect.top() + 34.0),
            Align2::LEFT_CENTER,
            conn_status,
            FontId::proportional(10.5),
            TEXT_MUTED_GRAY,
        );
    }

    pub(crate) fn draw_settings_modal(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings_modal;
        egui::Window::new("BK-R1X GLOBAL SETTINGS, PROFILES & TELEMETRY")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(780.0)
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    cols[0].group(|ui| self.draw_saved_profiles_card(ui));
                    cols[1].group(|ui| {
                        self.draw_startup_and_tray_card(ui);
                        ui.add_space(10.0);
                        self.draw_telemetry_summary_card(ui);
                    });
                });
            });
        self.show_settings_modal = open;
    }

    fn draw_saved_profiles_card(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("PROFILES & PER-GAME AUTO-SWITCH")
                .size(13.0)
                .strong()
                .color(TEXT_WHITE),
        );
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("NAME:");
            ui.text_edit_singleline(&mut self.new_profile_name);
        });
        ui.horizontal(|ui| {
            ui.label("PROCESS (OPTIONAL, E.G. CS2.EXE):");
            ui.text_edit_singleline(&mut self.new_profile_process);
        });

        if ghub_action_button(ui, "SAVE CURRENT PROFILE") {
            let trimmed_name = self.new_profile_name.trim();
            let display_name = if trimmed_name.is_empty() {
                "Custom Profile"
            } else {
                trimmed_name
            };
            let mut prof = HardwareProfile::from_hardware(
                display_name,
                &self.hw_state.config,
                &self.hw_state.buttons,
            );
            let proc_trimmed = self.new_profile_process.trim();
            if !proc_trimmed.is_empty() {
                prof.match_process = Some(proc_trimmed.to_owned());
            }
            let slug = sanitize_profile_slug(&prof.name);
            let dest = ensure_user_profiles_dir().join(format!("{slug}.toml"));
            match prof.save_to_file(&dest) {
                Ok(()) => {
                    let _ = send_ipc_request(&IpcRequest::ReloadProfiles);
                    self.saved_profiles = list_saved_profiles();
                    self.active_profile_label = prof.name.to_uppercase();
                    self.status_banner =
                        format!("Saved profile '{}' -> {}", prof.name, dest.display());
                }
                Err(err) => {
                    self.status_banner = format!("Failed saving profile: {err}");
                }
            }
        }

        ui.separator();

        let mut apply_prof: Option<HardwareProfile> = None;
        let mut delete_path: Option<PathBuf> = None;

        egui::ScrollArea::vertical()
            .id_salt("saved_profiles_scroll")
            .max_height(210.0)
            .show(ui, |ui| {
                for (path, prof) in &self.saved_profiles {
                    egui::Frame::NONE
                        .fill(BG_CONTROL_SLATE)
                        .inner_margin(8.0)
                        .corner_radius(CornerRadius::same(2))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(prof.name.to_uppercase())
                                            .strong()
                                            .color(TEXT_WHITE),
                                    );
                                    let sub = format!(
                                        "{} HZ · LOD {}MM · {:?}{}",
                                        prof.sensor.polling_rate_hz,
                                        prof.sensor.lod_mm,
                                        prof.sensor.dpi_stages,
                                        prof.match_process
                                            .as_deref()
                                            .map(|p| format!(" · GAME: {}", p.to_uppercase()))
                                            .unwrap_or_default()
                                    );
                                    ui.label(RichText::new(sub).size(11.0).color(TEXT_MUTED_GRAY));
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("🗑").clicked() {
                                            delete_path = Some(path.clone());
                                        }
                                        if ui.button("APPLY").clicked() {
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
            self.apply_loaded_profile(prof);
        }

        if let Some(del_path) = delete_path {
            let _ = fs::remove_file(&del_path);
            let _ = send_ipc_request(&IpcRequest::ReloadProfiles);
            self.saved_profiles = list_saved_profiles();
        }
    }

    fn draw_startup_and_tray_card(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("SYSTEM TRAY & WINDOWS STARTUP")
                .size(13.0)
                .strong()
                .color(TEXT_WHITE),
        );
        ui.add_space(6.0);

        egui::Frame::NONE
            .fill(BG_CONTROL_SLATE)
            .stroke(Stroke::new(1.0_f32, CYAN_ACCENT))
            .inner_margin(8.0)
            .corner_radius(CornerRadius::same(2))
            .show(ui, |ui| {
                let tray_txt = if self.tray_agent_running {
                    "● TRAY AGENT ACTIVE (< 2 MB RAM · 0% IDLE CPU)"
                } else {
                    "○ TRAY AGENT STOPPED"
                };
                ui.label(
                    RichText::new(tray_txt)
                        .size(11.5)
                        .strong()
                        .color(CYAN_ACCENT),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "• Left-click the BK-R1X tray icon anytime to open this configurator.\n\
                         • Right-click the tray icon to switch DPI, Polling Rate, or Profiles in 1 click.\n\
                         • Closing this window (X) frees 100% of GUI memory while keeping the tray icon running.",
                    )
                    .size(10.5)
                    .color(TEXT_MUTED_GRAY),
                );
            });

        ui.add_space(10.0);
        let mut auto = self.autostart_enabled;
        if ui
            .checkbox(
                &mut auto,
                RichText::new("LAUNCH AUTOMATICALLY AT WINDOWS LOGIN")
                    .size(11.5)
                    .color(TEXT_WHITE),
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
                Err(err) => {
                    self.status_banner = format!("Startup toggle error: {err}");
                }
            }
        }

        ui.add_space(8.0);
        ui.vertical(|ui| {
            if !self.tray_agent_running && ghub_action_button(ui, "▶ START SYSTEM TRAY AGENT") {
                self.tray_agent_running = ensure_tray_daemon_spawned();
            }
            if ghub_action_button(ui, "📦 INSTALL / UPDATE START MENU APP") {
                match install_user_application(true) {
                    Ok(summary) => {
                        self.autostart_enabled = summary.autostart_registered;
                        self.tray_agent_running = ensure_tray_daemon_spawned();
                        self.status_banner =
                            format!("Installed to {}", summary.install_dir.display());
                    }
                    Err(err) => {
                        self.status_banner = format!("Install error: {err}");
                    }
                }
            }
            if self.tray_agent_running
                && ui
                    .add_sized(
                        [ui.available_width(), 30.0],
                        egui::Button::new(
                            RichText::new("⏹ QUIT TRAY AGENT & EXIT")
                                .size(11.5)
                                .strong()
                                .color(Color32::from_rgb(0xFF, 0x52, 0x52)),
                        )
                        .fill(BG_CONTROL_SLATE),
                    )
                    .clicked()
            {
                let _ = send_ipc_request(&IpcRequest::Shutdown);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn draw_telemetry_card_contents(&mut self, ui: &mut egui::Ui) {
        match self.telemetry_sub_tab {
            TelemetrySubTab::Telemetry => {
                let dev_str = self
                    .connected_device_label
                    .as_deref()
                    .unwrap_or("PREVIEW MODE (NO 0xFF01 INTERFACE)");
                ui.label(
                    RichText::new("HARDWARE TELEMETRY")
                        .size(12.5)
                        .strong()
                        .color(TEXT_WHITE),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("ENDPOINT  : {dev_str}"))
                        .size(11.0)
                        .color(TEXT_MUTED_GRAY),
                );
                ui.label(
                    RichText::new(format!(
                        "TRANSPORT : {}",
                        if self.connected_via_daemon {
                            "SHARED IPC VIA SYSTEM TRAY AGENT"
                        } else {
                            "DIRECT HIDRAW / HID.DLL"
                        }
                    ))
                    .size(11.0)
                    .color(TEXT_MUTED_GRAY),
                );
                ui.label(
                    RichText::new(format!(
                        "TOPOLOGY  : {}",
                        self.hw_state
                            .connection
                            .map_or("OFFLINE", |mode| mode.as_str())
                    ))
                    .size(11.0)
                    .color(TEXT_MUTED_GRAY),
                );
                ui.label(
                    RichText::new(format!(
                        "MCU STATE : {}",
                        if self.hw_state.online {
                            "AWAKE / ACTIVE"
                        } else {
                            "DEEP SLEEP / OFFLINE"
                        }
                    ))
                    .size(11.0)
                    .color(TEXT_MUTED_GRAY),
                );
                ui.label(
                    RichText::new(format!("FIRMWARE  : v{}", self.hw_state.firmware))
                        .size(11.0)
                        .color(TEXT_MUTED_GRAY),
                );

                ui.add_space(10.0);
                let frac = f32::from(self.hw_state.battery.percent.min(100)) / 100.0;
                ui.add(egui::ProgressBar::new(frac).fill(CYAN_ACCENT).text(format!(
                    "500mAh LiPo Battery: {}% {}",
                    self.hw_state.battery.percent,
                    if self.hw_state.battery.charging {
                        "(Charging via USB-C)"
                    } else {
                        ""
                    }
                )));

                ui.add_space(12.0);
                let toggle_lbl = if self.show_raw_debugger_drawer {
                    "HIDE RAW HID WIRE DRAWER"
                } else {
                    "OPEN RAW HID WIRE DRAWER (64B)"
                };
                if ghub_action_button(ui, toggle_lbl) {
                    self.show_raw_debugger_drawer = !self.show_raw_debugger_drawer;
                }
            }
            TelemetrySubTab::HexVerifier => {
                ui.label(
                    RichText::new("INTERACTIVE HEX PACKET VERIFIER")
                        .size(12.0)
                        .strong()
                        .color(TEXT_WHITE),
                );
                ui.label(
                    RichText::new("Paste any 64B/65B WebHID or USBPcap hex frame to decode:")
                        .size(11.0)
                        .color(TEXT_MUTED_GRAY),
                );
                ui.add_space(4.0);
                ui.text_edit_singleline(&mut self.custom_hex_input);
                ui.add_space(6.0);
                if ghub_action_button(ui, "DECODE & VERIFY FRAME") {
                    self.custom_hex_decoded = verify_hex_string(&self.custom_hex_input);
                }
                if !self.custom_hex_decoded.is_empty() {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("ghub_hex_decoded_scroll")
                        .max_height(210.0)
                        .show(ui, |ui| {
                            ui.monospace(&self.custom_hex_decoded);
                        });
                }
            }
        }
    }

    fn draw_telemetry_summary_card(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("DEVICE TELEMETRY & WIRE INSPECTOR")
                .size(13.0)
                .strong()
                .color(TEXT_WHITE),
        );
        let dev_str = self
            .connected_device_label
            .as_deref()
            .unwrap_or("PREVIEW MODE (NO 0xFF01 INTERFACE)");
        ui.label(format!("ENDPOINT  : {dev_str}"));
        ui.label(format!(
            "TRANSPORT : {}",
            if self.connected_via_daemon {
                "TRAY AGENT IPC"
            } else {
                "DIRECT HID"
            }
        ));
        ui.label(format!("FIRMWARE  : v{}", self.hw_state.firmware));

        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.show_raw_debugger_drawer, "RAW HID DRAWER");
            if ui.button("DECODE HEX").clicked() {
                self.custom_hex_decoded = verify_hex_string(&self.custom_hex_input);
            }
        });
        ui.text_edit_singleline(&mut self.custom_hex_input);
        if !self.custom_hex_decoded.is_empty() {
            ui.monospace(&self.custom_hex_decoded);
        }
    }

    pub(crate) fn draw_raw_debugger_table(&mut self, ui: &mut egui::Ui, max_h: f32) {
        egui::Frame::NONE
            .fill(BG_CARD_CHARCOAL)
            .inner_margin(8.0)
            .corner_radius(CornerRadius::same(2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("RAW HID WIRE DEBUGGER (64-BYTE VENDOR REPORTS)")
                            .size(11.5)
                            .strong()
                            .color(CYAN_ACCENT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("CLEAR").clicked() {
                            self.trace_entries.clear();
                        }
                        ui.label(format!("{} FRAMES", self.trace_entries.len()));
                    });
                });

                egui::ScrollArea::vertical()
                    .id_salt("raw_hid_trace_scroll")
                    .max_height(max_h)
                    .show(ui, |ui| {
                        for entry in self.trace_entries.iter().rev() {
                            let (badge, col) = match entry.direction {
                                TraceDirection::Out => ("OUT", CYAN_ACCENT),
                                TraceDirection::In => ("IN ", TEXT_WHITE),
                            };
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("[{badge}]")).monospace().color(col),
                                );
                                ui.label(
                                    RichText::new(format!("rpt=0x{:02X}", entry.report_id))
                                        .monospace(),
                                );
                                ui.label(
                                    RichText::new(&entry.summary)
                                        .monospace()
                                        .color(TEXT_MUTED_GRAY),
                                );
                                if ui.small_button("COPY").clicked() {
                                    ui.ctx().copy_text(entry.payload_hex.clone());
                                }
                            });
                            ui.monospace(&entry.payload_hex);
                        }
                    });
            });
    }
}

fn paint_radial_glow(
    painter: &egui::Painter,
    center: Pos2,
    max_radius: f32,
    rgb: [u8; 3],
    peak_alpha: f32,
) {
    let rings = 14;
    for i in (1..=rings).rev() {
        let frac = (i as f32) / (rings as f32);
        let r = max_radius * frac;
        let alpha = ((1.0 - frac * 0.85) * peak_alpha * 55.0).round() as u8;
        let col = Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], alpha);
        painter.circle_filled(center, r, col);
    }
}

fn paint_diamond(painter: &egui::Painter, center: Pos2, half_size: f32, color: Color32) {
    let pts = vec![
        Pos2::new(center.x, center.y - half_size),
        Pos2::new(center.x + half_size, center.y),
        Pos2::new(center.x, center.y + half_size),
        Pos2::new(center.x - half_size, center.y),
    ];
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

fn paint_sunburst_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    painter.circle_stroke(center, 4.5, Stroke::new(1.8_f32, color));
    for i in 0..8 {
        let angle = (i as f32) * (TAU / 8.0);
        let dir = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment(
            [center + dir * 7.0, center + dir * 10.5],
            Stroke::new(1.6_f32, color),
        );
    }
}

fn paint_assignments_plus_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let box_rect = Rect::from_center_size(center, Vec2::splat(18.0));
    painter.rect_stroke(
        box_rect,
        CornerRadius::same(2),
        Stroke::new(1.6_f32, color),
        StrokeKind::Inside,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - 4.5, center.y),
            Pos2::new(center.x + 4.5, center.y),
        ],
        Stroke::new(1.8_f32, color),
    );
    painter.line_segment(
        [
            Pos2::new(center.x, center.y - 4.5),
            Pos2::new(center.x, center.y + 4.5),
        ],
        Stroke::new(1.8_f32, color),
    );
}

fn paint_sensitivity_crosshair_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    painter.line_segment(
        [
            Pos2::new(center.x - 9.5, center.y),
            Pos2::new(center.x + 9.5, center.y),
        ],
        s,
    );
    painter.line_segment(
        [
            Pos2::new(center.x, center.y - 9.5),
            Pos2::new(center.x, center.y + 9.5),
        ],
        s,
    );
    painter.circle_stroke(center, 5.5, s);
}

fn paint_profiles_sliders_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    for (dy, knob_dx) in [
        (-6.0_f32, -3.5_f32),
        (0.0_f32, 3.5_f32),
        (6.0_f32, -1.5_f32),
    ] {
        let y = center.y + dy;
        painter.line_segment(
            [Pos2::new(center.x - 8.5, y), Pos2::new(center.x + 8.5, y)],
            s,
        );
        painter.circle_filled(Pos2::new(center.x + knob_dx, y), 2.4, color);
    }
}

fn paint_telemetry_chip_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    let chip_rect = Rect::from_center_size(center, Vec2::splat(11.0));
    painter.rect_stroke(chip_rect, CornerRadius::same(2), s, StrokeKind::Inside);
    for offset in [-3.0_f32, 3.0_f32] {
        painter.line_segment(
            [
                Pos2::new(center.x + offset, chip_rect.top() - 3.5),
                Pos2::new(center.x + offset, chip_rect.top()),
            ],
            s,
        );
        painter.line_segment(
            [
                Pos2::new(center.x + offset, chip_rect.bottom()),
                Pos2::new(center.x + offset, chip_rect.bottom() + 3.5),
            ],
            s,
        );
        painter.line_segment(
            [
                Pos2::new(chip_rect.left() - 3.5, center.y + offset),
                Pos2::new(chip_rect.left(), center.y + offset),
            ],
            s,
        );
        painter.line_segment(
            [
                Pos2::new(chip_rect.right(), center.y + offset),
                Pos2::new(chip_rect.right() + 3.5, center.y + offset),
            ],
            s,
        );
    }
}

fn paint_home_back_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let s = Stroke::new(1.8_f32, color);
    painter.line_segment(
        [
            Pos2::new(center.x + 3.0, center.y - 6.0),
            Pos2::new(center.x - 4.0, center.y),
        ],
        s,
    );
    painter.line_segment(
        [
            Pos2::new(center.x - 4.0, center.y),
            Pos2::new(center.x + 3.0, center.y + 6.0),
        ],
        s,
    );
}

fn paint_gear_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    painter.circle_stroke(center, 5.0, Stroke::new(1.8_f32, color));
    for i in 0..6 {
        let angle = (i as f32) * (TAU / 6.0);
        let dir = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment(
            [center + dir * 5.8, center + dir * 8.5],
            Stroke::new(2.0_f32, color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_profile_slug_collapses_symbols_and_guards_empty_names() {
        assert_eq!(
            sanitize_profile_slug("  My Custom!! Profile?? "),
            "my-custom-profile"
        );
        assert_eq!(sanitize_profile_slug("CS2 1000Hz"), "cs2-1000hz");
        assert_eq!(sanitize_profile_slug("!!!---"), "custom-profile");
        assert_eq!(sanitize_profile_slug(""), "custom-profile");
    }

    #[test]
    fn hsv_and_rgb_roundtrip_cyan_accent() {
        let cyan = [0x00, 0xB8, 0xFC];
        let [h, s, v] = rgb_to_hsv(cyan);
        let round = hsv_to_rgb(h, s, v);
        assert!((i16::from(round[0]) - i16::from(cyan[0])).abs() <= 1);
        assert!((i16::from(round[1]) - i16::from(cyan[1])).abs() <= 1);
        assert!((i16::from(round[2]) - i16::from(cyan[2])).abs() <= 1);
    }

    #[test]
    fn embedded_mouse_photo_decompresses_to_exact_rgba_dimensions() {
        use std::io::Read;
        let expected = crate::MOUSE_PHOTO_WIDTH * crate::MOUSE_PHOTO_HEIGHT * 4;
        let mut rgba = Vec::with_capacity(expected);
        let mut decoder = flate2::read::DeflateDecoder::new(crate::MOUSE_PHOTO_DEFLATE);
        decoder
            .read_to_end(&mut rgba)
            .expect("valid deflate stream");
        assert_eq!(rgba.len(), expected);
    }
}
