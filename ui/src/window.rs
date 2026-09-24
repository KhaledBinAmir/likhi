//! The candidate window: our own, drawn with Direct2D and DirectWrite.
//!
//! This is the reason the shell exists. PIME's window paints from `GetSysColor` and exposes a font
//! name, a size and a per-row count; nothing else. This one follows the Windows app theme, has real
//! padding and rounded corners, and shapes Bengali correctly because DirectWrite does -- the vowel
//! sign in "লি" lands before the consonant without anyone having to think about it.
//!
//! One window per text service instance, created lazily on the application's UI thread (which is
//! where TSF calls us), never activated, always on top, hidden when nothing is being composed.
//!
//! It also refines itself. The list shown on a keystroke is whatever the engine had within the
//! typing deadline; when the typist pauses, a timer asks once more with time for the model and
//! redraws. Pilot users typing "myam" saw a list without ম্যাম and reasonably concluded the word
//! was not there -- it was, one full ranking away.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
// Direct2D points are plain vectors from a sibling crate; windows 0.62 has no D2D_POINT_2F alias.
use windows_numerics::Vector2;

use crate::{log, vlog, FontSource, Fonts};

const CLASS_NAME: PCWSTR = w!("LikhiCandidateWindow");

/// Faces to use, best first; the first one installed wins.
///
/// Google Sans does cover Bengali (U+0980-09FE) and is the nicest of these, but it is not in
/// Google's open-licensed font repository, so we may use it where someone already has it and may
/// not ship it. Noto Sans Bengali is under the SIL Open Font License, is drawn for screens, and is
/// what the installer puts on the machine. Nirmala UI is Windows' own Bengali face and is always
/// present, which is what makes it the last resort.
///
/// DirectWrite silently substitutes something arbitrary for a family it cannot find, so the choice
/// is made here against the system font collection and logged, rather than left to chance.
const FONT_CHAIN: &[PCWSTR] = &[
    w!("Google Sans"),
    w!("Noto Sans Bengali"),
    w!("Nirmala UI"),
];
/// Text size in device-independent pixels at 96 dpi; scaled by the monitor's DPI at draw time.
const FONT_DIP: f32 = 14.0;
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 8.0;
/// Between one candidate and the next.
const GAP: f32 = 18.0;
/// Between a candidate's number and its word. Without it the digit crowds the Bangla and the pair
/// reads as one token: "1আমার" rather than "1  আমার".
const NUMBER_GAP: f32 = 5.0;
/// Highlight pill overhang around the selected candidate.
const PILL_X: f32 = 6.0;
const PILL_Y: f32 = 3.0;
const RADIUS: f32 = 6.0;
/// Space between the composition's bottom edge and the window.
const OFFSET_Y: i32 = 4;

/// How long after the last keystroke before the list is refined with the full ranking.
const REFINE_TIMER_ID: usize = 1;
const REFINE_AFTER_MS: u32 = 150;

struct Theme {
    background: D2D1_COLOR_F,
    text: D2D1_COLOR_F,
    number: D2D1_COLOR_F,
    highlight: D2D1_COLOR_F,
    highlight_text: D2D1_COLOR_F,
    border: D2D1_COLOR_F,
}

