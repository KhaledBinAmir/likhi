//! Offering a verified update: a notification area icon and a Windows notification.
//!
//! Clicking it runs the installer, and Windows asks for permission, because Likhi is installed for
//! the whole machine. That prompt is the right amount of friction for a program that is about to
//! replace itself: nothing is installed behind anyone's back, and nothing is installed without the
//! installer having been verified twice -- once when it was downloaded, and again here immediately
//! before it runs, because it spent the time in between in a folder the user can write to.
//!
//! Its own thread and hidden window, separate from the candidate window. An update offer is rare
//! and unrelated to typing, and it must never make a keystroke wait.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteExW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, LoadIconW, LoadImageW,
    PostThreadMessageW, RegisterClassExW, RegisterWindowMessageW, TranslateMessage, HICON,
    IDI_INFORMATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MSG, SW_SHOWNORMAL, WM_APP,
    WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::update::{verify_installer, Ready};

const WM_WAKE: u32 = WM_APP + 10;
/// What the notification area sends back when the icon or its notification is clicked.
const WM_TRAY: u32 = WM_APP + 11;
/// Sent when the notification or its icon is clicked: the value `lParam` carries.
const NIN_BALLOONUSERCLICK: u32 = 0x0405;
const ICON_ID: u32 = 1;

struct Channel {
    tx: Sender<(Ready, Sender<bool>)>,
    thread_id: u32,
}

static CHANNEL: OnceLock<Option<Mutex<Channel>>> = OnceLock::new();
/// The update currently on offer, for the window procedure. Only the notification thread touches
/// it, so the lock is never contended; it is a lock because statics require one.
static PENDING: Mutex<Option<Ready>> = Mutex::new(None);
static WINDOW: Mutex<isize> = Mutex::new(0);
static LOG: OnceLock<fn(&str)> = OnceLock::new();

fn log(msg: &str) {
    if let Some(f) = LOG.get() {
        f(msg);
    }
}

