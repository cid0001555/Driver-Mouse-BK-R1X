//! Hardware, transport, IPC, and profile errors for `bkr1x-hal`.

use std::fmt;

use bkr1x_protocol::ProtocolError;

/// Unified error type returned by `bkr1x-hal` operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    /// Neither USB-C Wired (`0xA8A4:0x2255`) nor 2.4GHz Dongle (`0xA8A5:0x2255`) was found.
    NotFound,
    /// Mouse is connected via Bluetooth HOGP only (standard mouse HID present, vendor `0xFF01` interface absent).
    BluetoothConfigUnsupported,
    /// 2.4GHz Wireless receiver is enumerated, but the mouse MCU is currently in deep sleep or powered off.
    MouseAsleep,
    /// Read timed out (`150ms`) waiting for MCU response.
    Timeout,
    /// Underlying `hidapi` (`hidraw` / `hid.dll`) error.
    Hid(String),
    /// Packet framing, checksum, or PAW3311 hardware bounds validation error.
    Protocol(ProtocolError),
    /// Background daemon IPC transport error.
    Ipc(String),
    /// Profile TOML/JSON parsing or IO error.
    Profile(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(
                f,
                "Fire Phoenix BK-R1X not found (expected VID:PID 0xA8A4:0x2255 Wired or 0xA8A5:0x2255 2.4GHz on UsagePage 0xFF01)"
            ),
            Self::BluetoothConfigUnsupported => write!(
                f,
                "BK-R1X detected over Bluetooth, which only exposes standard HID mouse reports. Plug in the USB-C cable or 2.4GHz wireless receiver to configure hardware settings."
            ),
            Self::MouseAsleep => write!(
                f,
                "2.4GHz receiver is connected, but the BK-R1X mouse MCU is in deep sleep. Move or click the mouse to wake it."
            ),
            Self::Timeout => write!(f, "HID read timed out after 150ms"),
            Self::Hid(msg) => write!(f, "HID transport error: {msg}"),
            Self::Protocol(err) => write!(f, "protocol validation error: {err}"),
            Self::Ipc(msg) => write!(f, "daemon IPC error: {msg}"),
            Self::Profile(msg) => write!(f, "profile error: {msg}"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<ProtocolError> for DeviceError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<hidapi::HidError> for DeviceError {
    fn from(value: hidapi::HidError) -> Self {
        Self::Hid(value.to_string())
    }
}
