//! Likhi's icon in the notification area, beside the clock.
//!
//! People expect a keyboard like this to live there -- Avro and Bijoy both do -- because it is the
//! one place a background program is visibly present and can be told to go away. Without it there
//! was no way to exit Likhi at all short of Task Manager.
//!
//!   left click     open the Likhi window
//!   right click    Open Likhi / Check for updates / Install update (when one is ready) / Exit Likhi
//!
//! Verified updates are offered from the same icon as a Windows notification. Clicking one runs the
//! installer and Windows asks for permission, because Likhi is installed for the whole machine. The
//! installer is verified again immediately before it runs, because it waited in a folder the user
//! can write to.
//!
//! Its own thread and hidden window, separate from the candidate window: nothing here may ever make
//! a keystroke wait.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteExW, ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP,
    NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, GetSystemMetrics, LoadIconW, LoadImageW, PostMessageW,
    PostThreadMessageW, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow,
    TrackPopupMenu, TranslateMessage, HICON, IDI_INFORMATION, IMAGE_ICON, LR_LOADFROMFILE,
    MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::update::{verify_installer, Ready};

const WM_WAKE: u32 = WM_APP + 10;
/// What the notification area sends back when the icon or its notification is clicked.
const WM_TRAY: u32 = WM_APP + 11;
/// `lParam` when a notification (balloon) is clicked.
const NIN_BALLOONUSERCLICK: u32 = 0x0405;
const ICON_ID: u32 = 1;

const MENU_OPEN: usize = 1;
const MENU_CHECK: usize = 2;
const MENU_INSTALL: usize = 3;
const MENU_EXIT: usize = 4;

enum Cmd {
    Offer(Ready, Sender<bool>),
    /// A title and a message to show as a notification, e.g. the result of "check for updates".
    Tell(String, String),
}

struct Channel {
    tx: Sender<Cmd>,
    thread_id: u32,
}

static CHANNEL: OnceLock<Option<Mutex<Channel>>> = OnceLock::new();
/// The update currently on offer. Only the tray thread touches it; it is behind a lock because
/// statics must be.
static PENDING: Mutex<Option<Ready>> = Mutex::new(None);
static LOG: OnceLock<fn(&str)> = OnceLock::new();
/// Run just before "Exit Likhi" ends the process: whatever must reach the disk first.
static BEFORE_EXIT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

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

/// `<app>`, found from this executable at `<app>\engine\likhi-server.exe`.
fn app_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()?.parent()?.parent().map(|p| p.to_path_buf())
}

/// The Likhi icon the installer puts beside the text service; Windows' information icon if it is
/// missing, so a moved file never costs the tray icon itself.
///
/// Loaded at the small-icon size, which is what the notification area draws. The default size is
/// the large one, and Windows shrinking a 32-pixel image to 16 is what makes tray icons look soft.
fn load_icon() -> HICON {
    static CACHED: Mutex<isize> = Mutex::new(0);
    if let Ok(c) = CACHED.lock() {
        if *c != 0 {
            return HICON(*c as *mut core::ffi::c_void);
        }
    }
    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
    // Whichever architecture's copy this installation has: an ARM64 machine may carry no x64 one.
    let icon = app_dir()
        .and_then(|a| {
            ["x64", "arm64", "x86"]
                .iter()
                .map(|arch| a.join("shell").join(arch).join("likhi.ico"))
                .find(|p| p.exists())
        })
        .and_then(|p| {
            let wide: Vec<u16> = p.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            unsafe { LoadImageW(None, PCWSTR(wide.as_ptr()), IMAGE_ICON, cx, cy, LR_LOADFROMFILE) }
                .ok()
                .map(|h| HICON(h.0))
        })
        .unwrap_or_else(|| unsafe { LoadIconW(None, IDI_INFORMATION) }.unwrap_or_default());
    if let Ok(mut c) = CACHED.lock() {
        *c = icon.0 as isize;
    }
    icon
}

fn base_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

