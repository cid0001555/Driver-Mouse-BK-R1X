use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::DeviceError;
use crate::profile::{HardwareProfile, MacroProfileEntry};

pub const WINDOWS_NAMED_PIPE: &str = r"\\.\pipe\bkr1x-daemon";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcWireTrace {
    pub timestamp_unix_ms: u64,
    pub is_out: bool,
    pub report_id: u8,
    pub payload_hex: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Status,
    GetFullState,
    Refresh,
    SetQuickDpi { dpi: u16 },
    SetQuickRate { rate_hz: u16 },
    FlashButtons { raw_slots: [[u8; 4]; 8] },
    FlashMacros { macros: Vec<MacroProfileEntry> },
    ApplyProfile { profile: Box<HardwareProfile> },
    ReloadProfiles,
    OpenGui,
    Shutdown,
}

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
    #[serde(default = "default_true")]
    pub wakeup_on_move: bool,
    #[serde(default = "default_true")]
    pub move_lights_off: bool,
    pub sleep_minutes: u8,
    pub light_mode: String,
    pub active_profile: Option<String>,
    #[serde(default)]
    pub button_slots: Option<[[u8; 4]; 8]>,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok {
        #[serde(skip_serializing_if = "Option::is_none")]
        snapshot: Option<Box<DaemonStatusSnapshot>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        traces: Option<Vec<IpcWireTrace>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        message: String,
    },
}

pub fn is_daemon_running() -> bool {
    matches!(
        send_ipc_request(&IpcRequest::Status),
        Ok(IpcResponse::Ok { .. })
    )
}

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
        exchange_json_line(&mut stream, line.as_bytes())
    }

    #[cfg(windows)]
    {
        let mut pipe = open_windows_pipe_with_retry()?;
        exchange_json_line(&mut pipe, line.as_bytes())
    }
}

fn exchange_json_line<S: std::io::Read + Write>(
    stream: &mut S,
    request_line: &[u8],
) -> Result<IpcResponse, DeviceError> {
    stream
        .write_all(request_line)
        .map_err(|e| DeviceError::Ipc(format!("write failed: {e}")))?;
    stream
        .flush()
        .map_err(|e| DeviceError::Ipc(format!("flush failed: {e}")))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .map_err(|e| DeviceError::Ipc(format!("read failed: {e}")))?;
    serde_json::from_str(&response_line)
        .map_err(|e| DeviceError::Ipc(format!("invalid daemon JSON response: {e}")))
}

#[cfg(windows)]
fn open_windows_pipe_with_retry() -> Result<std::fs::File, DeviceError> {
    use std::fs::OpenOptions;
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    let wide_pipe: Vec<u16> = WINDOWS_NAMED_PIPE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    for attempt in 0..4 {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(WINDOWS_NAMED_PIPE)
        {
            Ok(file) => return Ok(file),
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && attempt < 3 => {
                // SAFETY: `wide_pipe` is a valid null-terminated UTF-16 pipe path.
                unsafe {
                    let _ = WaitNamedPipeW(wide_pipe.as_ptr(), 250);
                }
            }
            Err(err) => {
                return Err(DeviceError::Ipc(format!(
                    "daemon named pipe {WINDOWS_NAMED_PIPE} unavailable ({err})"
                )));
            }
        }
    }

    Err(DeviceError::Ipc(format!(
        "daemon named pipe {WINDOWS_NAMED_PIPE} busy"
    )))
}
