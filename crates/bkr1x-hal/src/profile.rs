use std::fs;
use std::path::Path;

use bkr1x_protocol::{
    ButtonAction, ButtonMapping, KeyModifiers, LightMode, LodHeight, MAX_DPI_STAGES, MacroInput,
    MacroStep, MacroTrigger, MediaFunction, MouseClickButton, MouseConfig, PollingRate,
    ScrollDirection, SensorFlags,
};
use serde::{Deserialize, Serialize};

use crate::error::DeviceError;
use crate::hex::decode_flexible_hex;

const MEDIA_SPEC_TABLE: [(MediaFunction, &str); 18] = [
    (MediaFunction::VolumeUp, "media:vol_up"),
    (MediaFunction::VolumeDown, "media:vol_down"),
    (MediaFunction::Mute, "media:mute"),
    (MediaFunction::PlayPause, "media:play_pause"),
    (MediaFunction::Stop, "media:stop"),
    (MediaFunction::PrevTrack, "media:prev"),
    (MediaFunction::NextTrack, "media:next"),
    (MediaFunction::MediaPlayer, "media:player"),
    (MediaFunction::Mail, "media:mail"),
    (MediaFunction::Calculator, "media:calculator"),
    (MediaFunction::MyComputer, "media:my_computer"),
    (MediaFunction::WebSearch, "media:web_search"),
    (MediaFunction::WebHome, "media:web_home"),
    (MediaFunction::WebBack, "media:web_back"),
    (MediaFunction::WebForward, "media:web_forward"),
    (MediaFunction::WebStop, "media:web_stop"),
    (MediaFunction::WebRefresh, "media:web_refresh"),
    (MediaFunction::WebFavorites, "media:web_favorites"),
];

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
    /// One-based active stage index (`1..=6`).
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
    #[serde(default = "default_true")]
    pub wakeup_on_move: bool,
    #[serde(default = "default_true")]
    pub move_lights_off: bool,
}

const fn default_true() -> bool {
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
    Simple(String),
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
    pub kind: String,
    pub code: u8,
    pub pressed: bool,
    pub delay_ms: u16,
}

