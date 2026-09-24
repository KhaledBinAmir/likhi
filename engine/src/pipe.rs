//! A named pipe the keyboard can reach from inside a Store application.
//! A port of `src/likhi/pipe.py`.
//!
//! An application from the Microsoft Store -- Telegram, WhatsApp, Mail -- runs in an AppContainer,
//! and an AppContainer cannot open a loopback socket. The text service runs *inside* that
//! application, so its connection to 127.0.0.1 fails and the keyboard falls back to typing Latin.
//! A named pipe has no such restriction, provided two things are right, and both fail silently when
//! they are not:
//!
//! * the DACL must grant the application-package SIDs. An AppContainer token carries restricted
//!   SIDs, so an ACE naming the user is not enough on its own. Two SIDs, not one: `AC` is ALL
//!   APPLICATION PACKAGES, and `S-1-15-2-2` is ALL RESTRICTED APPLICATION PACKAGES, which is what a
//!   Less Privileged AppContainer carries instead -- Edge and a growing number of Store apps do not
//!   have `AC` in their token at all.
//! * the pipe must carry a *low* mandatory integrity label. AppContainers run at low integrity, and
//!   Windows forbids a low-integrity process from writing to a medium-integrity object whatever the
//!   DACL says. Without the label the connection is refused after passing every other check.
//!
//! Both live in the one SDDL string below, which is shared with the Python and must stay identical:
//! whichever engine is running, the keyboard has to be able to reach it.
//!
//! The pipe name carries the Windows session id, so two people signed in to one machine each get
//! their own engine and their own personal dictionary rather than fighting over one pipe.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    FlushFileBuffers, ReadFile, WriteFile, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;

/// SYSTEM and administrators get everything; the owner -- whoever started the engine -- gets
/// everything; the two application-package SIDs get read and write, which is the point. The SACL
/// sets a low mandatory label with no write-up restriction, so an AppContainer at low integrity may
/// talk to a pipe created at medium.
const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;GRGW;;;AC)(A;;GRGW;;;S-1-15-2-2)S:(ML;;NW;;;LW)";

const BUFFER_BYTES: u32 = 64 * 1024;
const ERROR_PIPE_CONNECTED: u32 = 535;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A kernel handle, moved to the thread that will serve it.
///
/// `HANDLE` wraps a raw pointer and so is not `Send`, but a handle is an index into the process's
/// object table, not a pointer into this thread's memory: moving one to another thread is exactly
/// what the Win32 API expects. Ownership still moves with it -- only the receiving thread closes it.
struct SendHandle(HANDLE);
// SAFETY: see above. The handle is used by one thread at a time and closed exactly once.
unsafe impl Send for SendHandle {}

/// The security descriptor, shared by every pipe instance for the life of the process.
///
/// Same reasoning: the pointer is to a process-wide allocation that is never freed and never
/// written after construction, so sharing it across threads is sound.
struct SendDescriptor(PSECURITY_DESCRIPTOR);
// SAFETY: read-only after construction, leaked deliberately, never aliased mutably.
unsafe impl Send for SendDescriptor {}

pub fn session_id() -> u32 {
    let mut out = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut out);
    }
    out
}

pub fn pipe_name() -> String {
    format!(r"\\.\pipe\likhi-engine-s{}", session_id())
}

/// Whether an engine is already serving this session's pipe.
///
/// Asked without connecting, so the running engine never sees a client arrive and leave:
/// `WaitNamedPipeW` answers from the pipe's existence, and a pipe whose instances are all busy
/// still exists. This replaced a ping over the socket, which on Windows cost a full second at every
/// start -- a connection to a port nobody listens on is retried rather than refused, measured at
/// 2 s, and the ping's 1 s timeout was always what ended it.
pub fn engine_present() -> bool {
    const ERROR_SEM_TIMEOUT: u32 = 121;
    let name = wide(&pipe_name());
    if unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 1) }.as_bool() {
        return true;
    }
    unsafe { GetLastError() }.0 == ERROR_SEM_TIMEOUT
}

