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
    self, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};

use crate::keycodes::{HID_KEY_TABLE, keycode_label};
use crate::worker::{UiMacroSlot, UiTraceRow, WorkerCommand};
use crate::{ActiveTab, Bkr1xGuiApp, MAX_UI_TRACE_LOG, ensure_tray_daemon_spawned};

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
            Self::FixedCount => "Fixed Repeat",
            Self::HoldToLoop => "Hold to Loop",
            Self::ToggleLoop => "Toggle Loop",
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

impl Bkr1xGuiApp {
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
                    "Updated {} -> {} (click Flash Button Map to commit).",
                    ButtonSlot::ALL[slot].label(),
                    describe_button_action(action)
                );
            }
            Err(err) => {
                self.status_banner = format!("Safety guard: {err}");
            }
        }
    }

    pub(crate) fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
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
                    let prefix = if self.hw_state.battery.charging {
                        "⚡ "
                    } else {
                        "🔋 "
                    };
                    format!("{prefix}{pct}%")
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

    pub(crate) fn draw_performance_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| self.draw_dpi_stages_card(ui));
                cols[1].group(|ui| self.draw_sensor_and_power_card(ui));
            });
        });
    }

    fn draw_dpi_stages_card(&mut self, ui: &mut egui::Ui) {
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

        for idx in 0..usize::from(self.hw_state.config.dpi_count) {
            let is_active = usize::from(self.hw_state.config.active_dpi_stage) == idx;
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
                                egui::Slider::new(&mut val, PAW3311_DPI_MIN..=PAW3311_DPI_MAX)
                                    .step_by(f64::from(PAW3311_DPI_STEP))
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
    }

    fn draw_sensor_and_power_card(&mut self, ui: &mut egui::Ui) {
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
        ui.add(egui::Slider::new(&mut self.hw_state.config.debounce_ms, 0..=30).suffix(" ms"));

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
                    for mins in VALID_SLEEP_MINUTES {
                        ui.selectable_value(
                            &mut self.hw_state.config.sleep_minutes,
                            mins,
                            format!("{mins} min"),
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
    }

    pub(crate) fn draw_buttons_and_macros_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| self.draw_button_remapper_card(ui));
                cols[1].group(|ui| self.draw_macro_builder_card(ui));
            });
        });
    }

    fn draw_button_remapper_card(&mut self, ui: &mut egui::Ui) {
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
        let slot_enum = ButtonSlot::ALL[self.selected_slot.min(BUTTON_SLOT_COUNT - 1)];
        ui.label(
            RichText::new(format!("Assign Action for: {}", slot_enum.label()))
                .strong()
                .color(Color32::from_rgb(125, 207, 255)),
        );

        ui.horizontal_wrapped(|ui| {
            for (label, act) in [
                (
                    "Left Click",
                    ButtonAction::MouseClick(MouseClickButton::Left),
                ),
                (
                    "Right Click",
                    ButtonAction::MouseClick(MouseClickButton::Right),
                ),
                (
                    "Middle Click",
                    ButtonAction::MouseClick(MouseClickButton::Middle),
                ),
                (
                    "Side Back",
                    ButtonAction::MouseClick(MouseClickButton::Back),
                ),
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
                    for media in MediaFunction::ALL {
                        if ui.selectable_label(false, media.label()).clicked() {
                            self.assign_slot_with_safety(
                                self.selected_slot,
                                ButtonAction::Media(media),
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
                        ui.selectable_value(&mut self.edit_sc_keycode, entry.code, entry.label);
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
            let max_slot = u8::try_from(self.macros.len().saturating_sub(1)).unwrap_or(0);
            ui.add(
                egui::DragValue::new(&mut self.edit_macro_slot)
                    .range(0..=max_slot)
                    .prefix("Slot #"),
            );
            egui::ComboBox::from_id_salt("macro_mode_combo")
                .selected_text(self.edit_macro_mode.label())
                .show_ui(ui, |ui| {
                    for kind in MacroLoopKind::ALL {
                        ui.selectable_value(&mut self.edit_macro_mode, kind, kind.label());
                    }
                });
            if self.edit_macro_mode == MacroLoopKind::FixedCount {
                ui.add(
                    egui::DragValue::new(&mut self.edit_macro_repeat)
                        .range(1..=255)
                        .suffix("x"),
                );
            }
            if ui.button("Bind Macro").clicked() {
                let trigger = self.edit_macro_mode.to_trigger(self.edit_macro_repeat);
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
    }

    fn draw_macro_builder_card(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Hardware Macro Sequence Builder");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let used_bytes = self.estimate_macro_flash_bytes();
                let color = if used_bytes <= MACRO_FLASH_TOTAL_LEN {
                    Color32::from_rgb(187, 154, 247)
                } else {
                    Color32::from_rgb(247, 118, 142)
                };
                ui.label(
                    RichText::new(format!("Flash: {used_bytes} / {MACRO_FLASH_TOTAL_LEN} B"))
                        .color(color),
                );
            });
        });
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("select_macro_slot")
                .selected_text(self.macros.get(self.selected_macro_idx).map_or_else(
                    || "No Macro".to_owned(),
                    |m| format!("#{} {}", self.selected_macro_idx, m.name),
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

            if ui.button("+ New Macro").clicked() && self.macros.len() < MAX_MACRO_SLOTS {
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
                                MacroInput::MouseButton(btn) => {
                                    format!("Mouse [{}]", btn.label())
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
            if let Some(idx) = remove_idx
                && idx < mac.steps.len()
            {
                mac.steps.remove(idx);
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
                            for btn in MouseClickButton::ALL {
                                ui.selectable_value(&mut self.new_step_mouse_btn, btn, btn.label());
                            }
                        });
                } else {
                    egui::ComboBox::from_id_salt("new_step_key_combo")
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
    }

    pub(crate) fn draw_profiles_and_startup_panel(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |cols| {
            cols[0].group(|ui| self.draw_saved_profiles_card(ui));
            cols[1].group(|ui| self.draw_startup_and_tray_card(ui));
        });
    }

    fn draw_saved_profiles_card(&mut self, ui: &mut egui::Ui) {
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
                    self.status_banner =
                        format!("Saved profile '{}' -> {}", prof.name, dest.display());
                }
                Err(err) => {
                    self.status_banner = format!("Failed saving profile: {err}");
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
            if prof.buttons.is_some()
                && let Ok(btns) = prof.to_button_mapping()
            {
                self.hw_state.buttons = btns;
            }
            self.new_profile_name.clone_from(&prof.name);
            self.new_profile_process = prof.match_process.clone().unwrap_or_default();
            let _ = self
                .worker_tx
                .send(WorkerCommand::ApplySavedProfile(Box::new(prof)));
        }

        if let Some(del_path) = delete_path {
            let _ = fs::remove_file(&del_path);
            let _ = send_ipc_request(&IpcRequest::ReloadProfiles);
            self.saved_profiles = list_saved_profiles();
        }
    }

    fn draw_startup_and_tray_card(&mut self, ui: &mut egui::Ui) {
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
                Err(err) => {
                    self.status_banner = format!("Startup toggle error: {err}");
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
                    Err(err) => {
                        self.status_banner = format!("Install error: {err}");
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
    }

    fn draw_mouse_diagram(&mut self, ui: &mut egui::Ui) {
        let desired = Vec2::new(ui.available_width().min(360.0), 145.0);
        let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 6.0, Color32::from_rgb(18, 20, 28));
        let center = rect.center();

        let body = Rect::from_center_size(center, Vec2::new(92.0, 128.0));
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
            let btn_rect = Rect::from_center_size(center + offset, Vec2::new(32.0, 20.0));
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

    pub(crate) fn draw_telemetry_and_inspector_panel(&mut self, ui: &mut egui::Ui) {
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
                        .map_or("Offline", |mode| mode.as_str())
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
                ui.add(egui::ProgressBar::new(frac).text(format!(
                    "500mAh LiPo Battery: {}% {}",
                    self.hw_state.battery.percent,
                    if self.hw_state.battery.charging {
                        "(Charging via USB-C)"
                    } else {
                        ""
                    }
                )));
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

    pub(crate) fn draw_raw_debugger_table(&mut self, ui: &mut egui::Ui, max_h: f32) {
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
                                ui.ctx().copy_text(entry.payload_hex.clone());
                            }
                        });
                        ui.monospace(&entry.payload_hex);
                        ui.separator();
                    }
                });
        });
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
}
