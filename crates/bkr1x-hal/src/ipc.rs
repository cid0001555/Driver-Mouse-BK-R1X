//! Sub-millisecond JSON-line IPC transport between `bkr1x-daemon`, `bkr1x-cli`, `bkr1x-gui`,
//! and desktop status bars (`waybar`, `polybar`, `i3status`).
//!
//! - **Linux / Unix:** Unix Domain Socket at `$XDG_RUNTIME_DIR/bkr1x.sock` (i.e. `/run/user/$UID/bkr1x.sock`).
//! - **Windows:** Win32 Named Pipe at `\\.\pipe\bkr1x-daemon`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::DeviceError;
use crate::profile::HardwareProfile;

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
        // Fallback if XDG_RUNTIME_DIR is unset in minimal window managers
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

/// JSON-line request sent to `bkr1x-daemon`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    /// Returns cached status immediately in < 1ms without waking USB/2.4GHz bus.
    Status,
    /// Forces an immediate hardware battery + config refresh from the mouse.
    Refresh,
    /// Applies a validated [`HardwareProfile`] through the daemon's open handle.
    ApplyProfile { profile: Box<HardwareProfile> },
    /// Requests clean daemon shutdown.
    Shutdown,
}

/// Cached telemetry snapshot returned by `bkr1x-daemon` in < 1ms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatusSnapshot {
    pub connected: bool,
    pub online: bool,
    pub connection_mode: String,
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
    pub sleep_minutes: u8,
    pub light_mode: String,
    pub active_profile: Option<String>,
}

/// JSON-line response emitted by `bkr1x-daemon`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok {
        #[serde(skip_serializing_if = "Option::is_none")]
        snapshot: Option<DaemonStatusSnapshot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        message: String,
    },
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
        let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
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
