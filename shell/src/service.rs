//! The text service: what TSF instantiates inside each application that switches to Likhi.
//!
//! Milestone 2a. The typed Latin is shown as a composition; every keystroke asks the engine for
//! candidates within the typing deadline; Space or Enter asks once more with the commit deadline
//! and commits the highlighted candidate, digits 1-5 pick one directly, arrows move the highlight,
//! Escape cancels. What the engine was told is reported back with `learn` so it improves.
//!
//! Not yet: the candidate window (2b) and the underline on the composition (2c). Until 2b the
//! highlight is invisible but the keys already behave as they will.

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::TextServices::*;

use crate::candidates::CandidateWindow;
use crate::edit::EditSession;
use crate::engine::{Engine, COMMIT_DEADLINE_MS, DEFAULT_PORT, TYPE_DEADLINE_MS};
use crate::log;

const CANDIDATES: usize = 5;
/// How many committed words are sent as context. The engine uses the last one; sending two costs
/// nothing and leaves room for a bigram-of-bigrams later without a protocol change.
const CONTEXT_WORDS: usize = 2;

/// Everything that changes while typing, behind one `Rc` so edit-session closures can share it.
struct State {
    composition: Option<ITfComposition>,
    buffer: String,
    candidates: Vec<String>,
    cursor: usize,
    /// True once the person has moved the highlight themselves. An explicit choice is final: it is
    /// never overwritten by a later, better-informed ranking, and never re-derived at commit.
    chosen: bool,
    /// Recently committed words, oldest first.
    context: Vec<String>,
    engine: Engine,
}

impl State {
    fn new() -> Self {
        State {
            composition: None,
            buffer: String::new(),
            candidates: Vec::new(),
            cursor: 0,
            chosen: false,
            context: Vec::new(),
            engine: Engine::new(DEFAULT_PORT),
        }
    }

    fn clear_word(&mut self) {
        self.composition = None;
        self.buffer.clear();
        self.candidates.clear();
        self.cursor = 0;
        self.chosen = false;
    }

    fn remember(&mut self, word: &str) {
        if word.is_empty() {
            return;
        }
        self.context.push(word.to_string());
        if self.context.len() > CONTEXT_WORDS {
            self.context.remove(0);
        }
    }
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
    /// Created on first use, on the application's UI thread, and kept for the life of the service.
    window: RefCell<Option<CandidateWindow>>,
}

