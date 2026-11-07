use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bkr1x_hal::{DaemonStatusSnapshot, IpcRequest, IpcResponse};

use crate::DaemonCommand;
#[cfg(windows)]
use crate::tray_win::TrayNotifier;

#[cfg(windows)]
const MAX_IPC_REQUEST_BYTES: usize = 64 * 1024;

fn dispatch_sync<F>(cmd_tx: &Sender<DaemonCommand>, make_cmd: F) -> IpcResponse
where
    F: FnOnce(SyncSender<IpcResponse>) -> DaemonCommand,
{
    let (tx, rx) = mpsc::sync_channel(1);
    if cmd_tx.send(make_cmd(tx)).is_err() {
        return IpcResponse::Error {
            message: "daemon shutting down".to_owned(),
        };
    }
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| IpcResponse::Error {
            message: "hardware command timed out".to_owned(),
        })
}

fn read_boxed_snapshot(
    shared_snapshot: &Arc<RwLock<DaemonStatusSnapshot>>,
) -> Option<Box<DaemonStatusSnapshot>> {
    shared_snapshot
        .read()
        .ok()
        .map(|guard| Box::new(guard.clone()))
}

pub(crate) fn handle_ipc_line(
    line: &str,
    shared_snapshot: &Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: &Sender<DaemonCommand>,
    running: &Arc<AtomicBool>,
    #[cfg(windows)] tray: &TrayNotifier,
) -> IpcResponse {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return IpcResponse::Ok {
            snapshot: read_boxed_snapshot(shared_snapshot),
            traces: None,
            message: None,
        };
    }

    let req: IpcRequest = match serde_json::from_str(trimmed) {
        Ok(parsed) => parsed,
        Err(err) => {
            return IpcResponse::Error {
                message: format!("invalid JSON request: {err}"),
            };
        }
    };

    match req {
        IpcRequest::Status => IpcResponse::Ok {
            snapshot: read_boxed_snapshot(shared_snapshot),
            traces: None,
            message: None,
        },
        IpcRequest::GetFullState => {
            dispatch_sync(cmd_tx, |reply| DaemonCommand::GetFullState { reply })
        }
        IpcRequest::Refresh => dispatch_sync(cmd_tx, |reply| DaemonCommand::Refresh { reply }),
        IpcRequest::SetQuickDpi { dpi } => dispatch_sync(cmd_tx, move |reply| {
            DaemonCommand::SetQuickDpi { dpi, reply }
        }),
        IpcRequest::SetQuickRate { rate_hz } => dispatch_sync(cmd_tx, move |reply| {
            DaemonCommand::SetQuickRate { rate_hz, reply }
        }),
        IpcRequest::FlashButtons { raw_slots } => dispatch_sync(cmd_tx, move |reply| {
            DaemonCommand::FlashButtons { raw_slots, reply }
        }),
        IpcRequest::FlashMacros { macros } => dispatch_sync(cmd_tx, move |reply| {
            DaemonCommand::FlashMacros { macros, reply }
        }),
        IpcRequest::ApplyProfile { profile } => dispatch_sync(cmd_tx, move |reply| {
            DaemonCommand::ApplyProfile { profile, reply }
        }),
        IpcRequest::ReloadProfiles => {
            let _ = cmd_tx.send(DaemonCommand::ReloadProfiles);
            IpcResponse::Ok {
                snapshot: None,
                traces: None,
                message: Some("reloaded profiles".to_owned()),
            }
        }
        IpcRequest::OpenGui => {
            #[cfg(windows)]
            tray.request_open_gui();
            IpcResponse::Ok {
                snapshot: None,
                traces: None,
                message: Some("opened configurator".to_owned()),
            }
        }
        IpcRequest::Shutdown => {
            running.store(false, Ordering::SeqCst);
            let _ = cmd_tx.send(DaemonCommand::Shutdown);
            #[cfg(windows)]
            tray.request_quit();
            IpcResponse::Ok {
                snapshot: None,
                traces: None,
                message: Some("shutting down".to_owned()),
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn run_ipc_listener(
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
) {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let sock_path = bkr1x_hal::unix_socket_path();
    let _ = fs::remove_file(&sock_path);
    let Ok(listener) = UnixListener::bind(&sock_path) else {
        return;
    };
    let _ = fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600));

    for stream_res in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        let Ok(mut stream) = stream_res else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        let mut line = String::new();
        if let Ok(cloned) = stream.try_clone() {
            let mut reader = BufReader::new(cloned);
            let _ = reader.read_line(&mut line);
        }
        let resp = handle_ipc_line(&line, &shared_snapshot, &cmd_tx, &running);
        if let Ok(mut json) = serde_json::to_string(&resp) {
            json.push('\n');
            let _ = stream.write_all(json.as_bytes());
            let _ = stream.flush();
        }
    }
}