impl HardwareProfile {
    pub fn load_from_file(path: &Path) -> Result<Self, DeviceError> {
        let raw = fs::read_to_string(path)
            .map_err(|e| DeviceError::Profile(format!("failed reading {}: {e}", path.display())))?;

        let profile: Self = if is_json_extension(path) {
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

    pub fn save_to_file(&self, path: &Path) -> Result<(), DeviceError> {
        let serialized = if is_json_extension(path) {
            serde_json::to_string_pretty(self)
                .map_err(|e| DeviceError::Profile(format!("JSON serialize error: {e}")))?
        } else {
            toml::to_string_pretty(self)
                .map_err(|e| DeviceError::Profile(format!("TOML serialize error: {e}")))?
        };
        fs::write(path, serialized)
            .map_err(|e| DeviceError::Profile(format!("failed writing {}: {e}", path.display())))
    }

    pub fn from_hardware(
        name: impl Into<String>,
        cfg: &MouseConfig,
        buttons: &ButtonMapping,
    ) -> Self {
        let stage_count = (cfg.dpi_count as usize).min(MAX_DPI_STAGES);
        Self {
            name: name.into(),
            match_process: None,
            sensor: SensorSection {
                dpi_stages: cfg.dpi_stages[..stage_count].to_vec(),
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

    pub fn to_mouse_config(&self) -> Result<MouseConfig, DeviceError> {
        if self.sensor.dpi_stages.is_empty() || self.sensor.dpi_stages.len() > MAX_DPI_STAGES {
            return Err(DeviceError::Profile(
                "sensor.dpi_stages must contain 1..=6 DPI stages".to_owned(),
            ));
        }

        let mut dpi_stages = MouseConfig::factory_default().dpi_stages;
        for (idx, &dpi) in self.sensor.dpi_stages.iter().enumerate() {
            dpi_stages[idx] = dpi;
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

    pub fn to_macro_vecs(&self) -> Result<Vec<Vec<MacroStep>>, DeviceError> {
        decode_macro_entries(&self.macros)
    }
}

pub fn decode_macro_entries(
    entries: &[MacroProfileEntry],
) -> Result<Vec<Vec<MacroStep>>, DeviceError> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut seq = Vec::with_capacity(entry.steps.len());
        for step in &entry.steps {
            let input = if step.kind.eq_ignore_ascii_case("mouse") {
                let btn = MouseClickButton::from_mask(step.code).ok_or_else(|| {
                    DeviceError::Profile(format!(
                        "invalid mouse button mask {} in macro '{}'",
                        step.code, entry.name
                    ))
                })?;
                MacroInput::MouseButton(btn)
            } else if step.kind.eq_ignore_ascii_case("key")
                || step.kind.eq_ignore_ascii_case("keyboard")
            {
                MacroInput::KeyboardKey(step.code)
            } else {
                return Err(DeviceError::Profile(format!(
                    "unknown macro step kind '{}' in macro '{}'",
                    step.kind, entry.name
                )));
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

fn is_json_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

pub fn parse_light_mode(raw: &str) -> Result<LightMode, DeviceError> {
    let trimmed = raw.trim();
    for (mode, names) in [
        (LightMode::Off, &["off", "0"][..]),
        (LightMode::Wave, &["wave", "1"]),
        (LightMode::Neon, &["neon", "2"]),
        (LightMode::CyclingFlash, &["flash", "cycling_flash", "3"]),
        (LightMode::YoyoBall, &["yoyo", "yoyo_ball", "4"]),
        (LightMode::SingleFlash, &["single", "single_flash", "5"]),
        (
            LightMode::BreathingLoop,
            &["breathing", "breathing_loop", "6"],
        ),
    ] {
        if names
            .iter()
            .any(|candidate| trimmed.eq_ignore_ascii_case(candidate))
        {
            return Ok(mode);
        }
    }
    Err(DeviceError::Profile(format!(
        "unknown light_mode '{trimmed}' (expected off, wave, neon, flash, yoyo, single, breathing)"
    )))
}

pub const fn format_light_mode(mode: LightMode) -> &'static str {
    match mode {
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
        ButtonBindingSpec::Simple(raw) => parse_simple_action(raw.trim()),
        ButtonBindingSpec::Structured {
            kind,
            modifiers,
            keycode,
            macro_slot,
            repeat,
            loop_mode,
        } => {
            if kind.eq_ignore_ascii_case("shortcut") || kind.eq_ignore_ascii_case("key") {
                let mask = parse_modifier_list(modifiers);
                return Ok(ButtonAction::KeyboardShortcut {
                    modifiers: KeyModifiers(mask),
                    keycode: keycode.unwrap_or(0),
                });
            }
            if kind.eq_ignore_ascii_case("macro") {
                let slot = macro_slot.unwrap_or(0);
                let mode = loop_mode.as_deref().unwrap_or("fixed");
                let trigger = if mode.eq_ignore_ascii_case("hold") {
                    MacroTrigger::HoldToLoop
                } else if mode.eq_ignore_ascii_case("toggle") {
                    MacroTrigger::ToggleLoop
                } else {
                    MacroTrigger::FixedCount(repeat.unwrap_or(1).max(1))
                };
                return Ok(ButtonAction::Macro { slot, trigger });
            }
            Err(DeviceError::Profile(format!(
                "unknown structured button kind '{kind}'"
            )))
        }
    }
}

fn parse_simple_action(token: &str) -> Result<ButtonAction, DeviceError> {
    for (action, aliases) in [
        (
            ButtonAction::MouseClick(MouseClickButton::Left),
            &["left", "left_click"][..],
        ),
        (
            ButtonAction::MouseClick(MouseClickButton::Right),
            &["right", "right_click"],
        ),
        (
            ButtonAction::MouseClick(MouseClickButton::Middle),
            &["middle", "middle_click"],
        ),
        (
            ButtonAction::MouseClick(MouseClickButton::Back),
            &["back", "backward"],
        ),
        (
            ButtonAction::MouseClick(MouseClickButton::Forward),
            &["forward"],
        ),
        (ButtonAction::DpiLoop, &["dpi", "dpi_loop"]),
        (ButtonAction::ScrollUp, &["scroll_up"]),
        (ButtonAction::ScrollDown, &["scroll_down"]),
        (ButtonAction::Disabled, &["disabled", "none"]),
    ] {
        if aliases
            .iter()
            .any(|alias| token.eq_ignore_ascii_case(alias))
        {
            return Ok(action);
        }
    }

    for (media_fn, spec_key) in MEDIA_SPEC_TABLE {
        if token.eq_ignore_ascii_case(spec_key) {
            return Ok(ButtonAction::Media(media_fn));
        }
    }

    if let Some(hex_body) = token
        .strip_prefix("raw:")
        .or_else(|| token.strip_prefix("RAW:"))
    {
        let decoded = decode_flexible_hex(hex_body).map_err(|e| {
            DeviceError::Profile(format!("invalid raw button descriptor '{token}': {e}"))
        })?;
        let raw_arr: [u8; 4] = decoded.try_into().map_err(|_| {
            DeviceError::Profile(format!("raw button descriptor '{token}' must be 4 bytes"))
        })?;
        return Ok(ButtonAction::from_bytes(raw_arr));
    }

    Err(DeviceError::Profile(format!(
        "unknown button binding '{token}'"
    )))
}

fn parse_modifier_list(modifiers: &[String]) -> u8 {
    let mut mask = 0u8;
    for item in modifiers {
        for (bit, names) in [
            (KeyModifiers::LCTRL, &["ctrl", "lctrl"][..]),
            (KeyModifiers::LSHIFT, &["shift", "lshift"]),
            (KeyModifiers::LALT, &["alt", "lalt"]),
            (KeyModifiers::LWIN, &["win", "super", "lgui", "lwin"]),
            (KeyModifiers::RCTRL, &["rctrl"]),
            (KeyModifiers::RSHIFT, &["rshift"]),
            (KeyModifiers::RALT, &["ralt"]),
            (KeyModifiers::RWIN, &["rwin", "rgui"]),
        ] {
            if names.iter().any(|name| item.eq_ignore_ascii_case(name)) {
                mask |= bit;
            }
        }
    }
    mask
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
        ButtonAction::Media(func) => {
            let key = MEDIA_SPEC_TABLE
                .iter()
                .find(|(m, _)| *m == func)
                .map(|(_, key)| *key)
                .unwrap_or("disabled");
            ButtonBindingSpec::Simple(key.to_owned())
        }
        ButtonAction::KeyboardShortcut { modifiers, keycode } => {
            let mut mods = Vec::new();
            for (bit, label) in [
                (KeyModifiers::LCTRL, "ctrl"),
                (KeyModifiers::LSHIFT, "shift"),
                (KeyModifiers::LALT, "alt"),
                (KeyModifiers::LWIN, "win"),
                (KeyModifiers::RCTRL, "rctrl"),
                (KeyModifiers::RSHIFT, "rshift"),
                (KeyModifiers::RALT, "ralt"),
                (KeyModifiers::RWIN, "rwin"),
            ] {
                if (modifiers.0 & bit) != 0 {
                    mods.push(label.to_owned());
                }
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
            let (repeat, loop_mode) = match trigger {
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
                loop_mode,
            }
        }
        ButtonAction::Raw([b0, b1, b2, b3]) => {
            ButtonBindingSpec::Simple(format!("raw:{b0:02x}{b1:02x}{b2:02x}{b3:02x}"))
        }
    }
}
