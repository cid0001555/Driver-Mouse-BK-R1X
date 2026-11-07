// Constants and register offsets extracted from the OEM WebHID bundle (`bk-r1x.yjx2012.com`).

pub const VID_WIRED: u16 = 0xA8A4;
pub const PID_WIRED: u16 = 0x2255;

pub const VID_24GHZ_DONGLE: u16 = 0xA8A5;
pub const PID_24GHZ_DONGLE: u16 = 0x2255;

pub const VENDOR_USAGE_PAGE_MIN: u16 = 0xFF00;
pub const VENDOR_USAGE_PAGE_BKR1X: u16 = 0xFF01;
pub const VENDOR_USAGE_PAGE_MAX: u16 = 0xFFFF;
pub const VENDOR_USAGE_BKR1X: u16 = 0x0010;

pub const REPORT_ID_VENDOR_CMD: u8 = 0x00;
pub const REPORT_ID_MACRO_DIRECT: u8 = 0x06;

pub const REPORT_PAYLOAD_LEN: usize = 64;
pub const WIRE_FRAME_LEN: usize = 65;

pub const MACRO_FLASH_TOTAL_LEN: usize = 4096;
/// First 64 bytes of macro flash store 32 little-endian `u16` byte offsets into the event area.
pub const MACRO_DIRECTORY_LEN: usize = 64;
pub const MAX_MACRO_SLOTS: usize = 32;
pub const MACRO_CHUNK_DATA_LEN: usize = 56;

pub const TX_MAGIC: u8 = 0x55;
pub const RX_MAGIC: u8 = 0xAA;
pub const RX_PAIRING_MAGIC_0: u8 = 0x13;
pub const RX_PAIRING_MAGIC_1: u8 = 0xE2;

pub const AUTH_READ: [u8; 2] = [0xA5, 0x0B];
pub const AUTH_WRITE_KEYS: [u8; 2] = [0xA5, 0x22];
pub const AUTH_WRITE_CONFIG: [u8; 2] = [0xAE, 0x0A];

pub const PAW3311_DPI_MIN: u16 = 50;
pub const PAW3311_DPI_MAX: u16 = 18_000;
pub const PAW3311_DPI_STEP: u16 = 50;
pub const MAX_DPI_STAGES: usize = 6;
pub const MAX_DEBOUNCE_MS: u8 = 30;
pub const VALID_SLEEP_MINUTES_DEFAULT: u8 = 10;
pub const BUTTON_SLOT_COUNT: usize = 8;

pub const DEFAULT_STAGE_COLORS: [[u8; 3]; MAX_DPI_STAGES] = [
    [0xFF, 0x00, 0x00],
    [0x00, 0xFF, 0x00],
    [0x00, 0x00, 0xFF],
    [0xFF, 0xFF, 0x00],
    [0x00, 0xFF, 0xFF],
    [0x80, 0x00, 0x80],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    GetFirmwareVersion = 0x03,
    GetMouseKeys = 0x08,
    SetMouseKeys = 0x09,
    ReadMacroChunk = 0x0C,
    WriteMacroChunk = 0x0D,
    GetMouseConfig = 0x0E,
    SetMouseConfig = 0x0F,
    CommitMacroFlash = 0x10,
    SetLightMode = 0x21,
    GetBatteryInfo = 0x30,
    GetOnlineStatus = 0xED,
    AsyncTelemetry = 0xFA,
}

impl Opcode {
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
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

/// Byte offsets within the 64-byte HID payload (excluding the leading 1-byte Report ID).
pub mod offsets {
    pub const MAGIC: usize = 0;
    pub const OPCODE: usize = 1;
    pub const AUTH_0: usize = 2;
    pub const AUTH_1: usize = 3;
    pub const DATA_LEN: usize = 4;
    pub const TX_SUBHEADER_0: usize = 5;
    pub const TX_SUBHEADER_1: usize = 6;

    pub const DATA_LEN_BATTERY_ONLINE: u8 = 0x2E;
    pub const DATA_LEN_LIGHT_MODE: u8 = 0x03;
    pub const DATA_LEN_MOUSE_KEYS: u8 = 0x20;
    pub const DATA_LEN_MOUSE_CONFIG: u8 = 0x2F;

    pub const TX_LIGHT_MODE_VALUE: usize = 10;

    pub const TX_MACRO_OFFSET_LO: usize = 5;
    pub const TX_MACRO_OFFSET_HI: usize = 6;
    pub const TX_MACRO_BANK_FLAG: usize = 7;
    pub const TX_MACRO_DATA_START: usize = 8;
    pub const TX_COMMIT_MACRO_TAG: usize = 7;
    pub const COMMIT_MACRO_TAG_VALUE: u8 = 0x05;

    pub const DIRECT_MACRO_OPCODE: usize = 0;
    pub const DIRECT_MACRO_LEN: usize = 1;
    pub const DIRECT_MACRO_OFFSET_LO: usize = 2;
    pub const DIRECT_MACRO_OFFSET_HI: usize = 3;

    pub const RX_BATTERY_PERCENT: usize = 8;
    pub const RX_BATTERY_CHARGING: usize = 9;

    pub const RX_ASYNC_SUBTYPE: usize = 8;
    pub const RX_ASYNC_SUBTYPE_BATTERY: u8 = 0xD0;
    pub const RX_ASYNC_VAL_0: usize = 9;
    pub const RX_ASYNC_VAL_1: usize = 10;

    pub const RX_ONLINE_FLAG: usize = 8;
    pub const ONLINE_AWAKE_VALUE: u8 = 2;

    pub const RX_FW_MAJOR_ASCII: usize = 23;
    pub const RX_FW_MINOR_ASCII: usize = 24;
    pub const RX_FW_PATCH_ASCII: usize = 25;

    pub const CFG_LIGHT_MODE: usize = 9;
    pub const CFG_REPORT_RATE: usize = 10;
    pub const CFG_DPI_COUNT: usize = 11;
    pub const CFG_DPI_ACTIVE_ONE_BASED: usize = 12;
    pub const CFG_DPI_STAGES_START: usize = 13;
    pub const CFG_SCROLL_DIR: usize = 48;
    pub const CFG_LOD: usize = 49;
    pub const CFG_SENSOR_FLAGS: usize = 50;
    pub const CFG_DEBOUNCE_MS: usize = 51;
    pub const CFG_SLEEP_MINUTES: usize = 52;
    pub const CFG_HIGHSPEED_MODE: usize = 53;
    pub const CFG_POWER_LIGHT_PACKED_TX: usize = 54;
    pub const CFG_POWER_LIGHT_PACKED_RX_ALT: usize = 55;

    pub const KEYS_DATA_START: usize = 8;
}
