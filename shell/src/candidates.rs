//! The candidate window: our own, drawn with Direct2D and DirectWrite.
//!
//! This is the reason the shell exists. PIME's window paints from `GetSysColor` and exposes a font
//! name, a size and a per-row count; nothing else. This one follows the Windows app theme, has real
//! padding and rounded corners, and shapes Bengali correctly because DirectWrite does -- the vowel
//! sign in "লি" lands before the consonant without anyone having to think about it.
//!
//! One window per text service instance, created lazily on the application's UI thread (which is
//! where TSF calls us), never activated, always on top, hidden when nothing is being composed.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
// Direct2D points are plain vectors from a sibling crate; windows 0.62 has no D2D_POINT_2F alias.
use windows_numerics::Vector2;

use crate::log;

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
const RADIUS: f32 = 6.0;
/// Space between the composition's bottom edge and the window.
const OFFSET_Y: i32 = 4;

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
struct Content {
    candidates: Vec<String>,
    cursor: usize,
}

struct Inner {
    hwnd: HWND,
    content: Content,
    d2d: Option<ID2D1Factory>,
    dwrite: Option<IDWriteFactory>,
    format: Option<IDWriteTextFormat>,
    target: Option<ID2D1HwndRenderTarget>,
}

pub struct CandidateWindow {
    inner: Box<RefCell<Inner>>,
}

