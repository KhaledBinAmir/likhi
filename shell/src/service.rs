//! The text service: what TSF instantiates inside each application that switches to Likhi.
//!
//! Milestone 1. This proves the whole chain -- registration, activation, key interception, a live
//! composition that the application draws, commit and cancel -- with a placeholder conversion:
//! the typed Latin is committed in upper case, which is visibly ours and visibly not the layout.
//! Milestone 2 replaces that with the engine over the existing socket protocol and adds the
//! candidate window. Nothing about TSF changes between the two.

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::TextServices::*;

use crate::edit::EditSession;
use crate::log;

/// Everything that changes while typing, behind one `Rc` so edit-session closures can share it.
#[derive(Default)]
struct State {
    composition: Option<ITfComposition>,
    buffer: String,
}

#[implement(
    ITfTextInputProcessorEx,
    ITfThreadMgrEventSink,
    ITfKeyEventSink,
    ITfCompositionSink
)]
pub struct TextService {
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    client_id: Cell<u32>,
    thread_mgr_cookie: Cell<u32>,
    state: Rc<RefCell<State>>,
}

impl TextService {
    pub fn new() -> Self {
        TextService {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            thread_mgr_cookie: Cell::new(TF_INVALID_COOKIE),
            state: Rc::new(RefCell::new(State::default())),
        }
    }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    // High bit set: the key is currently held.
    let state = unsafe { GetKeyState(vk.0 as i32) };
    state < 0
}

fn key_toggled(vk: VIRTUAL_KEY) -> bool {
    let state = unsafe { GetKeyState(vk.0 as i32) };
    (state & 1) == 1
}

/// The Latin letter for a virtual key, or None. Virtual key codes for the letter keys are
/// 0x41..0x5A, the same values as ASCII 'A'..'Z', and follow the physical key rather than the
/// layout -- and this service declares a US substitute layout anyway (guids.rs).
fn letter(vk: u32) -> Option<char> {
    if !(0x41..=0x5A).contains(&vk) {
        return None;
    }
    let upper = key_down(VK_SHIFT) != key_toggled(VK_CAPITAL);
    let c = vk as u8 as char;
    Some(if upper { c } else { c.to_ascii_lowercase() })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl TextService_Impl {
    fn composing(&self) -> bool {
        self.state.borrow().composition.is_some()
    }

    /// Whether this key is ours. Asked twice per key (OnTestKeyDown, then OnKeyDown) and the answer
    /// must be the same both times, so it depends on nothing that the first call changes.
    fn wants(&self, vk: u32) -> bool {
        if key_down(VK_CONTROL) || key_down(VK_MENU) {
            return false;
        }
        if letter(vk).is_some() {
            return true;
        }
        self.composing()
            && matches!(
                VIRTUAL_KEY(vk as u16),
                VK_SPACE | VK_RETURN | VK_ESCAPE | VK_BACK
            )
    }

    fn handle(&self, ctx: &ITfContext, vk: u32) -> Result<()> {
        if let Some(ch) = letter(vk) {
            self.state.borrow_mut().buffer.push(ch);
            return self.show(ctx);
        }
        match VIRTUAL_KEY(vk as u16) {
            VK_BACK => {
                let empty = {
                    let mut s = self.state.borrow_mut();
                    s.buffer.pop();
                    s.buffer.is_empty()
                };
                if empty {
                    self.finish(ctx, String::new())
                } else {
                    self.show(ctx)
                }
            }
            VK_ESCAPE => self.finish(ctx, String::new()),
            VK_SPACE => {
                let text = self.convert() + " ";
                self.finish(ctx, text)
            }
            VK_RETURN => {
                let text = self.convert();
                self.finish(ctx, text)
            }
            _ => Ok(()),
        }
    }

    /// Milestone 1 placeholder for the engine.
    fn convert(&self) -> String {
        self.state.borrow().buffer.to_uppercase()
    }

    /// Start the composition if there is none, then set its text to the buffer.
    fn show(&self, ctx: &ITfContext) -> Result<()> {
        let state = self.state.clone();
        let sink: ITfCompositionSink = self.to_interface();
        let ctx2 = ctx.clone();
        EditSession::run(ctx, self.client_id.get(), move |ec| {
            let text = wide(&state.borrow().buffer);
            let existing = state.borrow().composition.clone();
            let composition = match existing {
                Some(c) => c,
                None => {
                    let insert: ITfInsertAtSelection = ctx2.cast()?;
                    let range = unsafe { insert.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[]) }?;
                    let owner: ITfContextComposition = ctx2.cast()?;
                    let c = unsafe { owner.StartComposition(ec, &range, &sink) }?;
                    state.borrow_mut().composition = Some(c.clone());
                    c
                }
            };
            let range = unsafe { composition.GetRange() }?;
            unsafe { range.SetText(ec, 0, &text) }?;
            place_caret_after(&ctx2, ec, &range)
        })
    }

    /// Replace the composition with `text` (empty cancels) and end it.
    fn finish(&self, ctx: &ITfContext, text: String) -> Result<()> {
        let state = self.state.clone();
        let ctx2 = ctx.clone();
        EditSession::run(ctx, self.client_id.get(), move |ec| {
            let composition = state.borrow_mut().composition.take();
            state.borrow_mut().buffer.clear();
            if let Some(composition) = composition {
                let range = unsafe { composition.GetRange() }?;
                unsafe { range.SetText(ec, 0, &wide(&text)) }?;
                place_caret_after(&ctx2, ec, &range)?;
                unsafe { composition.EndComposition(ec) }?;
            }
            Ok(())
        })
    }

    fn reset(&self) {
        let mut s = self.state.borrow_mut();
        s.composition = None;
        s.buffer.clear();
    }
}

