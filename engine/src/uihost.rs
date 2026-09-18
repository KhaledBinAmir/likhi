//! The candidate window, drawn by the engine on a text service's behalf.
//!
//! Needed because of a restriction that cannot be worked around from inside the application: a
//! window created by a process in an AppContainer never reaches the desktop. `CreateWindowExW`
//! returns a handle, `SetWindowPos` succeeds, and nothing is ever composed. Measured on Unigram,
//! where the text service reported showing a list at the right coordinates on every keystroke while
//! the window did not exist on the desktop at all. WhatsApp, which is also a Store application but
//! runs at full trust, is unaffected -- so the line is the sandbox, not the Store.
//!
//! The engine is an ordinary user process, so its windows do reach the desktop and a topmost one
//! sits above the sandboxed application. This is what Microsoft's own IMEs do for the same reason.
//!
//! Only the drawing moves. Every decision -- what the candidates are, which is highlighted, when to
//! commit -- stays in the text service, which is the only thing that knows what is being typed. This
//! module is a remote control for a window and nothing more.
//!
//! One window, not one per client: only one application has keyboard focus at a time, so only one
//! list can be on screen. The last request wins.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, PostThreadMessageW, SetTimer, TranslateMessage, MSG,
    WM_APP, WM_TIMER,
};

use likhi_ui::{CandidateWindow, Fonts};

/// Wakes the UI thread to drain the command queue. The commands themselves travel by channel:
/// a message can carry two machine words, and a candidate list is neither.
const WM_UI_WAKE: u32 = WM_APP + 1;

enum Command {
    Show { items: Vec<String>, cursor: usize, rect: RECT },
    Hide,
}

struct Host {
    tx: Sender<Command>,
    thread_id: u32,
}

static HOST: OnceLock<Option<Mutex<Host>>> = OnceLock::new();

/// Font settings for the window, read from the same config the text service reads.
fn fonts() -> (u128, Fonts) {
    let stamp = crate::telemetry::config_stamp();
    let cfg = crate::telemetry::shell_config();
    let name = cfg
        .as_ref()
        .and_then(|c| c.get("font_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let size = cfg
        .as_ref()
        .and_then(|c| c.get("font_size"))
        .and_then(|v| v.as_f64())
        .unwrap_or(14.0) as f32;
    (stamp, Fonts { font_name: name, font_size: size })
}

/// Start the UI thread, once. Returns false when there is no usable window, which is the normal
/// answer on a machine with no desktop -- a service account, or a session that has not signed in.
fn host() -> Option<&'static Mutex<Host>> {
    HOST.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Command>();
        let (ready_tx, ready_rx) = mpsc::channel::<Option<u32>>();

        // Its own thread with its own message loop. It cannot share the engine's request threads:
        // a window must be pumped, and those threads are blocked on a socket or a pipe.
        std::thread::Builder::new()
            .name("likhi-ui".into())
            .spawn(move || ui_thread(rx, ready_tx))
            .ok()?;

        // Wait for the window to exist before anyone is told there is one. A show that arrived
        // first would be dropped, and the first word someone typed would have no list.
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Some(thread_id)) => Some(Mutex::new(Host { tx, thread_id })),
            _ => None,
        }
    })
    .as_ref()
}

fn ui_thread(rx: Receiver<Command>, ready: Sender<Option<u32>>) {
    let hinstance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h.into(),
        Err(_) => {
            let _ = ready.send(None);
            return;
        }
    };
    let Some(window) = CandidateWindow::new(hinstance, Box::new(fonts)) else {
        let _ = ready.send(None);
        return;
    };
    let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    if ready.send(Some(thread_id)).is_err() {
        return;
    }

    let mut msg = MSG::default();
    let mut shown_at: Option<std::time::Instant> = None;
    let mut timer: usize = 0;

    loop {
        // GetMessage blocks, so this thread costs nothing while nobody is typing.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        if msg.message == WM_UI_WAKE {
            // Drain: several keystrokes can arrive between two wakes, and only the last matters.
            let mut last: Option<Command> = None;
            while let Ok(cmd) = rx.try_recv() {
                last = Some(cmd);
            }
            match last {
                Some(Command::Show { items, cursor, rect }) => {
                    window.show(&items, cursor, &rect);
                    shown_at = Some(std::time::Instant::now());
                    if timer == 0 {
                        timer = unsafe { SetTimer(None, 0, WATCHDOG_TICK_MS, None) };
                    }
                }
                Some(Command::Hide) => {
                    window.hide();
                    shown_at = None;
                    if timer != 0 {
                        let _ = unsafe { KillTimer(None, timer) };
                        timer = 0;
                    }
                }
                None => {}
            }
        }
        // The watchdog. A thread timer, so the message arrives with no window attached.
        //
        // This window outlives the process that asked for it, so a text service that is killed
        // mid-word -- the application crashes, or is closed while something is being composed --
        // would leave a list on the desktop that nothing owns and nobody can dismiss. The ordinary
        // path hides it long before this fires.
        if msg.message == WM_TIMER && msg.hwnd.is_invalid() {
            if let Some(at) = shown_at {
                if at.elapsed() > WATCHDOG_AFTER {
                    log::hide_stale();
                    window.hide();
                    shown_at = None;
                    if timer != 0 {
                        let _ = unsafe { KillTimer(None, timer) };
                        timer = 0;
                    }
                }
            }
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// How often the watchdog looks, and how long a list may sit untouched before it is taken down.
///
/// Generous on purpose: someone who types half a word and stops to think should still see their
/// candidates. This is only here to catch a client that will never send `ui_hide` because it is
/// gone.
const WATCHDOG_TICK_MS: u32 = 2_000;
const WATCHDOG_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

mod log {
    /// Separate so the message is written once and this stays out of the hot path above.
    pub fn hide_stale() {
        eprintln!("[likhi-server] candidate list hidden after 20s with no update; its client is gone");
    }
}

fn send(cmd: Command) -> bool {
    let Some(host) = host() else { return false };
    let Ok(host) = host.lock() else { return false };
    if host.tx.send(cmd).is_err() {
        return false;
    }
    unsafe { PostThreadMessageW(host.thread_id, WM_UI_WAKE, WPARAM(0), LPARAM(0)).is_ok() }
}

/// Show `items` at `rect`, in screen coordinates, with `cursor` highlighted.
pub fn show(items: Vec<String>, cursor: usize, rect: RECT) -> bool {
    if items.is_empty() {
        return hide();
    }
    send(Command::Show { items, cursor, rect })
}

pub fn hide() -> bool {
    send(Command::Hide)
}

/// True when a window exists and can be driven. Used to answer the text service honestly, so it
/// knows whether to keep drawing its own.
pub fn available() -> bool {
    host().is_some()
}

// A window handle that is never read outside the UI thread; present so the unused-import lint does
// not fire on HWND in builds where the window fails to create.
#[allow(dead_code)]
fn _unused(_: HWND) {}