/// Accept connections until `stop` is set, answering each line with `handle`.
///
/// Returns the pipe name it is listening on. The accept loop runs on its own thread and each
/// connection gets another: the engine answers within a deadline and connections are few -- one per
/// application with the keyboard active -- so a thread each is simpler than overlapped I/O and
/// costs nothing that matters.
pub fn serve<F>(handle: F, stop: Arc<AtomicBool>) -> Result<String, String>
where
    F: Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
{
    let name = pipe_name();
    let descriptor = unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide(SDDL).as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(|e| format!("security descriptor: {e}"))?;
        sd
    };
    // Deliberately never freed: it is referenced for the life of every pipe instance created from
    // it, and the engine holds those for the life of the process.
    let descriptor = SendDescriptor(descriptor);

    let handle = Arc::new(handle);
    let loop_name = name.clone();
    std::thread::Builder::new()
        .name("likhi-pipe".into())
        .spawn(move || {
            let descriptor = descriptor;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0 .0,
                bInheritHandle: false.into(),
            };
            let wname = wide(&loop_name);
            while !stop.load(Ordering::Relaxed) {
                let pipe = unsafe {
                    CreateNamedPipeW(
                        PCWSTR(wname.as_ptr()),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        BUFFER_BYTES,
                        BUFFER_BYTES,
                        0,
                        Some(&sa),
                    )
                };
                if pipe == INVALID_HANDLE_VALUE {
                    // Out of instances, or a transient failure. Back off rather than spin.
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                let connected = unsafe { ConnectNamedPipe(pipe, None) };
                // A client that connected between CreateNamedPipe and ConnectNamedPipe reports
                // ERROR_PIPE_CONNECTED, which is success.
                if connected.is_err() && unsafe { GetLastError() }.0 != ERROR_PIPE_CONNECTED {
                    unsafe { let _ = CloseHandle(pipe); }
                    continue;
                }
                if stop.load(Ordering::Relaxed) {
                    unsafe { let _ = CloseHandle(pipe); }
                    return;
                }
                let h = Arc::clone(&handle);
                let owned = SendHandle(pipe);
                let _ = std::thread::Builder::new()
                    .name("likhi-pipe-conn".into())
                    .spawn(move || {
                        // Bound whole, not by field: Rust 2021's disjoint capture would otherwise
                        // take the inner `HANDLE` and lose the `Send` the wrapper exists to carry.
                        let owned = owned;
                        serve_client(owned.0, h.as_ref())
                    });
            }
        })
        .map_err(|e| format!("cannot start the pipe thread: {e}"))?;

    Ok(name)
}

/// One connection, many request lines, until the client goes away.
fn serve_client<F>(pipe: HANDLE, handle: &F)
where
    F: Fn(&[u8]) -> Vec<u8>,
{
    let mut pending: Vec<u8> = Vec::with_capacity(1024);
    let mut buf = vec![0u8; BUFFER_BYTES as usize];
    loop {
        let mut read = 0u32;
        let ok = unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut read), None) };
        if ok.is_err() || read == 0 {
            break;
        }
        pending.extend_from_slice(&buf[..read as usize]);
        while let Some(at) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=at).collect();
            let line = &line[..line.len() - 1];
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let mut reply = handle(line);
            reply.push(b'\n');
            let mut written = 0u32;
            let sent = unsafe { WriteFile(pipe, Some(&reply), Some(&mut written), None) };
            if sent.is_err() {
                unsafe {
                    let _ = FlushFileBuffers(pipe);
                    let _ = DisconnectNamedPipe(pipe);
                    let _ = CloseHandle(pipe);
                }
                return;
            }
        }
    }
    unsafe {
        let _ = FlushFileBuffers(pipe);
        let _ = DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
    }
}
