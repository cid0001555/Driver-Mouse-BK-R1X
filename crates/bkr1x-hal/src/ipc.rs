//! Sub-millisecond JSON-line IPC transport between `bkr1x-daemon`, `bkr1x-cli`, `bkr1x-gui`,
//! and desktop status bars (`waybar`, `polybar`, `i3status`).
//!
//! - **Linux / Unix:** Unix Domain Socket at `$XDG_RUNTIME_DIR/bkr1x.sock` (i.e. `/run/user/$UID/bkr1x.sock`).
//! - **Windows:** Win32 Named Pipe at `\\.\pipe\bkr1x-daemon`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::DeviceError;
use crate::profile::{HardwareProfile, MacroProfileEntry};

/// Windows Named Pipe endpoint path.
pub const WINDOWS_NAMED_PIPE: &str = r"\\.\pipe\bkr1x-daemon";

/// Resolves `/run/user/$UID/bkr1x.sock` (via `$XDG_RUNTIME_DIR`) on Linux/Unix.
pub fn unix_socket_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("bkr1x.sock");
    }
    #[cfg(unix)]
    {
        let uid = std::env::var("UID").unwrap_or_else(|_| "1000".to_owned());
        let run_user = PathBuf::from(format!("/run/user/{uid}"));
        if run_user.is_dir() {
            return run_user.join("bkr1x.sock");
        }
        PathBuf::from(format!("/tmp/bkr1x-{uid}.sock"))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(WINDOWS_NAMED_PIPE)
    }
}

/// Serializable wire trace frame transferred from `bkr1x-daemon` to `bkr1x-gui`'s Raw HID Debugger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcWireTrace {
    pub timestamp_unix_ms: u64,
    pub is_out: bool,
    pub report_id: u8,
    pub payload_hex: String,
    pub summary: String,
}

/// JSON-line request sent to `bkr1x-daemon`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    /// Returns cached status immediately in < 1ms without waking USB/2.4GHz bus.
    Status,
    /// Returns full hardware state (config, buttons, VID/PID, recent raw HID frames) for `bkr1x-gui`.
    GetFullState,
    /// Forces an immediate hardware battery + config + buttons refresh from the mouse.
    Refresh,
    /// Switches active DPI immediately (used by System Tray quick menu & CLI).
    SetQuickDpi { dpi: u16 },
    /// Switches polling rate immediately (used by System Tray quick menu & CLI).
    SetQuickRate { rate_hz: u16 },
    /// Flashes 8 × 4-byte button mappings through the daemon's single HID handle.
    FlashButtons { raw_slots: [[u8; 4]; 8] },
    /// Compiles and flashes hardware macros through the daemon's single HID handle.
    FlashMacros { macros: Vec<MacroProfileEntry> },
    /// Applies a validated [`HardwareProfile`] through the daemon's open handle.
    ApplyProfile { profile: Box<HardwareProfile> },
    /// Reloads saved per-application game profiles from `%APPDATA%\bkr1x\profiles`.
    ReloadProfiles,
    /// Asks the running tray daemon to launch or focus `bkr1x-gui`.
    OpenGui,
    /// Requests clean daemon & tray shutdown.
    Shutdown,
}

/// Cached telemetry & full configuration snapshot returned by `bkr1x-daemon`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatusSnapshot {
    pub connected: bool,
    pub online: bool,
    pub connection_mode: String,
    #[serde(default)]
    pub vendor_id: u16,
    #[serde(default)]
    pub product_id: u16,
    #[serde(default)]
    pub product_name: String,
    pub firmware_version: String,
    pub battery_percent: u8,
    pub charging: bool,
    pub active_dpi: u16,
    pub active_dpi_stage: u8,
    pub dpi_stages: Vec<u16>,
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
    #[serde(default = "bool_true")]
    pub wakeup_on_move: bool,
    #[serde(default = "bool_true")]
    pub move_lights_off: bool,
    pub sleep_minutes: u8,
    pub light_mode: String,
    pub active_profile: Option<String>,
    #[serde(default)]
    pub button_slots: Option<[[u8; 4]; 8]>,
}

fn bool_true() -> bool {
    true
}

/// JSON-line response emitted by `bkr1x-daemon`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum IpcResponse {
    Ok {
        #[serde(skip_serializing_if = "Option::is_none")]
        snapshot: Option<DaemonStatusSnapshot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        traces: Option<Vec<IpcWireTrace>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        message: String,
    },
}

/// Checks whether `bkr1x-daemon` is currently reachable over IPC.
pub fn is_daemon_running() -> bool {
    matches!(send_ipc_request(&IpcRequest::Status), Ok(IpcResponse::Ok { .. }))
}

/// Sends a single IPC request to the running `bkr1x-daemon` and reads its JSON-line reply.
pub fn send_ipc_request(req: &IpcRequest) -> Result<IpcResponse, DeviceError> {
    let mut line =
        serde_json::to_string(req).map_err(|e| DeviceError::Ipc(format!("serialize: {e}")))?;
    line.push('\n');

    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        let path = unix_socket_path();
        let mut stream = UnixStream::connect(&path).map_err(|e| {
            DeviceError::Ipc(format!(
                "daemon socket {} unavailable ({e})",
                path.display()
            ))
        })?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(2500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        stream
            .write_all(line.as_bytes())
            .map_err(|e| DeviceError::Ipc(format!("write failed: {e}")))?;
        stream
            .flush()
            .map_err(|e| DeviceError::Ipc(format!("flush failed: {e}")))?;

        let mut reader = BufReader::new(stream);
        let mut resp_line = String::new();
        reader
            .read_line(&mut resp_line)
            .map_err(|e| DeviceError::Ipc(format!("read failed: {e}")))?;
        serde_json::from_str(&resp_line)
            .map_err(|e| DeviceError::Ipc(format!("invalid daemon JSON response: {e}")))
    }

    #[cfg(windows)]
    {
        use std::fs::OpenOptions;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(WINDOWS_NAMED_PIPE)
            .map_err(|e| {
                DeviceError::Ipc(format!(
                    "daemon named pipe {WINDOWS_NAMED_PIPE} unavailable ({e})"
                ))
            })?;

        let mut writer = &file;
        writer
            .write_all(line.as_bytes())
            .map_err(|e| DeviceError::Ipc(format!("pipe write failed: {e}")))?;
        writer
            .flush()
            .map_err(|e| DeviceError::Ipc(format!("pipe flush failed: {e}")))?;

        let mut reader = BufReader::new(&file);
        let mut resp_line = String::new();
        reader
            .read_line(&mut resp_line)
            .map_err(|e| DeviceError::Ipc(format!("pipe read failed: {e}")))?;
        serde_json::from_str(&resp_line)
            .map_err(|e| DeviceError::Ipc(format!("invalid daemon JSON response: {e}")))
    }
}