/// Put the icon in the notification area, or refresh it after Explorer restarted.
fn add_icon(hwnd: HWND) -> bool {
    let mut nid = base_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = load_icon();
    let tip = match PENDING.lock().ok().and_then(|p| p.as_ref().map(|r| r.manifest.version.clone())) {
        Some(v) => format!("Likhi -- update {v} ready to install"),
        None => format!("Likhi {} -- Bangla keyboard", crate::update::PRODUCT_VERSION.unwrap_or("dev")),
    };
    wide_into(&mut nid.szTip, &tip);
    unsafe {
        Shell_NotifyIconW(NIM_ADD, &nid).as_bool() || Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool()
    }
}

/// Show a Windows notification from the icon.
fn balloon(hwnd: HWND, title: &str, text: &str) -> bool {
    let mut nid = base_data(hwnd);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    wide_into(&mut nid.szInfoTitle, title);
    wide_into(&mut nid.szInfo, text);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() }
}

fn remove_icon(hwnd: HWND) {
    let nid = base_data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn open_likhi_window() {
    let Some(exe) = app_dir().map(|a| a.join("Likhi.exe")).filter(|p| p.exists()) else {
        log("tray: Likhi.exe not found beside the engine");
        return;
    };
    let wide: Vec<u16> = exe.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
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
        if let Ok(mut p) = PENDING.lock() {
            *p = None;
        }
        add_icon(hwnd);
        return;
    }

    // One installer at a time. A double click would otherwise start two, and two copies of Setup
    // replacing the same files is not a race worth having.
    {
        static LAUNCHED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        let Ok(mut last) = LAUNCHED.lock() else { return };
        if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(120)) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }

    let file: Vec<u16> = ready.path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    // The ordinary verb, NOT "runas". The installer's outer program is deliberately unelevated: it
    // asks for administrator rights for the part that needs them, and keeps an unelevated copy of
    // itself to run the last step -- restarting the engine -- as the person. "runas" elevates that
    // copy too, and the engine it restarted then ran as administrator: measured, on the first real
    // update, as an engine this user could no longer stop. Windows still asks for permission,
    // because the installer requests it itself.
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        hwnd,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: w!("/SILENT /SUPPRESSMSGBOXES /NORESTART"),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // The offer stays in place either way: declining the permission prompt is not reported to us,
    // and someone who declines should find the offer where they left it. When the install does go
    // ahead it stops this engine, and the icon goes with it.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => log(&format!("updates: started the installer for {}", ready.manifest.version)),
        Err(e) => log(&format!("updates: the installer did not start ({e}); still on offer")),
    }
}

/// "Check for updates" from the menu. Off this thread, because a check can take a minute and the
/// icon must keep answering; the result comes back as a notification.
fn check_from_menu() {
    std::thread::spawn(|| {
        let r = crate::update::check_now();
        let (title, text) = match r.status {
            // The offer itself is the notification; nothing more to say.
            "available" => return,
            "up_to_date" => ("Likhi is up to date".to_string(), format!("You have the newest version, {}.", r.current)),
            "dev" => ("Likhi development build".to_string(), "A development build does not update itself.".to_string()),
            _ => (
                "Could not check for updates".to_string(),
                format!("{} Likhi will try again tomorrow.", r.error.unwrap_or_default()),
            ),
        };
        tell(&title, &text);
    });
}