impl CandidateWindow {
    pub fn new() -> Option<Self> {
        let hinstance = unsafe { GetModuleHandleW(None) }.ok()?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
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
                Some(hinstance.into()),
                Some(&*inner as *const RefCell<Inner> as *const c_void),
            )
        }
        .ok()?;
        inner.borrow_mut().hwnd = hwnd;
        Some(CandidateWindow { inner })
    }

    /// Replace the list and highlight, re-measure, and show the window just below `anchor`
    /// (the composition's rectangle in screen coordinates).
    pub fn show(&self, candidates: &[String], cursor: usize, anchor: &RECT) {
        let hwnd = self.inner.borrow().hwnd;
        if candidates.is_empty() {
            self.hide();
            return;
        }
        {
            let mut inner = self.inner.borrow_mut();
            inner.content = Content {
                candidates: candidates.to_vec(),
                cursor,
            };
        }
        let (w, h) = match self.measure() {
            Some(size) => size,
            None => return,
        };
        let (x, y) = clamp_to_monitor(hwnd, anchor.left, anchor.bottom + OFFSET_Y, w, h);
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }

    pub fn hide(&self) {
        let hwnd = self.inner.borrow().hwnd;
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    fn measure(&self) -> Option<(i32, i32)> {
        let mut inner = self.inner.borrow_mut();
        inner.ensure_dwrite()?;
        let scale = inner.scale();
        let format = inner.format.clone()?;
        let dwrite = inner.dwrite.clone()?;
        let mut width = PAD_X * 2.0;
        let mut height: f32 = 0.0;
        for (i, c) in inner.content.candidates.iter().enumerate() {
            let m = text_metrics(&dwrite, &format, &label(i, c))?;
            width += m.width + if i + 1 < inner.content.candidates.len() { GAP } else { 0.0 };
            height = height.max(m.height);
        }
        height += PAD_Y * 2.0;
        Some(((width * scale).ceil() as i32, (height * scale).ceil() as i32))
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        let hwnd = self.inner.borrow().hwnd;
        if !hwnd.is_invalid() {
            unsafe {
                // Detach first so a late message cannot reach freed memory.
                set_user_data(hwnd, 0);
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

/// Measured as one string so the number and the word are spaced by the same metrics that draw them.
fn label(index: usize, candidate: &str) -> String {
    format!("{}  {}", index + 1, candidate)
}

fn number_label(index: usize) -> String {
    format!("{}", index + 1)
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
        if dwrite
            .GetSystemFontCollection(&mut collection, true)
            .is_err()
        {
            return false;
        }
        let Some(collection) = collection else {
            return false;
        };
        let mut index = 0u32;
        let mut exists = BOOL(0);
        if collection
            .FindFamilyName(family, &mut index, &mut exists)
            .is_err()
        {
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

/// Keep the window on the monitor the anchor is on: below the text if it fits, else above.
fn clamp_to_monitor(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            let mut nx = x.min(work.right - w).max(work.left);
            let mut ny = y;
            if ny + h > work.bottom {
                // Above the line instead; the anchor's bottom minus roughly one line height.
                ny = (y - OFFSET_Y - h - 22).max(work.top);
            }
            if nx < work.left {
                nx = work.left;
            }
            return (nx, ny);
        }
    }
    (x, y)
}

impl Inner {
    fn scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }

    fn ensure_dwrite(&mut self) -> Option<()> {
        if self.format.is_some() {
            return Some(());
        }
        unsafe {
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let family = FONT_CHAIN
                .iter()
                .copied()
                .find(|f| family_available(&dwrite, *f))
                .unwrap_or(FONT_CHAIN[FONT_CHAIN.len() - 1]);
            log!("candidate window font: {}", family.display());
            let format = dwrite
                .CreateTextFormat(
                    family,
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    FONT_DIP,
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

    fn paint(&mut self) {
        if self.ensure_dwrite().is_none() || self.ensure_target().is_none() {
            return;
        }
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

            let Ok(border) = target.CreateSolidColorBrush(&theme.border, None) else { let _ = target.EndDraw(None, None); return; };
            let Ok(text) = target.CreateSolidColorBrush(&theme.text, None) else { let _ = target.EndDraw(None, None); return; };
            let Ok(number) = target.CreateSolidColorBrush(&theme.number, None) else { let _ = target.EndDraw(None, None); return; };
            let Ok(highlight) = target.CreateSolidColorBrush(&theme.highlight, None) else { let _ = target.EndDraw(None, None); return; };
            let Ok(highlight_text) = target.CreateSolidColorBrush(&theme.highlight_text, None) else { let _ = target.EndDraw(None, None); return; };

            let outline = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: 0.5, top: 0.5, right: width_dip - 0.5, bottom: height_dip - 0.5 },
                radiusX: RADIUS,
                radiusY: RADIUS,
            };
            target.DrawRoundedRectangle(&outline, &border, 1.0, None);

            let mut x = PAD_X;
            let n = self.content.candidates.len();
            for (i, c) in self.content.candidates.iter().enumerate() {
                let full = label(i, c);
                let Some(m) = text_metrics(&dwrite, &format, &full) else { continue };
                let selected = i == self.content.cursor;
                if selected {
                    let pill = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: x - 6.0,
                            top: PAD_Y - 3.0,
                            right: x + m.width + 6.0,
                            bottom: height_dip - PAD_Y + 3.0,
                        },
                        radiusX: RADIUS - 2.0,
                        radiusY: RADIUS - 2.0,
                    };
                    target.FillRoundedRectangle(&pill, &highlight);
                }
                // Number in a quieter colour, then the word: two layouts so they can differ.
                let num = number_label(i);
                if let Some(nm) = text_metrics(&dwrite, &format, &num) {
                    let wide: Vec<u16> = num.encode_utf16().collect();
                    if let Ok(layout) = dwrite.CreateTextLayout(&wide, &format, 4096.0, height_dip) {
                        target.DrawTextLayout(
                            Vector2 { X: x, Y: 0.0 },
                            &layout,
                            if selected { &highlight_text } else { &number },
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                    let wide: Vec<u16> = c.encode_utf16().collect();
                    if let Ok(layout) = dwrite.CreateTextLayout(&wide, &format, 4096.0, height_dip) {
                        target.DrawTextLayout(
                            Vector2 { X: x + nm.width + NUMBER_GAP, Y: 0.0 },
                            &layout,
                            if selected { &highlight_text } else { &text },
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                x += m.width + if i + 1 < n { GAP } else { 0.0 };
            }

            if target.EndDraw(None, None).is_err() {
                // Device lost: drop the target, it is rebuilt on the next paint.
                self.target = None;
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            set_user_data(hwnd, create.lpCreateParams as isize);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            let ptr = get_user_data(hwnd) as *const RefCell<Inner>;
            if !ptr.is_null() {
                if let Ok(mut inner) = (*ptr).try_borrow_mut() {
                    inner.paint();
                }
            }
            let _ = ValidateRect(Some(hwnd), None);
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

#[allow(dead_code)]
pub fn describe() -> &'static str {
    // Used by the log at first paint, to make theme problems visible in shell.log.
    if apps_use_dark_theme() { "dark" } else { "light" }
}

#[allow(dead_code)]
fn _log_theme() {
    log!("candidate window theme: {}", describe());
}