#[cfg(windows)]
pub(crate) fn run_ipc_listener(
    shared_snapshot: Arc<RwLock<DaemonStatusSnapshot>>,
    cmd_tx: Sender<DaemonCommand>,
    running: Arc<AtomicBool>,
    tray: TrayNotifier,
) {
    use std::thread;

    use bkr1x_hal::ipc::WINDOWS_NAMED_PIPE;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = WINDOWS_NAMED_PIPE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    while running.load(Ordering::Relaxed) {
        // SAFETY: `wide_name` is a static null-terminated UTF-16 pipe path; handle is checked for `INVALID_HANDLE_VALUE`.
        let pipe: HANDLE = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                16384,
                16384,
                500,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            thread::sleep(Duration::from_secs(1));
            continue;
        }

        // SAFETY: `pipe` is a valid named pipe handle; `ERROR_PIPE_CONNECTED` indicates the client connected between `CreateNamedPipeW` and `ConnectNamedPipe`.
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 }
            || unsafe { GetLastError() == ERROR_PIPE_CONNECTED };

        if connected && let Some(line) = unsafe { read_pipe_json_line(pipe) } {
            let resp = handle_ipc_line(&line, &shared_snapshot, &cmd_tx, &running, &tray);
            if let Ok(mut out) = serde_json::to_string(&resp) {
                out.push('\n');
                unsafe {
                    write_all_pipe(pipe, out.as_bytes());
                    let _ = FlushFileBuffers(pipe);
                }
            }
        }

        // SAFETY: Always disconnect and close the per-request pipe instance before looping.
        unsafe {
            let _ = DisconnectNamedPipe(pipe);
            let _ = CloseHandle(pipe);
        }
    }
}

#[cfg(windows)]
unsafe fn read_pipe_json_line(pipe: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::ReadFile;

    let mut accum = Vec::with_capacity(512);
    let mut chunk = [0u8; 4096];

    while accum.len() < MAX_IPC_REQUEST_BYTES {
        let mut bytes_read = 0u32;
        let ok = unsafe {
            ReadFile(
                pipe,
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            ) != 0
        };
        if bytes_read == 0 {
            break;
        }
        let slice = &chunk[..bytes_read as usize];
        if let Some(nl_pos) = slice.iter().position(|&b| b == b'\n') {
            accum.extend_from_slice(&slice[..nl_pos]);
            return Some(String::from_utf8_lossy(&accum).into_owned());
        }
        accum.extend_from_slice(slice);
        if !ok {
            break;
        }
    }

    (!accum.is_empty()).then(|| String::from_utf8_lossy(&accum).into_owned())
}

#[cfg(windows)]
unsafe fn write_all_pipe(pipe: windows_sys::Win32::Foundation::HANDLE, mut data: &[u8]) {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;

    while !data.is_empty() {
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                pipe,
                data.as_ptr(),
                data.len().min(u32::MAX as usize) as u32,
                &mut written,
                std::ptr::null_mut(),
            ) != 0
        };
        if !ok || written == 0 {
            break;
        }
        data = &data[written as usize..];
    }
}