fn wide_into<const N: usize>(dst: &mut [u16; N], text: &str) {
    let mut i = 0;
    for u in text.encode_utf16() {
        if i + 1 >= N {
            break;
        }
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
}

/// The Likhi icon the installer puts beside the text service, found relative to this executable:
/// `<app>\engine\likhi-server.exe` sits beside `<app>\shell\x64\likhi.ico`. Windows' information
/// icon if it is missing, so a moved file never costs the notification itself.
fn load_icon() -> HICON {
    let path = std::env::current_exe().ok().and_then(|exe| {
        let p = exe.parent()?.parent()?.join("shell").join("x64").join("likhi.ico");
        p.exists().then_some(p)
    });
    if let Some(p) = path {
        let wide: Vec<u16> = p.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(h) = unsafe {
            LoadImageW(None, PCWSTR(wide.as_ptr()), IMAGE_ICON, 0, 0, LR_LOADFROMFILE | LR_DEFAULTSIZE)
        } {
            return HICON(h.0);
        }
    }
    unsafe { LoadIconW(None, IDI_INFORMATION) }.unwrap_or_default()
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

fn show(hwnd: HWND, ready: &Ready, modify: bool) -> bool {
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE | NIF_INFO;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = load_icon();
    nid.dwInfoFlags = NIIF_INFO;
    let v = &ready.manifest.version;
    wide_into(&mut nid.szTip, &format!("Likhi {v} is ready to install"));
    wide_into(&mut nid.szInfoTitle, &format!("Likhi {v} is ready"));
    // Says what will happen next, including the permission prompt, so the prompt is expected
    // rather than alarming.
    wide_into(
        &mut nid.szInfo,
        "Downloaded and verified. Click to install -- Windows will ask for permission. \
         Right-click the icon to be reminded tomorrow instead.",
    );
    unsafe { Shell_NotifyIconW(if modify { NIM_MODIFY } else { NIM_ADD }, &nid).as_bool() }
}

fn remove(hwnd: HWND) {
    let nid = icon_data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Run the installer, after checking it one last time.
fn install(hwnd: HWND) {
    let Some(ready) = PENDING.lock().ok().and_then(|p| p.clone()) else { return };

    // Verified again here, at the last moment. It was verified when it was downloaded, but it has
    // been sitting in a folder the user can write to since, and the check that counts is the one
    // made immediately before the file runs with administrator rights.
    let ok = std::fs::read(&ready.path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| verify_installer(&bytes, &ready.manifest));
    if let Err(e) = ok {
        log(&format!("updates: refusing to run {}: {e}", ready.path.display()));
        remove(hwnd);
        if let Ok(mut p) = PENDING.lock() {
            *p = None;
        }
        return;
    }

    // One installer at a time. A double click on the notification would otherwise start two, and
    // two copies of Setup replacing the same files is not a race worth having.
    {
        static LAUNCHED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        let Ok(mut last) = LAUNCHED.lock() else { return };
        if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(120)) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }

    let file: Vec<u16> = ready.path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    // /SILENT shows progress and asks nothing; /SUPPRESSMSGBOXES keeps it from stopping to ask;
    // /NORESTART because the installer no longer needs one and must never force one.
    let params = w!("/SILENT /SUPPRESSMSGBOXES /NORESTART");
    // The ordinary verb, NOT "runas". The installer's outer program is deliberately unelevated: it
    // asks for administrator rights for the part that needs them, and keeps an unelevated copy of
    // itself to run the last step -- restarting the engine -- as the person rather than as an
    // administrator. "runas" elevates that outer copy too, and the engine it restarted then ran as
    // administrator: measured, on the first real update, as an engine this user could no longer
    // stop. Windows still asks for permission, because the installer requests it itself.
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        hwnd,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: params,
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // The offer is left in place either way. With the ordinary verb the permission prompt belongs to
    // the installer, so declining it is not reported here -- and someone who declines should still
    // find the offer where they left it. When the install does go ahead it stops this engine, and
    // the icon goes with it.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => log(&format!("updates: started the installer for {}", ready.manifest.version)),
        Err(e) => log(&format!("updates: the installer did not start ({e}); still on offer")),
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
    let taskbar_created =
        *TASKBAR_CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

    if msg == WM_TRAY {
        match lparam.0 as u32 {
            NIN_BALLOONUSERCLICK | WM_LBUTTONUP => install(hwnd),
            // Not now. The daily check offers it again tomorrow.
            WM_RBUTTONUP => remove(hwnd),
            _ => {}
        }
        return LRESULT(0);
    }
    // Explorer restarted and the notification area was rebuilt without us. Put the offer back.
    if msg == taskbar_created && taskbar_created != 0 {
        if let Some(r) = PENDING.lock().ok().and_then(|p| p.clone()) {
            show(hwnd, &r, false);
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn thread_main(rx: Receiver<(Ready, Sender<bool>)>, ready: Sender<Option<u32>>) {
    let hinstance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h.into(),
        Err(_) => {
            let _ = ready.send(None);
            return;
        }
    };
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("LikhiUpdate"),
        ..Default::default()
    };
    unsafe { RegisterClassExW(&class) };
    // A real, never-shown top-level window rather than a message-only one: the TaskbarCreated
    // broadcast that says Explorer restarted is not delivered to message-only windows.
    let hwnd = match unsafe {
        CreateWindowExW(
            Default::default(),
            w!("LikhiUpdate"),
            w!("Likhi update"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(h) => h,
        Err(_) => {
            let _ = ready.send(None);
            return;
        }
    };
    if let Ok(mut w) = WINDOW.lock() {
        *w = hwnd.0 as isize;
    }
    let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    if ready.send(Some(thread_id)).is_err() {
        return;
    }

    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        if msg.message == WM_WAKE {
            while let Ok((r, ack)) = rx.try_recv() {
                let modify = PENDING.lock().map(|p| p.is_some()).unwrap_or(false);
                let shown = show(hwnd, &r, modify) || show(hwnd, &r, !modify);
                if shown {
                    if let Ok(mut p) = PENDING.lock() {
                        *p = Some(r);
                    }
                }
                let _ = ack.send(shown);
            }
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn channel() -> Option<&'static Mutex<Channel>> {
    CHANNEL
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let (ready_tx, ready_rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("likhi-notify".into())
                .spawn(move || thread_main(rx, ready_tx))
                .ok()?;
            match ready_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(Some(thread_id)) => Some(Mutex::new(Channel { tx, thread_id })),
                _ => None,
            }
        })
        .as_ref()
}

/// Put a verified update in front of the person. Returns whether it was actually shown, so the
/// daily check does not record an offer nobody saw.
pub fn offer(ready: &Ready) -> bool {
    let Some(ch) = channel() else { return false };
    let Ok(ch) = ch.lock() else { return false };
    let (ack_tx, ack_rx) = mpsc::channel();
    if ch.tx.send((ready.clone(), ack_tx)).is_err() {
        return false;
    }
    unsafe {
        let _ = PostThreadMessageW(ch.thread_id, WM_WAKE, WPARAM(0), LPARAM(0));
    }
    ack_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or(false)
}

/// Point this module's log lines at the engine's log. Harmless to call more than once.
pub fn set_log(f: fn(&str)) {
    let _ = LOG.set(f);
}

#[allow(dead_code)]
fn _unused(_: PathBuf) {}
