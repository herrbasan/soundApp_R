//! Single-instance gate and file hand-off for normal player launches.
//!
//! The first process creates a named mutex and a byte-mode named-pipe server.
//! Later launches (normally a shell file association) forward their file path to
//! that pipe, then exit; the running player loads it and comes forward.

use std::ffi::OsStr;
use std::os::raw::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

const PIPE_NAME: &str = r"\\.\pipe\SoundAppR-single-instance-v1";
const MUTEX_NAME: &str = r"Local\SoundAppR-single-instance-v1";
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_PIPE_CONNECTED: u32 = 535;
const INVALID_HANDLE_VALUE: isize = -1;
const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attributes: *const c_void, owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer: u32,
        in_buffer: u32,
        timeout: u32,
        security: *const c_void,
    ) -> isize;
    fn ConnectNamedPipe(handle: isize, overlapped: *mut c_void) -> i32;
    fn DisconnectNamedPipe(handle: isize) -> i32;
    fn ReadFile(
        handle: isize,
        buffer: *mut u8,
        len: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: isize) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn WriteFile(
        handle: isize,
        buffer: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
}

static MUTEX_HANDLE: OnceLock<isize> = OnceLock::new();

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

/// `true` when this process owns the single-instance mutex.
pub fn claim() -> Result<bool, String> {
    let name = wide(MUTEX_NAME);
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle == 0 {
        return Err(format!("CreateMutexW failed ({})", unsafe { GetLastError() }));
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !already_exists {
        let _ = MUTEX_HANDLE.set(handle);
    }
    Ok(!already_exists)
}

/// Start the pipe listener. Each message is either a path or a blank focus
/// request; the app drains them from the returned receiver.
pub fn start_server(sender: Sender<String>) {
    thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            let name = wide(PIPE_NAME);
            let pipe = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    4096,
                    4096,
                    0,
                    std::ptr::null(),
                )
            };
            if pipe == INVALID_HANDLE_VALUE || pipe == 0 {
                eprintln!("[single-instance] CreateNamedPipeW failed");
                return;
            }
            loop {
                let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0;
                if !connected && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                let mut payload = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let mut read = 0u32;
                    let ok = unsafe {
                        ReadFile(
                            pipe,
                            buffer.as_mut_ptr(),
                            buffer.len() as u32,
                            &mut read,
                            std::ptr::null_mut(),
                        )
                    } != 0;
                    if !ok || read == 0 {
                        break;
                    }
                    payload.extend_from_slice(&buffer[..read as usize]);
                    if read < buffer.len() as u32 {
                        break;
                    }
                }
                let message = String::from_utf8_lossy(&payload).trim().to_string();
                let _ = sender.send(message);
                unsafe {
                    DisconnectNamedPipe(pipe);
                }
            }
        })
        .expect("single-instance listener thread");
}

/// Send this launch's file (or a blank focus request) to the running player.
pub fn forward(path: Option<&Path>) -> bool {
    let name = wide(PIPE_NAME);
    let payload = path
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "\n".to_string());
    for _ in 0..40 {
        if unsafe { WaitNamedPipeW(name.as_ptr(), 100) } == 0 {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle == 0 {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                handle,
                payload.as_ptr(),
                payload.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        } != 0;
        unsafe {
            CloseHandle(handle);
        }
        return ok && written as usize == payload.len();
    }
    false
}
