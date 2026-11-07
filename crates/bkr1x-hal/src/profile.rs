//! Human-readable TOML / JSON hardware profile serialization and validation.

use std::fs;
use std::path::Path;

use bkr1x_protocol::{
    ButtonAction, ButtonMapping, KeyModifiers, LightMode, LodHeight, MacroInput, MacroStep,
    MacroTrigger, MediaFunction, MouseClickButton, MouseConfig, PollingRate, ScrollDirection,
    SensorFlags,
};
use serde::{Deserialize, Serialize};

use crate::error::DeviceError;

/// Serializable hardware profile loadable from `.toml` or `.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    #[serde(default = "default_profile_name")]
    pub name: String,
    #[serde(default)]
    pub match_process: Option<String>,
    pub sensor: SensorSection,
    #[serde(default)]
    pub power_lighting: PowerLightingSection,
    #[serde(default)]
    pub buttons: Option<ButtonsSection>,
    #[serde(default)]
    pub macros: Vec<MacroProfileEntry>,
}

fn default_profile_name() -> String {
    "Default Profile".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorSection {
    pub dpi_stages: Vec<u16>,
    /// 1-based active DPI stage (`1..=6`).
    pub active_stage: u8,
    pub polling_rate_hz: u16,
    pub lod_mm: u8,
    pub debounce_ms: u8,
    pub motion_sync: bool,
    pub angle_snapping: bool,
    pub ripple_control: bool,
    #[serde(default)]
    pub reverse_scroll: bool,
    #[serde(default)]
    pub highspeed_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerLightingSection {
    pub sleep_minutes: u8,
    pub light_mode: String,
    #[serde(default = "bool_true")]
    pub wakeup_on_move: bool,
    #[serde(default = "bool_true")]
    pub move_lights_off: bool,
}

fn bool_true() -> bool {
    true
}

impl Default for PowerLightingSection {
    fn default() -> Self {
        Self {
            sleep_minutes: 10,
            light_mode: "neon".to_owned(),
            wakeup_on_move: true,
            move_lights_off: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonsSection {
    pub left: ButtonBindingSpec,
    pub right: ButtonBindingSpec,
    pub middle: ButtonBindingSpec,
    pub back: ButtonBindingSpec,
    pub forward: ButtonBindingSpec,
    pub dpi_loop: ButtonBindingSpec,
    #[serde(default = "default_scroll_up")]
    pub scroll_up: ButtonBindingSpec,
    #[serde(default = "default_scroll_down")]
    pub scroll_down: ButtonBindingSpec,
}

fn default_scroll_up() -> ButtonBindingSpec {
    ButtonBindingSpec::Simple("scroll_up".to_owned())
}

fn default_scroll_down() -> ButtonBindingSpec {
    ButtonBindingSpec::Simple("scroll_down".to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ButtonBindingSpec {
    /// Shorthand string such as `"left"`, `"right"`, `"middle"`, `"back"`, `"forward"`,
    /// `"dpi_loop"`, `"scroll_up"`, `"scroll_down"`, `"disabled"`, `"media:play_pause"`, `"media:vol_up"`.
    Simple(String),
    /// Structured keyboard shortcut or hardware macro trigger.
    Structured {
        kind: String,
        #[serde(default)]
        modifiers: Vec<String>,
        #[serde(default)]
        keycode: Option<u8>,
        #[serde(default)]
        macro_slot: Option<u8>,
        #[serde(default)]
        repeat: Option<u8>,
        #[serde(default)]
        loop_mode: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroProfileEntry {
    pub name: String,
    pub steps: Vec<MacroStepSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroStepSpec {
    /// `"key"` or `"mouse"`.
    pub kind: String,
    /// USB HID keycode (e.g. `4` for `A`) or mouse button (`1=L, 2=R, 4=M, 8=Back, 16=Fwd`).
    pub code: u8,
    pub pressed: bool,
    pub delay_ms: u16,
}

impl HardwareProfile {
    /// Loads a `.toml` or `.json` profile from disk and validates all PAW3311 constraints.
    pub fn load_from_file(path: &Path) -> Result<Self, DeviceError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| DeviceError::Profile(format!("failed reading {}: {e}", path.display())))?;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("toml")
            .to_ascii_lowercase();

        let profile: Self = if ext == "json" {
            serde_json::from_str(&raw)
                .map_err(|e| DeviceError::Profile(format!("invalid JSON profile: {e}")))?
        } else {
            toml::from_str(&raw)
                .map_err(|e| DeviceError::Profile(format!("invalid TOML profile: {e}")))?
        };
        let _ = profile.to_mouse_config()?;
        let _ = profile.to_button_mapping()?;
        Ok(profile)
    }

    /// Saves this profile to `.toml` or `.json` based on file extension.
    pub fn save_to_file(&self, path: &Path) -> Result<(), DeviceError> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("toml")
            .to_ascii_lowercase();

        let text = if ext == "json" {
            serde_json::to_string_pretty(self)
                .map_err(|e| DeviceError::Profile(format!("JSON serialize error: {e}")))?
        } else {
            toml::to_string_pretty(self)
                .map_err(|e| DeviceError::Profile(format!("TOML serialize error: {e}")))?
        };
        fs::write(path, text)
            .map_err(|e| DeviceError::Profile(format!("failed writing {}: {e}", path.display())))
    }

    /// Builds a [`HardwareProfile`] from live MCU state.
    pub fn from_hardware(
        name: impl Into<String>,
        cfg: &MouseConfig,
        buttons: &ButtonMapping,
    ) -> Self {
        let stages = cfg.dpi_stages[..cfg.dpi_count as usize].to_vec();
        Self {
            name: name.into(),
            match_process: None,
            sensor: SensorSection {
                dpi_stages: stages,
                active_stage: cfg.active_dpi_stage + 1,
                polling_rate_hz: cfg.polling_rate.as_hz(),
                lod_mm: cfg.lod.as_mm(),
                debounce_ms: cfg.debounce_ms,
                motion_sync: cfg.sensor_flags.motion_sync,
                angle_snapping: cfg.sensor_flags.angle_snapping,
                ripple_control: cfg.sensor_flags.ripple_control,
                reverse_scroll: cfg.scroll_direction == ScrollDirection::Reverse,
                highspeed_mode: cfg.highspeed_mode,
            },
            power_lighting: PowerLightingSection {
                sleep_minutes: cfg.sleep_minutes,
                light_mode: format_light_mode(cfg.light_mode).to_owned(),
                wakeup_on_move: cfg.wakeup_on_move,
                move_lights_off: cfg.move_lights_off,
            },
            buttons: Some(ButtonsSection {
                left: spec_from_action(buttons.slots[0]),
                right: spec_from_action(buttons.slots[1]),
                middle: spec_from_action(buttons.slots[2]),
                back: spec_from_action(buttons.slots[3]),
                forward: spec_from_action(buttons.slots[4]),
                dpi_loop: spec_from_action(buttons.slots[5]),
                scroll_up: spec_from_action(buttons.slots[6]),
                scroll_down: spec_from_action(buttons.slots[7]),
            }),
            macros: Vec::new(),
        }
    }

    /// Converts and strictly validates the PAW3311 [`MouseConfig`].
    pub fn to_mouse_config(&self) -> Result<MouseConfig, DeviceError> {
        if self.sensor.dpi_stages.is_empty() || self.sensor.dpi_stages.len() > 6 {
            return Err(DeviceError::Profile(
                "sensor.dpi_stages must contain 1..=6 DPI stages".to_owned(),
            ));
        }
        let mut dpi_stages = MouseConfig::factory_default().dpi_stages;
        for (i, &val) in self.sensor.dpi_stages.iter().enumerate() {
            dpi_stages[i] = val;
        }
        let dpi_count = self.sensor.dpi_stages.len() as u8;
        if self.sensor.active_stage == 0 || self.sensor.active_stage > dpi_count {
            return Err(DeviceError::Profile(format!(
                "sensor.active_stage ({}) must be between 1 and {dpi_count}",
                self.sensor.active_stage
            )));
        }

        let cfg = MouseConfig {
            dpi_stages,
            dpi_count,
            active_dpi_stage: self.sensor.active_stage - 1,
            polling_rate: PollingRate::from_hz(self.sensor.polling_rate_hz)?,
            lod: LodHeight::from_mm(self.sensor.lod_mm)?,
            debounce_ms: self.sensor.debounce_ms,
            sensor_flags: SensorFlags {
                ripple_control: self.sensor.ripple_control,
                angle_snapping: self.sensor.angle_snapping,
                motion_sync: self.sensor.motion_sync,
                reserved_bits: SensorFlags::BIT_FACTORY_BASE,
            },
            sleep_minutes: self.power_lighting.sleep_minutes,
            light_mode: parse_light_mode(&self.power_lighting.light_mode)?,
            scroll_direction: if self.sensor.reverse_scroll {
                ScrollDirection::Reverse
            } else {
                ScrollDirection::Normal
            },
            highspeed_mode: self.sensor.highspeed_mode,
            wakeup_on_move: self.power_lighting.wakeup_on_move,
            move_lights_off: self.power_lighting.move_lights_off,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Converts and validates the 8-slot [`ButtonMapping`].
    pub fn to_button_mapping(&self) -> Result<ButtonMapping, DeviceError> {
        let Some(ref b) = self.buttons else {
            return Ok(ButtonMapping::factory_default());
        };
        let mapping = ButtonMapping {
            slots: [
                parse_action_spec(&b.left)?,
                parse_action_spec(&b.right)?,
                parse_action_spec(&b.middle)?,
                parse_action_spec(&b.back)?,
                parse_action_spec(&b.forward)?,
                parse_action_spec(&b.dpi_loop)?,
                parse_action_spec(&b.scroll_up)?,
                parse_action_spec(&b.scroll_down)?,
            ],
        };
        mapping.validate()?;
        Ok(mapping)
    }

    /// Compiles optional profile macro entries into [`MacroStep`] vectors.
    pub fn to_macro_vecs(&self) -> Result<Vec<Vec<MacroStep>>, DeviceError> {
        let mut out = Vec::with_capacity(self.macros.len());
        for entry in &self.macros {
            let mut seq = Vec::with_capacity(entry.steps.len());
            for step in &entry.steps {
                let input = match step.kind.to_ascii_lowercase().as_str() {
                    "mouse" => {
                        let btn = MouseClickButton::from_mask(step.code).ok_or_else(|| {
                            DeviceError::Profile(format!(
                                "invalid mouse button mask {} in macro '{}'",
                                step.code, entry.name
                            ))
                        })?;
                        MacroInput::MouseButton(btn)
                    }
                    "key" | "keyboard" => MacroInput::KeyboardKey(step.code),
                    other => {
                        return Err(DeviceError::Profile(format!(
                            "unknown macro step kind '{other}' in macro '{}'",
                            entry.name
                        )));
                    }
                };
                seq.push(MacroStep {
                    input,
                    pressed: step.pressed,
                    delay_ms: step.delay_ms,
                });
            }
            out.push(seq);
        }
        Ok(out)
    }
}

pub fn parse_light_mode(s: &str) -> Result<LightMode, DeviceError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "0" => Ok(LightMode::Off),
        "wave" | "1" => Ok(LightMode::Wave),
        "neon" | "2" => Ok(LightMode::Neon),
        "flash" | "cycling_flash" | "3" => Ok(LightMode::CyclingFlash),
        "yoyo" | "yoyo_ball" | "4" => Ok(LightMode::YoyoBall),
        "single" | "single_flash" | "5" => Ok(LightMode::SingleFlash),
        "breathing" | "breathing_loop" | "6" => Ok(LightMode::BreathingLoop),
        other => Err(DeviceError::Profile(format!(
            "unknown light_mode '{other}' (expected off, wave, neon, flash, yoyo, single, breathing)"
        ))),
    }
}

pub fn format_light_mode(m: LightMode) -> &'static str {
    match m {
        LightMode::Off => "off",
        LightMode::Wave => "wave",
        LightMode::Neon => "neon",
        LightMode::CyclingFlash => "cycling_flash",
        LightMode::YoyoBall => "yoyo_ball",
        LightMode::SingleFlash => "single_flash",
        LightMode::BreathingLoop => "breathing_loop",
    }
}

fn parse_action_spec(spec: &ButtonBindingSpec) -> Result<ButtonAction, DeviceError> {
    match spec {
        ButtonBindingSpec::Simple(s) => match s.trim().to_ascii_lowercase().as_str() {
            "left" | "left_click" => Ok(ButtonAction::MouseClick(MouseClickButton::Left)),
            "right" | "right_click" => Ok(ButtonAction::MouseClick(MouseClickButton::Right)),
            "middle" | "middle_click" => Ok(ButtonAction::MouseClick(MouseClickButton::Middle)),
            "back" | "backward" => Ok(ButtonAction::MouseClick(MouseClickButton::Back)),
            "forward" => Ok(ButtonAction::MouseClick(MouseClickButton::Forward)),
            "dpi" | "dpi_loop" => Ok(ButtonAction::DpiLoop),
            "scroll_up" => Ok(ButtonAction::ScrollUp),
            "scroll_down" => Ok(ButtonAction::ScrollDown),
            "disabled" | "none" => Ok(ButtonAction::Disabled),
            "media:vol_up" => Ok(ButtonAction::Media(MediaFunction::VolumeUp)),
            "media:vol_down" => Ok(ButtonAction::Media(MediaFunction::VolumeDown)),
            "media:mute" => Ok(ButtonAction::Media(MediaFunction::Mute)),
            "media:play_pause" => Ok(ButtonAction::Media(MediaFunction::PlayPause)),
            "media:next" => Ok(ButtonAction::Media(MediaFunction::NextTrack)),
            "media:prev" => Ok(ButtonAction::Media(MediaFunction::PrevTrack)),
            "media:calculator" => Ok(ButtonAction::Media(MediaFunction::Calculator)),
            other => Err(DeviceError::Profile(format!(
                "unknown button binding '{other}'"
            ))),
        },
        ButtonBindingSpec::Structured {
            kind,
            modifiers,
            keycode,
            macro_slot,
            repeat,
            loop_mode,
        } => match kind.to_ascii_lowercase().as_str() {
            "shortcut" | "key" => {
                let mut mask = 0u8;
                for m in modifiers {
                    match m.to_ascii_lowercase().as_str() {
                        "ctrl" | "lctrl" => mask |= KeyModifiers::LCTRL,
                        "shift" | "lshift" => mask |= KeyModifiers::LSHIFT,
                        "alt" | "lalt" => mask |= KeyModifiers::LALT,
                        "win" | "super" | "lgui" => mask |= KeyModifiers::LWIN,
                        "rctrl" => mask |= KeyModifiers::RCTRL,
                        "rshift" => mask |= KeyModifiers::RSHIFT,
                        "ralt" => mask |= KeyModifiers::RALT,
                        _ => {}
                    }
                }
                Ok(ButtonAction::KeyboardShortcut {
                    modifiers: KeyModifiers(mask),
                    keycode: keycode.unwrap_or(0),
                })
            }
            "macro" => {
                let slot = macro_slot.unwrap_or(0);
                let trigger = match loop_mode.as_deref().unwrap_or("fixed") {
                    "hold" => MacroTrigger::HoldToLoop,
                    "toggle" => MacroTrigger::ToggleLoop,
                    _ => MacroTrigger::FixedCount(repeat.unwrap_or(1).max(1)),
                };
                Ok(ButtonAction::Macro { slot, trigger })
            }
            other => Err(DeviceError::Profile(format!(
                "unknown structured button kind '{other}'"
            ))),
        },
    }
}

fn spec_from_action(action: ButtonAction) -> ButtonBindingSpec {
    match action {
        ButtonAction::MouseClick(MouseClickButton::Left) => {
            ButtonBindingSpec::Simple("left".to_owned())
        }
        ButtonAction::MouseClick(MouseClickButton::Right) => {
            ButtonBindingSpec::Simple("right".to_owned())
        }
        ButtonAction::MouseClick(MouseClickButton::Middle) => {
            ButtonBindingSpec::Simple("middle".to_owned())
        }
        ButtonAction::MouseClick(MouseClickButton::Back) => {
            ButtonBindingSpec::Simple("back".to_owned())
        }
        ButtonAction::MouseClick(MouseClickButton::Forward) => {
            ButtonBindingSpec::Simple("forward".to_owned())
        }
        ButtonAction::DpiLoop => ButtonBindingSpec::Simple("dpi_loop".to_owned()),
        ButtonAction::ScrollUp => ButtonBindingSpec::Simple("scroll_up".to_owned()),
        ButtonAction::ScrollDown => ButtonBindingSpec::Simple("scroll_down".to_owned()),
        ButtonAction::Disabled => ButtonBindingSpec::Simple("disabled".to_owned()),
        ButtonAction::Media(MediaFunction::VolumeUp) => {
            ButtonBindingSpec::Simple("media:vol_up".to_owned())
        }
        ButtonAction::Media(MediaFunction::VolumeDown) => {
            ButtonBindingSpec::Simple("media:vol_down".to_owned())
        }
        ButtonAction::Media(MediaFunction::Mute) => {
            ButtonBindingSpec::Simple("media:mute".to_owned())
        }
        ButtonAction::Media(MediaFunction::PlayPause) => {
            ButtonBindingSpec::Simple("media:play_pause".to_owned())
        }
        ButtonAction::Media(MediaFunction::NextTrack) => {
            ButtonBindingSpec::Simple("media:next".to_owned())
        }
        ButtonAction::Media(MediaFunction::PrevTrack) => {
            ButtonBindingSpec::Simple("media:prev".to_owned())
        }
        ButtonAction::Media(MediaFunction::Calculator) => {
            ButtonBindingSpec::Simple("media:calculator".to_owned())
        }
        ButtonAction::KeyboardShortcut { modifiers, keycode } => {
            let mut mods = Vec::new();
            if modifiers.has_ctrl() {
                mods.push("ctrl".to_owned());
            }
            if modifiers.has_shift() {
                mods.push("shift".to_owned());
            }
            if modifiers.has_alt() {
                mods.push("alt".to_owned());
            }
            if modifiers.has_win() {
                mods.push("win".to_owned());
            }
            ButtonBindingSpec::Structured {
                kind: "shortcut".to_owned(),
                modifiers: mods,
                keycode: Some(keycode),
                macro_slot: None,
                repeat: None,
                loop_mode: None,
            }
        }
        ButtonAction::Macro { slot, trigger } => {
            let (repeat, mode) = match trigger {
                MacroTrigger::FixedCount(n) => (Some(n), Some("fixed".to_owned())),
                MacroTrigger::HoldToLoop => (None, Some("hold".to_owned())),
                MacroTrigger::ToggleLoop => (None, Some("toggle".to_owned())),
            };
            ButtonBindingSpec::Structured {
                kind: "macro".to_owned(),
                modifiers: Vec::new(),
                keycode: None,
                macro_slot: Some(slot),
                repeat,
                loop_mode: mode,
            }
        }
        ButtonAction::Media(_) | ButtonAction::Raw(_) => {
            ButtonBindingSpec::Simple("disabled".to_owned())
        }
    }
}
