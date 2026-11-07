//! Hardware identification, HID usage constants, opcodes, and byte offsets
//! extracted from the official Fire Phoenix BK-R1X WebHID driver (`bk-r1x.yjx2012.com`).

/// USB-C Wired mode Vendor ID (`43172` decimal in OEM bundle).
pub const VID_WIRED: u16 = 0xA8A4;
/// USB-C Wired mode Product ID (`8789` decimal in OEM bundle).
pub const PID_WIRED: u16 = 0x2255;

/// 2.4GHz Wireless Receiver Dongle Vendor ID (`43173` decimal in OEM bundle).
pub const VID_24GHZ_DONGLE: u16 = 0xA8A5;
/// 2.4GHz Wireless Receiver Dongle Product ID (`8789` decimal in OEM bundle).
pub const PID_24GHZ_DONGLE: u16 = 0x2255;

/// Minimum Vendor-Defined HID Usage Page (`0xFF00`).
pub const VENDOR_USAGE_PAGE_MIN: u16 = 0xFF00;
/// Exact BK-R1X Vendor-Defined HID Usage Page (`65281` = `0xFF01`).
pub const VENDOR_USAGE_PAGE_BKR1X: u16 = 0xFF01;
/// Maximum Vendor-Defined HID Usage Page (`0xFFFF`).
pub const VENDOR_USAGE_PAGE_MAX: u16 = 0xFFFF;
/// Exact BK-R1X Vendor-Defined HID Usage (`16` = `0x0010`).
pub const VENDOR_USAGE_BKR1X: u16 = 0x0010;

/// HID Report ID used for standard YJX2012 vendor commands (`0x00`).
pub const REPORT_ID_VENDOR_CMD: u8 = 0x00;
/// HID Report ID used for direct macro memory read/reset commands (`0x06`).
pub const REPORT_ID_MACRO_DIRECT: u8 = 0x06;

/// Payload size of a single HID vendor report (excluding the 1-byte Report ID).
pub const REPORT_PAYLOAD_LEN: usize = 64;
/// Total wire frame size passed to `hidapi::HidDevice::write` (`[report_id, payload[0..64]]`).
pub const WIRE_FRAME_LEN: usize = 65;

/// Total MCU flash space reserved for hardware macro directory + events.
pub const MACRO_FLASH_TOTAL_LEN: usize = 4096;
/// Byte size of the 32-entry `u16` LE offset directory at the start of macro flash.
pub const MACRO_DIRECTORY_LEN: usize = 64;
/// Maximum number of hardware macro slots supported by the MCU directory.
pub const MAX_MACRO_SLOTS: usize = 32;
/// Maximum payload chunk size per `WriteMacroChunk` (`0x0D`) packet.
pub const MACRO_CHUNK_DATA_LEN: usize = 56;

/// Outgoing host-to-device packet header byte (`85` = `0x55`).
pub const TX_MAGIC: u8 = 0x55;
/// Incoming device-to-host packet header byte (`170` = `0xAA`).
pub const RX_MAGIC: u8 = 0xAA;
/// Incoming dongle pairing notification header `[0x13, 0xE2]`.
pub const RX_PAIRING_MAGIC_0: u8 = 0x13;
pub const RX_PAIRING_MAGIC_1: u8 = 0xE2;

/// Read authentication token bytes `[0xA5, 0x0B]`.
pub const AUTH_READ: [u8; 2] = [0xA5, 0x0B];
/// Key mapping / macro commit authentication token bytes `[0xA5, 0x22]`.
pub const AUTH_WRITE_KEYS: [u8; 2] = [0xA5, 0x22];
/// Sensor & device configuration write authentication token bytes `[0xAE, 0x0A]`.
pub const AUTH_WRITE_CONFIG: [u8; 2] = [0xAE, 0x0A];

/// PixArt PAW3311 minimum hardware CPI/DPI resolution.
pub const PAW3311_DPI_MIN: u16 = 50;
/// PixArt PAW3311 maximum hardware CPI/DPI resolution on BK-R1X.
pub const PAW3311_DPI_MAX: u16 = 18_000;
/// PixArt PAW3311 native step alignment (50 CPI per step; OEM web UI uses 100).
pub const PAW3311_DPI_STEP: u16 = 50;
/// Maximum programmable DPI stages stored in MCU flash.
pub const MAX_DPI_STAGES: usize = 6;
/// Maximum configurable button debounce delay in milliseconds.
pub const MAX_DEBOUNCE_MS: u8 = 30;
/// Default sleep timeout in minutes (`10` min).
pub const VALID_SLEEP_MINUTES_DEFAULT: u8 = 10;
/// Number of programmable key/wheel slots in the MCU button table.
pub const BUTTON_SLOT_COUNT: usize = 8;

/// Default stage indicator RGB colors from `968.15ef65fde5f6a90f.js`.
pub const DEFAULT_STAGE_COLORS: [[u8; 3]; MAX_DPI_STAGES] = [
    [0xFF, 0x00, 0x00], // Stage 1: 800 DPI  (#FF0000 Red)
    [0x00, 0xFF, 0x00], // Stage 2: 1200 DPI (#00FF00 Green)
    [0x00, 0x00, 0xFF], // Stage 3: 1600 DPI (#0000FF Blue)
    [0xFF, 0xFF, 0x00], // Stage 4: 3200 DPI (#FFFF00 Yellow)
    [0x00, 0xFF, 0xFF], // Stage 5: 5000 DPI (#00FFFF Cyan)
    [0x80, 0x00, 0x80], // Stage 6: 18000 DPI (#800080 Purple)
];