/// Collapse `range` to its end and make that the selection, so typing continues after the text.
fn place_caret_after(ctx: &ITfContext, ec: u32, range: &ITfRange) -> Result<()> {
    unsafe {
        range.Collapse(ec, TF_ANCHOR_END)?;
        let mut selection = TF_SELECTION {
            range: ManuallyDrop::new(Some(range.clone())),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_NONE,
                fInterimChar: BOOL(0),
            },
        };
        let result = ctx.SetSelection(ec, &[selection.clone()]);
        // The clone inside ManuallyDrop holds a reference COM will never release for us.
        ManuallyDrop::drop(&mut selection.range);
        result
    }
}

// ------------------------------------------------------------------ activation

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        self.ActivateEx(ptim, tid, 0)
    }

    fn Deactivate(&self) -> Result<()> {
        log!("deactivate");
        let tim = self.thread_mgr.borrow_mut().take();
        if let Some(tim) = tim {
            unsafe {
                if let Ok(keys) = tim.cast::<ITfKeystrokeMgr>() {
                    let _ = keys.UnadviseKeyEventSink(self.client_id.get());
                }
                if let Ok(source) = tim.cast::<ITfSource>() {
                    let cookie = self.thread_mgr_cookie.replace(TF_INVALID_COOKIE);
                    if cookie != TF_INVALID_COOKIE {
                        let _ = source.UnadviseSink(cookie);
                    }
                }
            }
        }
        self.reset();
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, flags: u32) -> Result<()> {
        let tim = ptim.ok()?.clone();
        self.client_id.set(tid);
        log!("activate client={tid} flags={flags:#x}");
        unsafe {
            let source: ITfSource = tim.cast()?;
            let sink: ITfThreadMgrEventSink = self.to_interface();
            let cookie = source.AdviseSink(&ITfThreadMgrEventSink::IID, &sink)?;
            self.thread_mgr_cookie.set(cookie);

            let keys: ITfKeystrokeMgr = tim.cast()?;
            let key_sink: ITfKeyEventSink = self.to_interface();
            keys.AdviseKeyEventSink(tid, &key_sink, true)?;
        }
        *self.thread_mgr.borrow_mut() = Some(tim);
        Ok(())
    }
}

// ------------------------------------------------------------------ thread manager events

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }
    fn OnUninitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }
    fn OnSetFocus(&self, _focus: Ref<ITfDocumentMgr>, _prev: Ref<ITfDocumentMgr>) -> Result<()> {
        // Focus moved to another document: whatever was being composed there is the application's
        // now. Forget our half of it rather than write into the wrong window later.
        self.reset();
        Ok(())
    }
    fn OnPushContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
    fn OnPopContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
}

// ------------------------------------------------------------------ keys

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, _foreground: BOOL) -> Result<()> {
        Ok(())
    }

    fn OnTestKeyDown(&self, _pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL::from(self.wants(wparam.0 as u32)))
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = wparam.0 as u32;
        if !self.wants(vk) {
            return Ok(BOOL(0));
        }
        let ctx = pic.ok()?.clone();
        if let Err(e) = self.handle(&ctx, vk) {
            // Never let a failure escape into the host: log it, drop our state, and let the key go.
            log!("key {vk:#x} failed: {e}");
            self.reset();
            return Ok(BOOL(0));
        }
        Ok(BOOL(1))
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(BOOL(0))
    }
}

// ------------------------------------------------------------------ composition

impl ITfCompositionSink_Impl for TextService_Impl {
    fn OnCompositionTerminated(&self, _ecwrite: u32, _composition: Ref<ITfComposition>) -> Result<()> {
        // The application ended it (focus loss, a click elsewhere). Its text stays as it is.
        log!("composition terminated by the application");
        self.reset();
        Ok(())
    }
}
