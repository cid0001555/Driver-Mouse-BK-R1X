use core::fmt;

use crate::constants::{
    BUTTON_SLOT_COUNT, MACRO_DIRECTORY_LEN, MACRO_FLASH_TOTAL_LEN, MAX_DEBOUNCE_MS, MAX_DPI_STAGES,
    MAX_MACRO_SLOTS, PAW3311_DPI_MAX, PAW3311_DPI_MIN, PAW3311_DPI_STEP,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    PacketTooShort {
        actual: usize,
        expected_min: usize,
    },
    InvalidMagicHeader(u8),
    ChecksumMismatch,
    UnknownOpcode(u8),
    InvalidDpi {
        stage: usize,
        value: u16,
    },
    InvalidDpiCount(u8),
    InvalidActiveDpiStage {
        active_index: u8,
        count: u8,
    },
    InvalidPollingRate(u16),
    InvalidLod(u8),
    InvalidDebounceMs(u8),
    InvalidSleepMinutes(u8),
    /// Prevents locking the user out of primary OS mouse interaction.
    NoPrimaryLeftClickAssigned,
    InvalidMacroSlot(u8),
    MacroFlashOverflow {
        required: usize,
        capacity: usize,
    },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort {
                actual,
                expected_min,
            } => write!(
                f,
                "packet too short ({actual} bytes, expected >= {expected_min})"
            ),
            Self::InvalidMagicHeader(byte) => {
                write!(f, "invalid packet magic header 0x{byte:02X}")
            }
            Self::ChecksumMismatch => write!(f, "packet checksum verification failed"),
            Self::UnknownOpcode(op) => write!(f, "unknown YJX2012 opcode 0x{op:02X}"),
            Self::InvalidDpi { stage, value } => write!(
                f,
                "invalid PAW3311 DPI {value} on stage {} (must be {PAW3311_DPI_MIN}..={PAW3311_DPI_MAX} in steps of {PAW3311_DPI_STEP})",
                stage + 1
            ),
            Self::InvalidDpiCount(count) => {
                write!(f, "invalid DPI stage count {count} (must be 1..=6)")
            }
            Self::InvalidActiveDpiStage {
                active_index,
                count,
            } => write!(
                f,
                "active DPI stage index {active_index} out of range for {count} enabled stages"
            ),
            Self::InvalidPollingRate(hz) => write!(
                f,
                "invalid polling rate {hz} Hz (supported: 125, 250, 500, 1000 Hz)"
            ),
            Self::InvalidLod(mm) => {
                write!(f, "invalid lift-off distance {mm} mm (supported: 1mm, 2mm)")
            }
            Self::InvalidDebounceMs(ms) => {
                write!(
                    f,
                    "invalid debounce {ms} ms (must be 0..={MAX_DEBOUNCE_MS} ms)"
                )
            }
            Self::InvalidSleepMinutes(min) => write!(
                f,
                "invalid sleep timeout {min} min (supported: 1, 3, 5, 10, 20, 30, 60 min)"
            ),
            Self::NoPrimaryLeftClickAssigned => {
                write!(
                    f,
                    "at least one mouse button must remain mapped to Left Click"
                )
            }
            Self::InvalidMacroSlot(slot) => {
                write!(
                    f,
                    "invalid macro slot {slot} (must be 0..{MAX_MACRO_SLOTS})"
                )
            }
            Self::MacroFlashOverflow { required, capacity } => write!(
                f,
                "macro sequence requires {required} bytes, exceeding {capacity}-byte MCU flash"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    UsbWired,
    Wireless24Ghz,
}

impl ConnectionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UsbWired => "USB-C Wired",
            Self::Wireless24Ghz => "2.4GHz Wireless",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PollingRate {
    Hz125,
    Hz250,
    Hz500,
    #[default]
    Hz1000,
}

impl PollingRate {
    pub const ALL: [Self; 4] = [Self::Hz125, Self::Hz250, Self::Hz500, Self::Hz1000];

    pub const fn from_hz(hz: u16) -> Result<Self, ProtocolError> {
        match hz {
            125 => Ok(Self::Hz125),
            250 => Ok(Self::Hz250),
            500 => Ok(Self::Hz500),
            1000 => Ok(Self::Hz1000),
            other => Err(ProtocolError::InvalidPollingRate(other)),
        }
    }

    pub const fn as_hz(self) -> u16 {
        match self {
            Self::Hz125 => 125,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
            Self::Hz1000 => 1000,
        }
    }

    /// Wire format encodes polling rate as `1..=4` (`report_rate_index + 1`).
    pub const fn to_wire_byte(self) -> u8 {
        match self {
            Self::Hz125 => 1,
            Self::Hz250 => 2,
            Self::Hz500 => 3,
            Self::Hz1000 => 4,
        }
    }

    pub const fn from_wire_byte_lossy(byte: u8) -> Self {
        match byte {
            1 => Self::Hz125,
            2 => Self::Hz250,
            3 => Self::Hz500,
            _ => Self::Hz1000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LodHeight {
    #[default]
    Mm1 = 1,
    Mm2 = 2,
}

impl LodHeight {
    pub const fn from_mm(mm: u8) -> Result<Self, ProtocolError> {
        match mm {
            1 => Ok(Self::Mm1),
            2 => Ok(Self::Mm2),
            other => Err(ProtocolError::InvalidLod(other)),
        }
    }

    pub const fn as_mm(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollDirection {
    #[default]
    Normal = 0,
    Reverse = 1,
}

impl ScrollDirection {
    pub const fn from_wire(byte: u8) -> Self {
        if byte == 1 {
            Self::Reverse
        } else {
            Self::Normal
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum LightMode {
    Off = 0,
    Wave = 1,
    #[default]
    Neon = 2,
    CyclingFlash = 3,
    YoyoBall = 4,
    SingleFlash = 5,
    BreathingLoop = 6,
}

impl LightMode {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Wave,
        Self::Neon,
        Self::CyclingFlash,
        Self::YoyoBall,
        Self::SingleFlash,
        Self::BreathingLoop,
    ];

    pub const fn from_wire(byte: u8) -> Self {
        match byte {
            0 => Self::Off,
            1 => Self::Wave,
            2 => Self::Neon,
            3 => Self::CyclingFlash,
            4 => Self::YoyoBall,
            5 => Self::SingleFlash,
            6 => Self::BreathingLoop,
            _ => Self::Off,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Wave => "Wave",
            Self::Neon => "Neon",
            Self::CyclingFlash => "Cycling Flash",
            Self::YoyoBall => "YOYO Ball",
            Self::SingleFlash => "Single Flash",
            Self::BreathingLoop => "Breathing Loop",
        }
    }
}

/// PAW3311 DSP bitmask (`payload[50]`). Bit `0x04` is an undocumented OEM baseline bit
/// (`default = 0x35`) that must be preserved across read-modify-write updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorFlags {
    pub ripple_control: bool,
    pub angle_snapping: bool,
    pub motion_sync: bool,
    pub reserved_bits: u8,
}

impl Default for SensorFlags {
    fn default() -> Self {
        Self::from_bits(0x35)
    }
}

impl SensorFlags {
    pub const BIT_RIPPLE_CONTROL: u8 = 0x01;
    pub const BIT_FACTORY_BASE: u8 = 0x04;
    pub const BIT_ANGLE_SNAPPING: u8 = 0x10;
    pub const BIT_MOTION_SYNC: u8 = 0x20;
    pub const KNOWN_MASK: u8 =
        Self::BIT_RIPPLE_CONTROL | Self::BIT_ANGLE_SNAPPING | Self::BIT_MOTION_SYNC;

    pub const fn from_bits(bits: u8) -> Self {
        Self {
            ripple_control: (bits & Self::BIT_RIPPLE_CONTROL) != 0,
            angle_snapping: (bits & Self::BIT_ANGLE_SNAPPING) != 0,
            motion_sync: (bits & Self::BIT_MOTION_SYNC) != 0,
            reserved_bits: bits & !Self::KNOWN_MASK,
        }
    }

    pub const fn to_bits(self) -> u8 {
        let mut bits = self.reserved_bits & !Self::KNOWN_MASK;
        if self.ripple_control {
            bits |= Self::BIT_RIPPLE_CONTROL;
        }
        if self.angle_snapping {
            bits |= Self::BIT_ANGLE_SNAPPING;
        }
        if self.motion_sync {
            bits |= Self::BIT_MOTION_SYNC;
        }
        bits
    }
}

pub const VALID_SLEEP_MINUTES: [u8; 7] = [1, 3, 5, 10, 20, 30, 60];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseConfig {
    pub dpi_stages: [u16; MAX_DPI_STAGES],
    pub dpi_count: u8,
    /// Zero-based index into `dpi_stages` (`0..dpi_count`).
    pub active_dpi_stage: u8,
    pub polling_rate: PollingRate,
    pub lod: LodHeight,
    pub debounce_ms: u8,
    pub sensor_flags: SensorFlags,
    pub sleep_minutes: u8,
    pub light_mode: LightMode,
    pub scroll_direction: ScrollDirection,
    pub highspeed_mode: bool,
    pub wakeup_on_move: bool,
    pub move_lights_off: bool,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self::factory_default()
    }
}

impl MouseConfig {
    pub const fn factory_default() -> Self {
        Self {
            dpi_stages: [800, 1200, 1600, 3200, 5000, 18000],
            dpi_count: 6,
            active_dpi_stage: 2,
            polling_rate: PollingRate::Hz1000,
            lod: LodHeight::Mm1,
            debounce_ms: 2,
            sensor_flags: SensorFlags {
                ripple_control: true,
                angle_snapping: true,
                motion_sync: true,
                reserved_bits: SensorFlags::BIT_FACTORY_BASE,
            },
            sleep_minutes: 10,
            light_mode: LightMode::Neon,
            scroll_direction: ScrollDirection::Normal,
            highspeed_mode: false,
            wakeup_on_move: true,
            move_lights_off: true,
        }
    }

    pub const fn active_dpi(&self) -> u16 {
        let stage_idx = self.active_dpi_stage as usize;
        if stage_idx < MAX_DPI_STAGES {
            self.dpi_stages[stage_idx]
        } else {
            self.dpi_stages[0]
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        let enabled_count = self.dpi_count as usize;
        if enabled_count == 0 || enabled_count > MAX_DPI_STAGES {
            return Err(ProtocolError::InvalidDpiCount(self.dpi_count));
        }
        if self.active_dpi_stage >= self.dpi_count {
            return Err(ProtocolError::InvalidActiveDpiStage {
                active_index: self.active_dpi_stage,
                count: self.dpi_count,
            });
        }
        for (stage, &dpi) in self.dpi_stages[..enabled_count].iter().enumerate() {
            validate_dpi(stage, dpi)?;
        }
        if self.debounce_ms > MAX_DEBOUNCE_MS {
            return Err(ProtocolError::InvalidDebounceMs(self.debounce_ms));
        }
        if !VALID_SLEEP_MINUTES.contains(&self.sleep_minutes) {
            return Err(ProtocolError::InvalidSleepMinutes(self.sleep_minutes));
        }
        Ok(())
    }
}

pub const fn validate_dpi(stage: usize, dpi: u16) -> Result<(), ProtocolError> {
    if dpi < PAW3311_DPI_MIN || dpi > PAW3311_DPI_MAX || !dpi.is_multiple_of(PAW3311_DPI_STEP) {
        Err(ProtocolError::InvalidDpi { stage, value: dpi })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum ButtonSlot {
    Left = 0,
    Right = 1,
    Middle = 2,
    Back = 3,
    Forward = 4,
    DpiLoop = 5,
    ScrollUp = 6,
    ScrollDown = 7,
}

impl ButtonSlot {
    pub const ALL: [Self; BUTTON_SLOT_COUNT] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
        Self::DpiLoop,
        Self::ScrollUp,
        Self::ScrollDown,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left Button",
            Self::Right => "Right Button",
            Self::Middle => "Middle Click",
            Self::Back => "Side Back (B4)",
            Self::Forward => "Side Forward (B5)",
            Self::DpiLoop => "DPI / Bottom Button",
            Self::ScrollUp => "Scroll Wheel Up",
            Self::ScrollDown => "Scroll Wheel Down",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MouseClickButton {
    Left = 0x01,
    Right = 0x02,
    Middle = 0x04,
    Back = 0x08,
    Forward = 0x10,
}

impl MouseClickButton {
    pub const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
    ];

    pub const fn from_mask(mask: u8) -> Option<Self> {
        match mask {
            0x01 => Some(Self::Left),
            0x02 => Some(Self::Right),
            0x04 => Some(Self::Middle),
            0x08 => Some(Self::Back),
            0x10 => Some(Self::Forward),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left Click",
            Self::Right => "Right Click",
            Self::Middle => "Middle Click",
            Self::Back => "Backward",
            Self::Forward => "Forward",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers(pub u8);

impl KeyModifiers {
    pub const NONE: Self = Self(0);
    pub const LCTRL: u8 = 0x01;
    pub const LSHIFT: u8 = 0x02;
    pub const LALT: u8 = 0x04;
    pub const LWIN: u8 = 0x08;
    pub const RCTRL: u8 = 0x10;
    pub const RSHIFT: u8 = 0x20;
    pub const RALT: u8 = 0x40;
    pub const RWIN: u8 = 0x80;

    pub const fn has_ctrl(self) -> bool {
        (self.0 & (Self::LCTRL | Self::RCTRL)) != 0
    }

    pub const fn has_shift(self) -> bool {
        (self.0 & (Self::LSHIFT | Self::RSHIFT)) != 0
    }

    pub const fn has_alt(self) -> bool {
        (self.0 & (Self::LALT | Self::RALT)) != 0
    }

    pub const fn has_win(self) -> bool {
        (self.0 & (Self::LWIN | Self::RWIN)) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum MediaFunction {
    VolumeUp = 0x00E9,
    VolumeDown = 0x00EA,
    Mute = 0x00E2,
    PlayPause = 0x00CD,
    Stop = 0x00B7,
    PrevTrack = 0x00B6,
    NextTrack = 0x00B5,
    MediaPlayer = 0x0183,
    Mail = 0x018A,
    Calculator = 0x0192,
    MyComputer = 0x0194,
    WebSearch = 0x0221,
    WebHome = 0x0223,
    WebBack = 0x0224,
    WebForward = 0x0225,
    WebStop = 0x0226,
    WebRefresh = 0x0227,
    WebFavorites = 0x022A,
}

impl MediaFunction {
    pub const ALL: [Self; 18] = [
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
        Self::PlayPause,
        Self::Stop,
        Self::PrevTrack,
        Self::NextTrack,
        Self::MediaPlayer,
        Self::Mail,
        Self::Calculator,
        Self::MyComputer,
        Self::WebSearch,
        Self::WebHome,
        Self::WebBack,
        Self::WebForward,
        Self::WebStop,
        Self::WebRefresh,
        Self::WebFavorites,
    ];

    pub const fn from_usage(usage: u16) -> Option<Self> {
        match usage {
            0x00E9 => Some(Self::VolumeUp),
            0x00EA => Some(Self::VolumeDown),
            0x00E2 => Some(Self::Mute),
            0x00CD => Some(Self::PlayPause),
            0x00B7 => Some(Self::Stop),
            0x00B6 => Some(Self::PrevTrack),
            0x00B5 => Some(Self::NextTrack),
            0x0183 => Some(Self::MediaPlayer),
            0x018A => Some(Self::Mail),
            0x0192 => Some(Self::Calculator),
            0x0194 => Some(Self::MyComputer),
            0x0221 => Some(Self::WebSearch),
            0x0223 => Some(Self::WebHome),
            0x0224 => Some(Self::WebBack),
            0x0225 => Some(Self::WebForward),
            0x0226 => Some(Self::WebStop),
            0x0227 => Some(Self::WebRefresh),
            0x022A => Some(Self::WebFavorites),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::VolumeUp => "Volume +",
            Self::VolumeDown => "Volume -",
            Self::Mute => "Mute",
            Self::PlayPause => "Play / Pause",
            Self::Stop => "Media Stop",
            Self::PrevTrack => "Previous Track",
            Self::NextTrack => "Next Track",
            Self::MediaPlayer => "Media Player",
            Self::Mail => "Mail",
            Self::Calculator => "Calculator",
            Self::MyComputer => "My Computer",
            Self::WebSearch => "Browser Search",
            Self::WebHome => "Browser Home",
            Self::WebBack => "Browser Back",
            Self::WebForward => "Browser Forward",
            Self::WebStop => "Browser Stop",
            Self::WebRefresh => "Browser Refresh",
            Self::WebFavorites => "Browser Favorites",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacroTrigger {
    FixedCount(u8),
    #[default]
    HoldToLoop,
    ToggleLoop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    MouseClick(MouseClickButton),
    Disabled,
    ScrollUp,
    ScrollDown,
    DpiLoop,
    KeyboardShortcut {
        modifiers: KeyModifiers,
        keycode: u8,
    },
    Media(MediaFunction),
    Macro {
        slot: u8,
        trigger: MacroTrigger,
    },
    Raw([u8; 4]),
}

impl ButtonAction {
    pub const fn to_bytes(self) -> [u8; 4] {
        match self {
            Self::MouseClick(btn) => [0x20, btn as u8, 0x00, 0x00],
            Self::Disabled => [0x20, 0x00, 0x00, 0x00],
            Self::ScrollUp => [0x21, 0x38, 0x01, 0x00],
            Self::ScrollDown => [0x21, 0x38, 0xFF, 0x00],
            Self::DpiLoop => [0x21, 0x55, 0x00, 0x00],
            Self::KeyboardShortcut { modifiers, keycode } => [0x10, modifiers.0, keycode, 0x00],
            Self::Media(func) => {
                let [lo, hi] = (func as u16).to_le_bytes();
                [0x30, lo, hi, 0x00]
            }
            Self::Macro { slot, trigger } => match trigger {
                MacroTrigger::FixedCount(n) => [0x70, slot, if n == 0 { 1 } else { n }, 0x00],
                MacroTrigger::HoldToLoop => [0x70, slot, 0x01, 0x02],
                MacroTrigger::ToggleLoop => [0x70, slot, 0x01, 0x03],
            },
            Self::Raw(raw) => raw,
        }
    }

    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        match bytes {
            [0x20, 0x00, 0x00, 0x00] => Self::Disabled,
            [0x20, mask, 0x00, 0x00] => match MouseClickButton::from_mask(mask) {
                Some(btn) => Self::MouseClick(btn),
                None => Self::Raw(bytes),
            },
            [0x21, 0x38, 0x01, 0x00] => Self::ScrollUp,
            [0x21, 0x38, 0xFF, 0x00] => Self::ScrollDown,
            // Factory firmware reports `[0xF0, 0x01, 0x01, 0x00]` on unprogrammed DPI-loop slots.
            [0x21, 0x55, 0x00, 0x00] | [0xF0, 0x01, 0x01, 0x00] => Self::DpiLoop,
            [0x10, mods, keycode, 0x00] => Self::KeyboardShortcut {
                modifiers: KeyModifiers(mods),
                keycode,
            },
            [0x30, lo, hi, 0x00] => match MediaFunction::from_usage(u16::from_le_bytes([lo, hi])) {
                Some(func) => Self::Media(func),
                None => Self::Raw(bytes),
            },
            [0x70, slot, count, mode] => {
                let trigger = match mode {
                    0x02 => MacroTrigger::HoldToLoop,
                    0x03 => MacroTrigger::ToggleLoop,
                    _ => MacroTrigger::FixedCount(if count == 0 { 1 } else { count }),
                };
                Self::Macro { slot, trigger }
            }
            other => Self::Raw(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonMapping {
    pub slots: [ButtonAction; BUTTON_SLOT_COUNT],
}

impl Default for ButtonMapping {
    fn default() -> Self {
        Self::factory_default()
    }
}

impl ButtonMapping {
    pub const DEFAULT_SLOTS: [ButtonAction; BUTTON_SLOT_COUNT] = [
        ButtonAction::MouseClick(MouseClickButton::Left),
        ButtonAction::MouseClick(MouseClickButton::Right),
        ButtonAction::MouseClick(MouseClickButton::Middle),
        ButtonAction::MouseClick(MouseClickButton::Back),
        ButtonAction::MouseClick(MouseClickButton::Forward),
        ButtonAction::DpiLoop,
        ButtonAction::ScrollUp,
        ButtonAction::ScrollDown,
    ];

    pub const fn factory_default() -> Self {
        Self {
            slots: Self::DEFAULT_SLOTS,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        let has_left_click = self
            .slots
            .iter()
            .any(|action| matches!(action, ButtonAction::MouseClick(MouseClickButton::Left)));
        if !has_left_click {
            return Err(ProtocolError::NoPrimaryLeftClickAssigned);
        }
        for &action in &self.slots {
            if let ButtonAction::Macro { slot, .. } = action
                && (slot as usize) >= MAX_MACRO_SLOTS
            {
                return Err(ProtocolError::InvalidMacroSlot(slot));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroInput {
    MouseButton(MouseClickButton),
    KeyboardKey(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroStep {
    pub input: MacroInput,
    pub pressed: bool,
    pub delay_ms: u16,
}

impl MacroStep {
    /// MCU macro event binary layout: `[delay_lo, delay_hi, flags, hid_code]`.
    /// `flags`: bit 6 (`0x40`) = key down, bit 7 (`0x80`) = end-of-macro marker.
    pub const fn encode(self, is_last: bool) -> [u8; 4] {
        let [delay_lo, delay_hi] = self.delay_ms.to_le_bytes();
        let (kind_bits, code) = match self.input {
            MacroInput::MouseButton(btn) => (0x03u8, btn as u8),
            MacroInput::KeyboardKey(keycode) => (0x02u8, keycode),
        };
        let mut flags = kind_bits;
        if self.pressed {
            flags |= 0x40;
        }
        if is_last {
            flags |= 0x80;
        }
        [delay_lo, delay_hi, flags, code]
    }
}

pub fn compile_macro_bank(
    macros: &[&[MacroStep]],
    out_flash: &mut [u8; MACRO_FLASH_TOTAL_LEN],
) -> Result<usize, ProtocolError> {
    if macros.len() > MAX_MACRO_SLOTS {
        let reported_slot = u8::try_from(macros.len()).unwrap_or(u8::MAX);
        return Err(ProtocolError::InvalidMacroSlot(reported_slot));
    }

    out_flash.fill(0);
    let mut cursor = MACRO_DIRECTORY_LEN;

    for (slot_idx, steps) in macros.iter().enumerate() {
        if steps.is_empty() {
            continue;
        }
        let byte_len = steps.len().saturating_mul(4);
        let required = cursor.saturating_add(byte_len);
        if required > MACRO_FLASH_TOTAL_LEN {
            return Err(ProtocolError::MacroFlashOverflow {
                required,
                capacity: MACRO_FLASH_TOTAL_LEN,
            });
        }

        let [off_lo, off_hi] = (cursor as u16).to_le_bytes();
        out_flash[slot_idx * 2] = off_lo;
        out_flash[slot_idx * 2 + 1] = off_hi;

        let last_idx = steps.len() - 1;
        for (step_idx, step) in steps.iter().enumerate() {
            out_flash[cursor..cursor + 4].copy_from_slice(&step.encode(step_idx == last_idx));
            cursor += 4;
        }
    }

    Ok(cursor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatteryTelemetry {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl Default for FirmwareVersion {
    fn default() -> Self {
        Self::new(1, 0, 0)
    }
}

impl FirmwareVersion {
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