impl TextService {
    pub fn new() -> Self {
        TextService {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            thread_mgr_cookie: Cell::new(TF_INVALID_COOKIE),
            state: Rc::new(RefCell::new(State::new())),
            window: RefCell::new(None),
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

/// 1..5 on the main row picks a candidate while composing. Shift+digit is punctuation, not a pick.
fn digit_pick(vk: u32) -> Option<usize> {
    if (0x31..=0x35).contains(&vk) && !key_down(VK_SHIFT) {
        Some((vk - 0x31) as usize)
    } else {
        None
    }
}

/// Punctuation typed while composing: it ends the word, so the highlighted candidate is committed
/// and the mark follows it. Resolved from the virtual key rather than the layout, which is safe
/// because the profile declares a US substitute layout.
///
/// A full stop becomes the Bengali daṛi. Bangla ends a sentence with a vertical stroke, and a
/// keyboard that makes people reach for a character map to finish a sentence is not a Bangla
/// keyboard. Every other mark is shared with Latin and passes through unchanged.
fn punctuation(vk: u32) -> Option<&'static str> {
    let shift = key_down(VK_SHIFT);
    Some(match (vk, shift) {
        (0xBC, false) => ",",
        (0xBC, true) => "<",
        (0xBE, false) => "\u{0964}", // ।
        (0xBE, true) => ">",
        (0xBA, false) => ";",
        (0xBA, true) => ":",
        (0xBF, false) => "/",
        (0xBF, true) => "?",
        (0xDE, false) => "'",
        (0xDE, true) => "\"",
        (0xBD, false) => "-",
        (0xBD, true) => "_",
        (0x31, true) => "!",
        (0x38, true) => "*",
        (0x39, true) => "(",
        (0x30, true) => ")",
        _ => return None,
    })
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
        if !self.composing() {
            return false;
        }
        digit_pick(vk).is_some()
            || punctuation(vk).is_some()
            || matches!(
                VIRTUAL_KEY(vk as u16),
                VK_SPACE | VK_RETURN | VK_ESCAPE | VK_BACK | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN
            )
    }

    fn handle(&self, ctx: &ITfContext, vk: u32) -> Result<()> {
        if let Some(ch) = letter(vk) {
            self.state.borrow_mut().buffer.push(ch);
            self.ask(TYPE_DEADLINE_MS);
            self.show(ctx)?;
            self.update_window(ctx);
            return Ok(());
        }
        if let Some(index) = digit_pick(vk) {
            let picked = self.state.borrow().candidates.get(index).cloned();
            return match picked {
                Some(word) => self.commit(ctx, index, word, ""),
                None => Ok(()), // no such candidate: swallow the key rather than type a digit
            };
        }
        if let Some(mark) = punctuation(vk) {
            return self.commit_current(ctx, mark);
        }
        match VIRTUAL_KEY(vk as u16) {
            VK_BACK => {
                let empty = {
                    let mut s = self.state.borrow_mut();
                    s.buffer.pop();
                    s.buffer.is_empty()
                };
                if empty {
                    self.cancel(ctx)
                } else {
                    self.ask(TYPE_DEADLINE_MS);
                    self.show(ctx)?;
                    self.update_window(ctx);
                    Ok(())
                }
            }
            VK_ESCAPE => self.cancel(ctx),
            VK_SPACE | VK_RETURN => {
                let trailing = if VIRTUAL_KEY(vk as u16) == VK_SPACE { " " } else { "" };
                self.commit_current(ctx, trailing)
            }
            VK_RIGHT | VK_DOWN => {
                {
                    let mut s = self.state.borrow_mut();
                    if !s.candidates.is_empty() {
                        s.cursor = (s.cursor + 1) % s.candidates.len();
                        s.chosen = true;
                    }
                }
                self.update_window(ctx);
                Ok(())
            }
            VK_LEFT | VK_UP => {
                {
                    let mut s = self.state.borrow_mut();
                    if !s.candidates.is_empty() {
                        s.cursor = (s.cursor + s.candidates.len() - 1) % s.candidates.len();
                        s.chosen = true;
                    }
                }
                self.update_window(ctx);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Commit whatever is highlighted, followed by `trailing`.
    ///
    /// Re-asks with the commit deadline only when the person has not chosen for themselves. That
    /// second question exists so the committed word comes from the full ranking rather than the
    /// fast path shown while typing -- but it returns a different list, and running it after an
    /// explicit arrow selection threw that selection away and committed something else.
    fn commit_current(&self, ctx: &ITfContext, trailing: &str) -> Result<()> {
        if !self.state.borrow().chosen {
            self.ask(COMMIT_DEADLINE_MS);
        }
        let (index, word) = {
            let s = self.state.borrow();
            let index = s.cursor.min(s.candidates.len().saturating_sub(1));
            let word = s
                .candidates
                .get(index)
                .cloned()
                .unwrap_or_else(|| s.buffer.clone());
            (index, word)
        };
        self.commit(ctx, index, word, trailing)
    }

    /// Ask the engine for candidates for the current buffer. A dead engine leaves the list empty,
    /// and commit then falls back to the Latin as typed -- degraded, never broken.
    ///
    /// The highlight always returns to the first candidate. An earlier version tried to keep it on
    /// whichever word it was on, which meant that after typing "ama" and then "r" the highlight
    /// followed the old word to wherever it had fallen in the new ranking -- so the picker opened
    /// pointing at the third entry for no reason the typist could see. Each new letter is a new
    /// ranking, and the top of a new ranking is the answer.
    fn ask(&self, deadline_ms: u32) {
        let mut s = self.state.borrow_mut();
        let buffer = s.buffer.clone();
        let context = s.context.clone();
        match s.engine.suggest(&buffer, &context, CANDIDATES, deadline_ms) {
            Some(reply) => s.candidates = reply.candidates,
            None => s.candidates.clear(),
        }
        s.cursor = 0;
        s.chosen = false;
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

    /// Commit `word` (plus `trailing`) in place of the composition, report the choice, remember it.
    fn commit(&self, ctx: &ITfContext, index: usize, word: String, trailing: &str) -> Result<()> {
        {
            let mut s = self.state.borrow_mut();
            let buffer = s.buffer.clone();
            let context = s.context.clone();
            let top1 = s.candidates.first().cloned().unwrap_or_default();
            if !buffer.is_empty() && word != buffer {
                s.engine.learn(&buffer, &word, &context, &top1);
            }
            let _ = index;
            s.remember(&word);
        }
        self.finish(ctx, format!("{word}{trailing}"))
    }

    fn cancel(&self, ctx: &ITfContext) -> Result<()> {
        self.finish(ctx, String::new())
    }

    /// Replace the composition with `text` (empty cancels) and end it.
    fn finish(&self, ctx: &ITfContext, text: String) -> Result<()> {
        self.hide_window();
        let state = self.state.clone();
        let ctx2 = ctx.clone();
        EditSession::run(ctx, self.client_id.get(), move |ec| {
            let composition = state.borrow_mut().composition.take();
            state.borrow_mut().clear_word();
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
        self.state.borrow_mut().clear_word();
        self.hide_window();
    }

    fn hide_window(&self) {
        if let Some(w) = self.window.borrow().as_ref() {
            w.hide();
        }
    }

    /// Put the candidate list under the text being composed.
    ///
    /// The anchor comes from the context view, which is the only thing that knows where the
    /// composition ended up on screen -- an application may have scrolled, wrapped or transformed
    /// it. When it cannot say (`GetTextExt` reports the text is clipped, or there is no view), the
    /// window is hidden rather than drawn somewhere wrong.
    fn update_window(&self, ctx: &ITfContext) {
        let (candidates, cursor) = {
            let s = self.state.borrow();
            (s.candidates.clone(), s.cursor)
        };
        if candidates.is_empty() {
            self.hide_window();
            return;
        }
        if self.window.borrow().is_none() {
            *self.window.borrow_mut() = CandidateWindow::new();
            if self.window.borrow().is_none() {
                log!("candidate window could not be created");
                return;
            }
        }
        let anchor = match self.composition_rect(ctx) {
            Some(r) => r,
            None => {
                self.hide_window();
                return;
            }
        };
        if let Some(w) = self.window.borrow().as_ref() {
            w.show(&candidates, cursor, &anchor);
        }
    }

    /// Screen rectangle of the composition, via a read-only edit session.
    fn composition_rect(&self, ctx: &ITfContext) -> Option<RECT> {
        let composition = self.state.borrow().composition.clone()?;
        let result = Rc::new(Cell::new(None::<RECT>));
        let out = result.clone();
        let ctx2 = ctx.clone();
        let ok = EditSession::run_read(ctx, self.client_id.get(), move |ec| {
            let range = unsafe { composition.GetRange() }?;
            let view = unsafe { ctx2.GetActiveView() }?;
            let mut rect = RECT::default();
            let mut clipped = BOOL(0);
            unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) }?;
            if !clipped.as_bool() {
                out.set(Some(rect));
            }
            Ok(())
        });
        if ok.is_err() {
            return None;
        }
        result.get()
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