/// Exit Likhi: stop the engine, and keep the keyboard from starting it again until the person asks.
///
/// The marker file is what makes "exit" mean exit. The text service restarts an engine that is not
/// running, which is right after a crash or a Windows update and wrong here -- without the marker,
/// exiting would last until the next keystroke. The next sign-in, opening Likhi, or starting the
/// engine any other way removes it.
fn exit_likhi(hwnd: HWND) {
    let marker = crate::telemetry::local_app_data().join("Likhi").join("exited");
    let _ = std::fs::write(&marker, b"exited from the tray menu\n");
    log("tray: exit chosen; the keyboard will not restart the engine until Likhi is started again");
    if let Some(f) = BEFORE_EXIT.get() {
        f();
    }
    remove_icon(hwnd);
    std::process::exit(0);
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let pending = PENDING.lock().ok().and_then(|p| p.as_ref().map(|r| r.manifest.version.clone()));
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open Likhi"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CHECK, w!("Check for updates"));
        if let Some(v) = pending {
            let label: Vec<u16> = format!("Install update {v}").encode_utf16().chain(std::iter::once(0)).collect();
            let _ = AppendMenuW(menu, MF_STRING, MENU_INSTALL, PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit Likhi"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Without this the menu does not close when the person clicks elsewhere: a documented
        // quirk of notification-area menus.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match chosen.0 as usize {
            MENU_OPEN => open_likhi_window(),
            MENU_CHECK => check_from_menu(),
            MENU_INSTALL => install(hwnd),
            MENU_EXIT => exit_likhi(hwnd),
            _ => {}
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
    let taskbar_created =
        *TASKBAR_CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

    if msg == WM_TRAY {
        match lparam.0 as u32 {
            NIN_BALLOONUSERCLICK => {
                if PENDING.lock().map(|p| p.is_some()).unwrap_or(false) {
                    install(hwnd);
                } else {
                    open_likhi_window();
                }
            }
            WM_LBUTTONUP => open_likhi_window(),
            WM_RBUTTONUP => show_menu(hwnd),
            _ => {}
        }
        return LRESULT(0);
    }
    // Explorer restarted and rebuilt the notification area without us. Put the icon back.
    if msg == taskbar_created && taskbar_created != 0 {
        add_icon(hwnd);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn thread_main(rx: Receiver<Cmd>, ready: Sender<Option<u32>>) {
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
        lpszClassName: w!("LikhiTray"),
        ..Default::default()
    };
    unsafe { RegisterClassExW(&class) };
    // A real, never-shown top-level window rather than a message-only one: the TaskbarCreated
    // broadcast that says Explorer restarted is not delivered to message-only windows.
    let hwnd = match unsafe {
        CreateWindowExW(
            Default::default(),
            w!("LikhiTray"),
            w!("Likhi"),
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
    if !add_icon(hwnd) {
        log("tray: the notification area did not accept the icon");
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
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Offer(r, ack) => {
                        let v = r.manifest.version.clone();
                        if let Ok(mut p) = PENDING.lock() {
                            *p = Some(r);
                        }
                        add_icon(hwnd); // the tooltip now says an update is ready
                        let shown = balloon(
                            hwnd,
                            &format!("Likhi {v} is ready"),
                            "Downloaded and verified. Click to install -- Windows will ask for \
                             permission. Or install later from the Likhi icon by the clock.",
                        );
                        let _ = ack.send(shown);
                    }
                    Cmd::Tell(title, text) => {
                        balloon(hwnd, &title, &text);
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

fn channel() -> Option<&'static Mutex<Channel>> {
    CHANNEL
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let (ready_tx, ready_rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("likhi-tray".into())
                .spawn(move || thread_main(rx, ready_tx))
                .ok()?;
            match ready_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(Some(thread_id)) => Some(Mutex::new(Channel { tx, thread_id })),
                _ => None,
            }
        })
        .as_ref()
}

fn send(cmd: Cmd) -> bool {
    let Some(ch) = channel() else { return false };
    let Ok(ch) = ch.lock() else { return false };
    if ch.tx.send(cmd).is_err() {
        return false;
    }
    unsafe { PostThreadMessageW(ch.thread_id, WM_WAKE, WPARAM(0), LPARAM(0)).is_ok() }
}

/// Put the icon beside the clock. Called once at startup.
pub fn start(log_fn: fn(&str)) {
    let _ = LOG.set(log_fn);
    // Starting the engine is also how someone undoes "Exit Likhi", so the marker goes here.
    let marker = crate::telemetry::local_app_data().join("Likhi").join("exited");
    let _ = std::fs::remove_file(marker);
    let _ = channel();
}

/// Show a notification from the icon.
pub fn tell(title: &str, text: &str) {
    send(Cmd::Tell(title.to_string(), text.to_string()));
}

/// Put a verified update in front of the person. Returns whether it was actually shown, so the
/// daily check does not record an offer nobody saw.
pub fn offer(ready: &Ready) -> bool {
    let (ack_tx, ack_rx) = mpsc::channel();
    if !send(Cmd::Offer(ready.clone(), ack_tx)) {
        return false;
    }
    ack_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or(false)
}

/// Point this module's log lines at the engine's log. Harmless to call more than once.
pub fn set_log(f: fn(&str)) {
    let _ = LOG.set(f);
}

/// What to do just before "Exit Likhi" ends the process. Only the first call takes effect.
pub fn before_exit(f: impl Fn() + Send + Sync + 'static) {
    let _ = BEFORE_EXIT.set(Box::new(f));
}