/// YJX2012 V1 command opcodes (`payload[1]` on 64-byte report, `wire[2]` on 65-byte frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    /// Query ASCII firmware version (`rx[23..=25]`).
    GetFirmwareVersion = 0x03,
    /// Read 8 × 4-byte button mapping descriptors (`32` bytes).
    GetMouseKeys = 0x08,
    /// Write 8 × 4-byte button mapping descriptors (`32` bytes).
    SetMouseKeys = 0x09,
    /// Read 56-byte chunk from 4096-byte macro flash (uses `ReportID = 0x06`).
    ReadMacroChunk = 0x0C,
    /// Write up to 56-byte chunk into 4096-byte macro flash.
    WriteMacroChunk = 0x0D,
    /// Read active PAW3311 & MCU configuration block (`47` bytes).
    GetMouseConfig = 0x0E,
    /// Write active PAW3311 & MCU configuration block (`47` bytes).
    SetMouseConfig = 0x0F,
    /// Commit written macro chunks to non-volatile flash.
    CommitMacroFlash = 0x10,
    /// Set LED lighting effect mode (`0x00..=0x06`).
    SetLightMode = 0x21,
    /// Query battery percentage (`rx[8]`) and charging flag (`rx[9]`).
    GetBatteryInfo = 0x30,
    /// Query whether the wireless mouse MCU is awake/online (`rx[8] == 2`).
    GetOnlineStatus = 0xED,
    /// Asynchronous device-push telemetry (`rx[8] == 0xD0` battery, else DPI/rate).
    AsyncTelemetry = 0xFA,
}

impl Opcode {
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x03 => Some(Self::GetFirmwareVersion),
            0x08 => Some(Self::GetMouseKeys),
            0x09 => Some(Self::SetMouseKeys),
            0x0C => Some(Self::ReadMacroChunk),
            0x0D => Some(Self::WriteMacroChunk),
            0x0E => Some(Self::GetMouseConfig),
            0x0F => Some(Self::SetMouseConfig),
            0x10 => Some(Self::CommitMacroFlash),
            0x21 => Some(Self::SetLightMode),
            0x30 => Some(Self::GetBatteryInfo),
            0xED => Some(Self::GetOnlineStatus),
            0xFA => Some(Self::AsyncTelemetry),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::GetFirmwareVersion => "GET_FIRMWARE_VERSION",
            Self::GetMouseKeys => "GET_MOUSE_KEYS",
            Self::SetMouseKeys => "SET_MOUSE_KEYS",
            Self::ReadMacroChunk => "READ_MACRO_CHUNK",
            Self::WriteMacroChunk => "WRITE_MACRO_CHUNK",
            Self::GetMouseConfig => "GET_MOUSE_CONFIG",
            Self::SetMouseConfig => "SET_MOUSE_CONFIG",
            Self::CommitMacroFlash => "COMMIT_MACRO_FLASH",
            Self::SetLightMode => "SET_LIGHT_MODE",
            Self::GetBatteryInfo => "GET_BATTERY_INFO",
            Self::GetOnlineStatus => "GET_ONLINE_STATUS",
            Self::AsyncTelemetry => "ASYNC_TELEMETRY",
        }
    }
}

/// Byte offsets within the 64-byte HID payload (after the 1-byte Report ID).
pub mod offsets {
    pub const MAGIC: usize = 0;
    pub const OPCODE: usize = 1;
    pub const AUTH_0: usize = 2;
    pub const AUTH_1: usize = 3;
    pub const DATA_LEN: usize = 4;

    // RX Battery (`Opcode::GetBatteryInfo` = 0x30)
    pub const RX_BATTERY_PERCENT: usize = 8;
    pub const RX_BATTERY_CHARGING: usize = 9;

    // RX Async Telemetry (`Opcode::AsyncTelemetry` = 0xFA)
    pub const RX_ASYNC_SUBTYPE: usize = 8;
    pub const RX_ASYNC_SUBTYPE_BATTERY: u8 = 0xD0;
    pub const RX_ASYNC_VAL_0: usize = 9;
    pub const RX_ASYNC_VAL_1: usize = 10;

    // RX Online Status (`Opcode::GetOnlineStatus` = 0xED)
    pub const RX_ONLINE_FLAG: usize = 8;
    pub const ONLINE_AWAKE_VALUE: u8 = 2;

    // RX Firmware Version (`Opcode::GetFirmwareVersion` = 0x03)
    pub const RX_FW_MAJOR_ASCII: usize = 23;
    pub const RX_FW_MINOR_ASCII: usize = 24;
    pub const RX_FW_PATCH_ASCII: usize = 25;

    // Config block offsets in 64-byte payload (`GetMouseConfig` RX & `SetMouseConfig` TX payload)
    pub const CFG_LIGHT_MODE: usize = 9;
    pub const CFG_REPORT_RATE: usize = 10;
    pub const CFG_DPI_COUNT: usize = 11;
    pub const CFG_DPI_ACTIVE_ONE_BASED: usize = 12;
    pub const CFG_DPI_STAGES_START: usize = 13; // 6 * u16 LE = 13..25
    pub const CFG_SCROLL_DIR: usize = 48;
    pub const CFG_LOD: usize = 49;
    pub const CFG_SENSOR_FLAGS: usize = 50;
    pub const CFG_DEBOUNCE_MS: usize = 51;
    pub const CFG_SLEEP_MINUTES: usize = 52;
    pub const CFG_HIGHSPEED_MODE: usize = 53;
    pub const CFG_POWER_LIGHT_PACKED_TX: usize = 54;
    pub const CFG_POWER_LIGHT_PACKED_RX_ALT: usize = 55;

    // Key mapping offsets in 64-byte payload
    pub const KEYS_DATA_START: usize = 8; // 8 * 4 bytes = 8..40
}