fn rgb(r: u8, g: u8, b: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

/// Windows' "Choose your mode" for apps, read the way every desktop app does.
fn apps_use_dark_theme() -> bool {
    unsafe {
        let mut key = HKEY::default();
        let sub = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        if RegOpenKeyExW(HKEY_CURRENT_USER, sub, None, KEY_READ, &mut key).is_err() {
            return false;
        }
        let mut value: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let r = RegQueryValueExW(
            key,
            w!("AppsUseLightTheme"),
            None,
            None,
            Some(&mut value as *mut u32 as *mut u8),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        r.is_ok() && value == 0
    }
}

fn theme() -> Theme {
    if apps_use_dark_theme() {
        Theme {
            background: rgb(32, 32, 36),
            text: rgb(240, 240, 240),
            number: rgb(150, 150, 158),
            highlight: rgb(16, 138, 95),
            highlight_text: rgb(255, 255, 255),
            border: rgb(64, 64, 70),
        }
    } else {
        Theme {
            background: rgb(255, 255, 255),
            text: rgb(24, 24, 24),
            number: rgb(120, 120, 128),
            highlight: rgb(16, 138, 95),
            highlight_text: rgb(255, 255, 255),
            border: rgb(210, 210, 214),
        }
    }
}

/// What the window draws. Kept apart from the Direct2D objects so a change of content never has
/// to touch device resources.
#[derive(Default, Clone)]
pub struct Content {
    pub candidates: Vec<String>,
    pub cursor: usize,
    /// A next-word suggestion rather than a composition list. Labelled with the key that takes it,
    /// Tab, instead of a number: after a committed word the digit keys type Bengali digits, so a
    /// numbered entry would promise something pressing that number does not do.
    pub tab_hint: bool,
}

/// One candidate's measurements, in DIPs. The number and the word are measured separately because
/// they are drawn separately, in different colours; measuring the pair as one string gave a width
/// that did not match what was drawn, and the highlight pill missed its text.
struct CellMetrics {
    number_width: f32,
    word_width: f32,
    height: f32,
}

impl CellMetrics {
    fn width(&self) -> f32 {
        self.number_width + NUMBER_GAP + self.word_width
    }
}

/// Asked when the typist pauses: the fresh full ranking, or None if there is nothing to refine.
pub type Refiner = Box<dyn Fn() -> Option<Content>>;

struct Inner {
    hwnd: HWND,
    content: Content,
    d2d: Option<ID2D1Factory>,
    dwrite: Option<IDWriteFactory>,
    format: Option<IDWriteTextFormat>,
    target: Option<ID2D1HwndRenderTarget>,
    /// Config stamp the current text format was built from, so a font chosen in the Likhi window
    /// takes effect while typing rather than at the next sign-in.
    config_stamp: u128,
    last_checked: Option<std::time::Instant>,
    refiner: Option<Refiner>,
    /// Where the font settings come from. Injected: the text service and the engine find their
    /// configuration by different routes, and this window should not have to know which.
    fonts: FontSource,
}

pub struct CandidateWindow {
    inner: Box<RefCell<Inner>>,
}

impl CandidateWindow {
    /// `hinstance` must be the module this code is compiled into, not the host application's.
    /// A window class registered from a DLL has to name that DLL, or the class outlives the code
    /// its window procedure points into and the next window to use it jumps into freed memory.
    pub fn new(hinstance: HINSTANCE, fonts: FontSource) -> Option<Self> {
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            // CS_DROPSHADOW: the shadow Windows gives menus and tooltips, free, and the single
            // cheapest thing that makes a floating panel look like it belongs to the desktop.
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // Fails harmlessly with CLASS_ALREADY_EXISTS after the first window in a process.
        unsafe { RegisterClassExW(&class) };

        let inner = Box::new(RefCell::new(Inner {
            hwnd: HWND::default(),
            content: Content::default(),
            d2d: None,
            dwrite: None,
            format: None,
            target: None,
            config_stamp: 0,
            last_checked: None,
            refiner: None,
            fonts,
        }));
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("Likhi"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(hinstance),
                Some(&*inner as *const RefCell<Inner> as *const c_void),
            )
        }
        .ok()?;
        inner.borrow_mut().hwnd = hwnd;

        // Real rounded corners from the window manager on Windows 11. Before this the corners were
        // painted in the background colour inside a square window, which showed against anything
        // that was not that colour. Older Windows ignores the attribute and keeps the square.
        unsafe {
            let preference = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as *const c_void,
                std::mem::size_of_val(&preference) as u32,
            );
        }
        Some(CandidateWindow { inner })
    }

    /// Install the function the pause timer calls to fetch the full ranking.
    pub fn set_refiner(&self, refiner: Refiner) {
        self.inner.borrow_mut().refiner = Some(refiner);
    }

    /// Replace the list and highlight, re-measure, and show the window just below `anchor`
    /// (the composition's rectangle in screen coordinates). Arms the refine timer.
    pub fn show(&self, candidates: &[String], cursor: usize, anchor: &RECT) {
        self.show_with(candidates, cursor, anchor, false);
    }

    /// show, choosing whether entries are numbered or labelled Tab (a next-word suggestion).
    pub fn show_with(&self, candidates: &[String], cursor: usize, anchor: &RECT, tab_hint: bool) {
        if candidates.is_empty() {
            self.hide();
            return;
        }
        let hwnd = self.inner.borrow().hwnd;
        let size = {
            let mut inner = self.inner.borrow_mut();
            inner.content = Content {
                candidates: candidates.to_vec(),
                cursor,
                tab_hint,
            };
            inner.measure()
        };
        let Some((w, h)) = size else { return };
        let (x, y) = place(anchor, w, h);
        vlog!("candidate window shown at {x},{y} size {w}x{h} ({} items)", candidates.len());
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(Some(hwnd), None, false);
            SetTimer(Some(hwnd), REFINE_TIMER_ID, REFINE_AFTER_MS, None);
        }
    }

    pub fn hide(&self) {
        let hwnd = self.inner.borrow().hwnd;
        unsafe {
            let _ = KillTimer(Some(hwnd), REFINE_TIMER_ID);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        let hwnd = self.inner.borrow().hwnd;
        if !hwnd.is_invalid() {
            unsafe {
                let _ = KillTimer(Some(hwnd), REFINE_TIMER_ID);
                // Detach first so a late message cannot reach freed memory.
                set_user_data(hwnd, 0);
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

fn number_label(content: &Content, index: usize) -> String {
    if content.tab_hint {
        "Tab".to_string()
    } else {
        format!("{}", index + 1)
    }
}

// The window's user data holds a pointer, and Win32 spells that differently per architecture:
// SetWindowLongPtrW is the real 64-bit call, while on 32-bit it is an alias for SetWindowLongW,
// which takes an i32. These two wrappers keep the cast in one place instead of at every call.
#[cfg(target_pointer_width = "64")]
unsafe fn set_user_data(hwnd: HWND, value: isize) {
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, value);
}

#[cfg(target_pointer_width = "32")]
unsafe fn set_user_data(hwnd: HWND, value: isize) {
    SetWindowLongW(hwnd, GWLP_USERDATA, value as i32);
}

#[cfg(target_pointer_width = "64")]
unsafe fn get_user_data(hwnd: HWND) -> isize {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA)
}

#[cfg(target_pointer_width = "32")]
unsafe fn get_user_data(hwnd: HWND) -> isize {
    GetWindowLongW(hwnd, GWLP_USERDATA) as isize
}

/// Whether a font family is actually installed, asked of the system collection. `CreateTextFormat`
/// succeeds for a family that does not exist and substitutes at draw time, so this is the only way
/// to know which face will really be used.
fn family_available(dwrite: &IDWriteFactory, family: PCWSTR) -> bool {
    unsafe {
        let mut collection: Option<IDWriteFontCollection> = None;
        // No rescan: that walks the font directory, and this runs up to four times per format.
        // A font installed a moment ago is picked up the next time the format is rebuilt.
        if dwrite.GetSystemFontCollection(&mut collection, false).is_err() {
            return false;
        }
        let Some(collection) = collection else {
            return false;
        };
        let mut index = 0u32;
        let mut exists = BOOL(0);
        if collection.FindFamilyName(family, &mut index, &mut exists).is_err() {
            return false;
        }
        exists.as_bool()
    }
}

fn text_metrics(dwrite: &IDWriteFactory, format: &IDWriteTextFormat, text: &str) -> Option<DWRITE_TEXT_METRICS> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe { dwrite.CreateTextLayout(&wide, format, 4096.0, 1024.0) }.ok()?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics) }.ok()?;
    Some(metrics)
}

/// Where to put a window of `w` x `h` for text at `anchor`: just below it, on the monitor the text
/// is on, and above it when there is no room below.
///
/// The monitor is the anchor's, found from the anchor itself. An earlier version asked which
/// monitor the *window* was on, which for a window that had never been shown was whichever one
/// held (0,0) -- so on a two-monitor desk the list could open on the wrong screen.
fn place(anchor: &RECT, w: i32, h: i32) -> (i32, i32) {
    let below = anchor.bottom + OFFSET_Y;
    unsafe {
        let monitor = MonitorFromRect(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            let x = anchor.left.min(work.right - w).max(work.left);
            let y = if below + h > work.bottom {
                (anchor.top - OFFSET_Y - h).max(work.top)
            } else {
                below
            };
            return (x, y);
        }
    }
    (anchor.left, below)
}

impl Inner {
    fn scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }

    /// Drop the text format when the config has changed, so the next draw rebuilds it.
    /// Checked at most once a second: this runs per keystroke and a file stat is not free.
    fn watch_config(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_checked {
            if now.duration_since(last) < std::time::Duration::from_secs(1) {
                return;
            }
        }
        self.last_checked = Some(now);
        let (stamp, _) = (self.fonts)();
        if stamp != self.config_stamp {
            self.config_stamp = stamp;
            self.format = None;
        }
    }

    fn ensure_dwrite(&mut self) -> Option<()> {
        self.watch_config();
        if self.format.is_some() {
            return Some(());
        }
        let config: Fonts = (self.fonts)().1;
        unsafe {
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            // A font named in the config wins if it is installed; otherwise the built-in chain.
            let wanted = config.font_name.trim();
            let chosen: Vec<u16> = wanted.encode_utf16().chain(std::iter::once(0)).collect();
            let named = !wanted.is_empty() && family_available(&dwrite, PCWSTR(chosen.as_ptr()));
            let family = if named {
                PCWSTR(chosen.as_ptr())
            } else {
                FONT_CHAIN
                    .iter()
                    .copied()
                    .find(|f| family_available(&dwrite, *f))
                    .unwrap_or(FONT_CHAIN[FONT_CHAIN.len() - 1])
            };
            let size = if (8.0..=48.0).contains(&config.font_size) {
                config.font_size
            } else {
                FONT_DIP
            };
            log!("candidate window font: {} at {}px", family.display(), size);
            let format = dwrite
                .CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("bn-BD"),
                )
                .ok()?;
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            self.dwrite = Some(dwrite);
            self.format = Some(format);
        }
        Some(())
    }

    fn ensure_target(&mut self) -> Option<()> {
        if self.target.is_some() {
            return Some(());
        }
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U {
                    width: (rc.right - rc.left).max(1) as u32,
                    height: (rc.bottom - rc.top).max(1) as u32,
                },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let target = d2d.CreateHwndRenderTarget(&props, &hwnd_props).ok()?;
            self.d2d = Some(d2d);
            self.target = Some(target);
        }
        Some(())
    }

    /// Measurements for every candidate, or None when text cannot be measured at all.
    fn cells(&self) -> Option<Vec<CellMetrics>> {
        let dwrite = self.dwrite.as_ref()?;
        let format = self.format.as_ref()?;
        self.content
            .candidates
            .iter()
            .enumerate()
            .map(|(i, word)| {
                let n = text_metrics(dwrite, format, &number_label(&self.content, i))?;
                let w = text_metrics(dwrite, format, word)?;
                Some(CellMetrics {
                    number_width: n.width,
                    word_width: w.width,
                    height: n.height.max(w.height),
                })
            })
            .collect()
    }

    /// Window size in pixels for the current content.
    fn measure(&mut self) -> Option<(i32, i32)> {
        self.ensure_dwrite()?;
        let scale = self.scale();
        let cells = self.cells()?;
        let gaps = GAP * cells.len().saturating_sub(1) as f32;
        let width = PAD_X * 2.0 + cells.iter().map(CellMetrics::width).sum::<f32>() + gaps;
        let height = PAD_Y * 2.0 + cells.iter().map(|c| c.height).fold(0.0_f32, f32::max);
        Some(((width * scale).ceil() as i32, (height * scale).ceil() as i32))
    }

    /// The pause timer fired: fetch the full ranking and, if it differs, redraw in place.
    fn refine(&mut self) {
        let Some(refiner) = self.refiner.as_ref() else { return };
        let Some(content) = refiner() else { return };
        if content.candidates == self.content.candidates && content.cursor == self.content.cursor {
            return;
        }
        self.content = content;
        let Some((w, h)) = self.measure() else { return };
        unsafe {
            // Same top-left, new size: the text has not moved, only the list under it.
            let mut rc = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rc);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), rc.left, rc.top, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&mut self) {
        if self.ensure_dwrite().is_none() || self.ensure_target().is_none() {
            return;
        }
        let Some(cells) = self.cells() else { return };
        let target = self.target.clone().expect("target");
        let dwrite = self.dwrite.clone().expect("dwrite");
        let format = self.format.clone().expect("format");
        let theme = theme();
        let scale = self.scale();

        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let size = D2D_SIZE_U {
                width: (rc.right - rc.left).max(1) as u32,
                height: (rc.bottom - rc.top).max(1) as u32,
            };
            let _ = target.Resize(&size);
            // Draw in DIPs: tell the target the real DPI so Direct2D scales for us.
            target.SetDpi(96.0 * scale, 96.0 * scale);
            let width_dip = size.width as f32 / scale;
            let height_dip = size.height as f32 / scale;

            target.BeginDraw();
            target.Clear(Some(&theme.background as *const D2D1_COLOR_F));

            let brushes = [
                target.CreateSolidColorBrush(&theme.border, None),
                target.CreateSolidColorBrush(&theme.text, None),
                target.CreateSolidColorBrush(&theme.number, None),
                target.CreateSolidColorBrush(&theme.highlight, None),
                target.CreateSolidColorBrush(&theme.highlight_text, None),
            ];
            let [Ok(border), Ok(text), Ok(number), Ok(highlight), Ok(highlight_text)] = brushes else {
                let _ = target.EndDraw(None, None);
                return;
            };

            let outline = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: 0.5, top: 0.5, right: width_dip - 0.5, bottom: height_dip - 0.5 },
                radiusX: RADIUS,
                radiusY: RADIUS,
            };
            target.DrawRoundedRectangle(&outline, &border, 1.0, None);

            let mut x = PAD_X;
            for (i, (word, cell)) in self.content.candidates.iter().zip(&cells).enumerate() {
                let selected = i == self.content.cursor;
                if selected {
                    let pill = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: x - PILL_X,
                            top: PAD_Y - PILL_Y,
                            right: x + cell.width() + PILL_X,
                            bottom: height_dip - PAD_Y + PILL_Y,
                        },
                        radiusX: RADIUS - 2.0,
                        radiusY: RADIUS - 2.0,
                    };
                    target.FillRoundedRectangle(&pill, &highlight);
                }
                let number_brush = if selected { &highlight_text } else { &number };
                let word_brush = if selected { &highlight_text } else { &text };
                draw_text(&target, &dwrite, &format, &number_label(&self.content, i), x, height_dip, number_brush);
                draw_text(&target, &dwrite, &format, word, x + cell.number_width + NUMBER_GAP, height_dip, word_brush);
                x += cell.width() + GAP;
            }

            if target.EndDraw(None, None).is_err() {
                // Device lost: drop the target, it is rebuilt on the next paint.
                self.target = None;
            }
        }
    }
}

/// One run of text at `x`, vertically centred by the format's paragraph alignment.
unsafe fn draw_text(
    target: &ID2D1HwndRenderTarget,
    dwrite: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &str,
    x: f32,
    height_dip: f32,
    brush: &ID2D1SolidColorBrush,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    if let Ok(layout) = dwrite.CreateTextLayout(&wide, format, 4096.0, height_dip) {
        target.DrawTextLayout(Vector2 { X: x, Y: 0.0 }, &layout, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let inner = || {
        let ptr = get_user_data(hwnd) as *const RefCell<Inner>;
        if ptr.is_null() { None } else { Some(&*ptr) }
    };
    match msg {
        WM_NCCREATE => {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            set_user_data(hwnd, create.lpCreateParams as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            if let Some(cell) = inner() {
                if let Ok(mut inner) = cell.try_borrow_mut() {
                    inner.paint();
                }
            }
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == REFINE_TIMER_ID => {
            let _ = KillTimer(Some(hwnd), REFINE_TIMER_ID);
            if let Some(cell) = inner() {
                if let Ok(mut inner) = cell.try_borrow_mut() {
                    inner.refine();
                }
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Never take focus from the application, and never let a click reach it through us.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETTINGCHANGE => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
