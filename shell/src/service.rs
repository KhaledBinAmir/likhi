//! The text service: what TSF instantiates inside each application that switches to Likhi.
//!
//! Typed Latin is shown as a composition, underlined; every keystroke asks the engine for
//! candidates within the typing deadline and shows them in our own window; Space or Enter commits
//! the highlighted candidate, digits pick one, arrows move the highlight, punctuation ends the word
//! and follows it, Escape cancels. The toggle key switches to plain English and back. Every commit
//! is reported to the engine so it learns and the pilot can count.

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::TextServices::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetSystemMetrics, GetWindowRect, GUITHREADINFO,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::Win32::Graphics::Gdi::ClientToScreen;

use likhi_ui::CandidateWindow;
use crate::config::{Config, BANGLA_DIGITS};
use crate::display::{DisplayAttributeInfo, EnumDisplayAttributeInfo};
use crate::edit::EditSession;
use crate::engine::{Commit, Engine, COMMIT_DEADLINE_MS, TYPE_DEADLINE_MS};
use crate::guids::GUID_DISPLAY_ATTRIBUTE;
use crate::log;
use crate::vlog;
use crate::uielement::{CandidateUi, UiState};

/// How many committed words are sent as context. The engine uses the last one; sending two costs
/// nothing and leaves room for a bigram-of-bigrams later without a protocol change.
const CONTEXT_WORDS: usize = 2;

const VK_OEM_PERIOD_CODE: u32 = 0xBE;
const DARI: &str = "\u{0964}";

/// Everything that changes while typing, behind one `Rc` so edit-session closures can share it.
struct State {
    composition: Option<ITfComposition>,
    buffer: String,
    candidates: Vec<String>,
    cursor: usize,
    /// True once the person has moved the highlight themselves. An explicit choice is final: it is
    /// never overwritten by a later, better-informed ranking, and never re-derived at commit.
    chosen: bool,
    /// The engine answered from its fast path because the typing deadline arrived first, so this
    /// list may not be the one the full ranking would give.
    partial: bool,
    /// That fast answer is an attested spelling of exactly what was typed, seen more than once.
    /// Asking again would be allowed to overrule it, and measurement says that is a mistake.
    strong: bool,
    /// Backspace was used inside this word. Reported with the commit: a word someone had to correct
    /// mid-way is a different signal from one typed straight through.
    retyped: bool,
    /// Recently committed words, oldest first.
    context: Vec<String>,
    engine: Engine,
}

impl State {
    fn new(port: u16) -> Self {
        State {
            composition: None,
            buffer: String::new(),
            candidates: Vec::new(),
            cursor: 0,
            chosen: false,
            partial: false,
            strong: false,
            retyped: false,
            context: Vec::new(),
            engine: Engine::new(port),
        }
    }

    fn clear_word(&mut self) {
        self.composition = None;
        self.buffer.clear();
        self.candidates.clear();
        self.cursor = 0;
        self.chosen = false;
        self.partial = false;
        self.strong = false;
        self.retyped = false;
    }

    /// Whether asking the engine again could improve this answer.
    ///
    /// Only a fast-path answer can be improved at all, and only one the fast path is not confident
    /// about. Measured over Dakshina, the chat set and the feedback words: refining everything
    /// scores 73.9 top-1, refining nothing 58.4, refining only the unconfident 75.7 -- and on chat
    /// words alone, refining everything is a regression, 79.5 against 81.7.
    fn worth_refining(&self) -> bool {
        self.partial && !self.strong
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
    ITfCompositionSink,
    ITfDisplayAttributeProvider
)]
pub struct TextService {
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    client_id: Cell<u32>,
    thread_mgr_cookie: Cell<u32>,
    state: Rc<RefCell<State>>,
    /// Created on first use, on the application's UI thread, and kept for the life of the service.
    window: RefCell<Option<CandidateWindow>>,
    config: Config,
    /// The toggle key's virtual key code, resolved once. It is consulted on every keystroke, and
    /// parsing "F12" out of a string each time is work the typing path does not need.
    toggle_vk: Option<u32>,
    /// False after the toggle key: letters go through as plain English without leaving the keyboard.
    bangla: Cell<bool>,
    /// The atom TSF uses to name our display attribute on a range. Resolved once at activation.
    attribute_atom: Cell<u32>,
    /// The executable this instance lives in, for per-application counters. We *are* inside the
    /// application, so this is exact -- no foreground-window guessing.
    app_name: String,
    /// The candidate list as TSF sees it, shared with the published UI element.
    ui: Rc<RefCell<UiState>>,
    /// The element we published, and the id TSF gave it, while a list is on screen.
    ui_element: RefCell<Option<ITfUIElement>>,
    ui_element_id: Cell<u32>,
    /// Whether the host asked us to draw the list ourselves. False in an immersive application,
    /// which draws it from the published element instead.
    draw_ourselves: Cell<bool>,
}

impl TextService {
    pub fn new() -> Self {
        let config = Config::load();
        let state = Rc::new(RefCell::new(State::new(config.server_port)));
        let app_name = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .unwrap_or_default();
        TextService {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            thread_mgr_cookie: Cell::new(TF_INVALID_COOKIE),
            state,
            window: RefCell::new(None),
            toggle_vk: config.toggle_vk(),
            config,
            bangla: Cell::new(true),
            attribute_atom: Cell::new(0),
            app_name,
            ui: Rc::new(RefCell::new(UiState::default())),
            ui_element: RefCell::new(None),
            ui_element_id: Cell::new(0),
            draw_ourselves: Cell::new(true),
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

/// An unshifted digit on the main row, as a value 0..9. Shift+digit is punctuation, not a digit.
fn digit(vk: u32) -> Option<usize> {
    if (0x30..=0x39).contains(&vk) && !key_down(VK_SHIFT) {
        Some((vk - 0x30) as usize)
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
        (VK_OEM_PERIOD_CODE, false) => DARI,
        (VK_OEM_PERIOD_CODE, true) => ">",
        (0xBA, false) => ";",
        (0xBA, true) => ":",
        (0xBF, false) => "/",
        (0xBF, true) => "?",
        (0xDE, false) => "'",
        (0xDE, true) => "\"",
        (0xBD, false) => "-",
        (0xBD, true) => "_",
        (0xBB, false) => "=",
        (0xBB, true) => "+",
        (0xDB, false) => "[",
        (0xDB, true) => "{",
        (0xDD, false) => "]",
        (0xDD, true) => "}",
        (0xDC, false) => "\\",
        (0xDC, true) => "|",
        (0xC0, false) => "`",
        (0xC0, true) => "~",
        (0x31, true) => "!",
        (0x32, true) => "@",
        (0x33, true) => "#",
        (0x34, true) => "$",
        (0x35, true) => "%",
        (0x36, true) => "^",
        (0x37, true) => "&",
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

    /// The full stop alone, typed with no word open. The daṛi is the Bangla sentence end whether or
    /// not a word was being composed at the time, and after a Space it never is.
    fn lone_period(&self, vk: u32) -> bool {
        self.config.danda_for_period
            && !self.composing()
            && vk == VK_OEM_PERIOD_CODE
            && !key_down(VK_SHIFT)
    }

    fn lone_digit(&self, vk: u32) -> Option<usize> {
        if self.config.bangla_digits && !self.composing() {
            digit(vk)
        } else {
            None
        }
    }

    /// Whether this key is ours. Asked twice per key (OnTestKeyDown, then OnKeyDown) and the answer
    /// must be the same both times, so it depends on nothing that the first call changes.
    fn wants(&self, vk: u32) -> bool {
        if key_down(VK_CONTROL) || key_down(VK_MENU) {
            return false;
        }
        // The toggle is ours in both modes: it is how you get back.
        if self.toggle_vk == Some(vk) {
            return true;
        }
        if !self.bangla.get() {
            return false;
        }
        if letter(vk).is_some() || self.lone_digit(vk).is_some() || self.lone_period(vk) {
            return true;
        }
        if !self.composing() {
            return false;
        }
        // While a word is open every digit is ours -- a pick if there is such a candidate, swallowed
        // if not -- so an application never receives a stray "7" in the middle of a composition.
        digit(vk).is_some()
            || punctuation(vk).is_some()
            || matches!(
                VIRTUAL_KEY(vk as u16),
                VK_SPACE | VK_RETURN | VK_ESCAPE | VK_BACK | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN
            )
    }

    fn handle(&self, ctx: &ITfContext, vk: u32) -> Result<()> {
        if self.toggle_vk == Some(vk) {
            // Finish whatever is open before switching, so the mode change never eats a word.
            if self.composing() {
                self.commit_current(ctx, "")?;
            }
            let now = !self.bangla.get();
            self.bangla.set(now);
            log!("mode: {}", if now { "Bangla" } else { "English passthrough" });
            return Ok(());
        }
        if !self.bangla.get() {
            return Ok(());
        }
        if let Some(d) = self.lone_digit(vk) {
            return self.insert(ctx, BANGLA_DIGITS[d]);
        }
        if self.lone_period(vk) {
            return self.insert(ctx, DARI);
        }
        if let Some(ch) = letter(vk) {
            self.state.borrow_mut().buffer.push(ch);
            self.ask(TYPE_DEADLINE_MS);
            self.show(ctx)?;
            self.update_window(ctx);
            return Ok(());
        }
        if let Some(d) = digit(vk) {
            // 1..9 picks; 0 and out-of-range digits are swallowed rather than typed into the word.
            if d == 0 {
                return Ok(());
            }
            self.settle(ctx);
            let picked = self.state.borrow().candidates.get(d - 1).cloned();
            return match picked {
                Some(word) => self.commit(ctx, d - 1, word, ""),
                None => Ok(()),
            };
        }
        if let Some(mark) = punctuation(vk) {
            let mark = if mark == DARI && !self.config.danda_for_period { "." } else { mark };
            return self.commit_current(ctx, mark);
        }
        match VIRTUAL_KEY(vk as u16) {
            VK_BACK => {
                let empty = {
                    let mut s = self.state.borrow_mut();
                    s.buffer.pop();
                    s.retyped = true;
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
            VK_RIGHT | VK_DOWN => self.move_highlight(ctx, 1),
            VK_LEFT | VK_UP => self.move_highlight(ctx, -1),
            _ => Ok(()),
        }
    }

    fn move_highlight(&self, ctx: &ITfContext, by: isize) -> Result<()> {
        self.settle(ctx);
        {
            let mut s = self.state.borrow_mut();
            let n = s.candidates.len();
            if n > 0 {
                s.cursor = (s.cursor as isize + by).rem_euclid(n as isize) as usize;
                s.chosen = true;
            }
        }
        self.update_window(ctx);
        Ok(())
    }

    /// Commit whatever is highlighted, followed by `trailing`.
    ///
    /// Re-asks with the commit deadline only when the person has not chosen for themselves. That
    /// second question exists so the committed word comes from the full ranking rather than the
    /// fast path shown while typing -- but it returns a different list, and running it after an
    /// explicit arrow selection threw that selection away and committed something else.
    fn commit_current(&self, ctx: &ITfContext, trailing: &str) -> Result<()> {
        let reask = {
            let s = self.state.borrow();
            !s.chosen && s.worth_refining()
        };
        if reask {
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
        let wanted = self.config.candidates.clamp(1, 9);
        let mut s = self.state.borrow_mut();
        let buffer = s.buffer.clone();
        let context = s.context.clone();
        match s.engine.suggest(&buffer, &context, wanted, deadline_ms) {
            Some(reply) => {
                s.candidates = reply.candidates;
                s.partial = reply.partial;
                s.strong = reply.strong;
            }
            None => {
                s.candidates.clear();
                s.partial = false;
                s.strong = false;
            }
        }
        s.cursor = 0;
        s.chosen = false;
    }

    /// Replace a fast-path list with the full ranking before the person picks from it.
    ///
    /// The list shown while typing is whatever the engine had within the typing deadline, and for
    /// some words that is not the list the model would give: "chiro" shows ছাড়া while the full
    /// ranking puts চিরো first. Committing with Space already re-asks, so the committed word was
    /// always the better one -- but reaching for an arrow key or a number means choosing from what
    /// is on screen, and that has to be the same list. Settling costs the time of one full query,
    /// paid once, at the moment someone has stopped typing to look.
    fn settle(&self, ctx: &ITfContext) {
        if !self.state.borrow().worth_refining() {
            return;
        }
        self.ask(COMMIT_DEADLINE_MS);
        self.update_window(ctx);
    }

    /// Start the composition if there is none, then set its text to the buffer.
    fn show(&self, ctx: &ITfContext) -> Result<()> {
        let state = self.state.clone();
        let sink: ITfCompositionSink = self.to_interface();
        let ctx2 = ctx.clone();
        let atom = self.attribute_atom.get();
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
            if atom != 0 {
                mark_composing(&ctx2, ec, &range, atom);
            }
            place_caret_after(&ctx2, ec, &range)
        })
    }

    /// Type text straight into the document, with no composition. Used for Bengali digits and the
    /// lone daṛi.
    fn insert(&self, ctx: &ITfContext, text: &str) -> Result<()> {
        let text = wide(text);
        let ctx2 = ctx.clone();
        EditSession::run(ctx, self.client_id.get(), move |ec| {
            let insert: ITfInsertAtSelection = ctx2.cast()?;
            let range =
                unsafe { insert.InsertTextAtSelection(ec, INSERT_TEXT_AT_SELECTION_FLAGS(0), &text) }?;
            place_caret_after(&ctx2, ec, &range)
        })
    }

    /// Commit `word` (plus `trailing`) in place of the composition, report it, remember it.
    ///
    /// Reported on every commit, not only when the word differs from the Latin typed. The engine's
    /// learn call is also the pilot's only counting path: `index` is how "first suggestion taken"
    /// is measured, and an earlier version sent nothing for index 0, which made that number
    /// meaningless.
    fn commit(&self, ctx: &ITfContext, index: usize, word: String, trailing: &str) -> Result<()> {
        {
            let mut s = self.state.borrow_mut();
            let buffer = s.buffer.clone();
            let context = s.context.clone();
            let top1 = s.candidates.first().cloned().unwrap_or_default();
            let retyped = s.retyped;
            if !buffer.is_empty() {
                s.engine.learn(&Commit {
                    roman: &buffer,
                    chosen: &word,
                    top1: &top1,
                    index,
                    retyped,
                    app: &self.app_name,
                    context: &context,
                });
            }
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

    /// Take the candidate list off the screen, wherever it is being drawn.
    ///
    /// Every path that ends a word goes through here -- committed, cancelled, focus lost, service
    /// deactivated -- which is why the remote window is hidden here rather than at each of them. It
    /// was hidden in only one of them at first, and a list drawn by the engine then stayed on screen
    /// after typing stopped and floated over whatever application came to the front next.
    fn hide_window(&self) {
        self.end_ui_element();
        if let Some(w) = self.window.borrow().as_ref() {
            w.hide();
        }
        if crate::in_app_container() {
            // Borrowed separately: `end_ui_element` above can re-enter through TSF, and holding a
            // mutable borrow of the state across that is a panic waiting for an unlucky host.
            self.state.borrow_mut().engine.ui_hide();
        }
    }

    fn ui_element_mgr(&self) -> Option<ITfUIElementMgr> {
        self.thread_mgr.borrow().as_ref()?.cast().ok()
    }

    /// Publish the list, and find out who is going to draw it.
    ///
    /// Returns true when the host wants *us* to draw. An immersive application answers false and
    /// renders the list itself from the published element; a host that does not support UI elements
    /// at all leaves us drawing, which is the desktop behaviour we already had.
    fn begin_ui_element(&self, ctx: &ITfContext) -> bool {
        if self.ui_element.borrow().is_some() {
            return self.draw_ourselves.get();
        }
        {
            let mut ui = self.ui.borrow_mut();
            ui.document = unsafe { ctx.GetDocumentMgr() }.ok();
            ui.shown = true;
        }
        let element: ITfUIElement = CandidateUi::new(self.ui.clone()).into();
        let Some(mgr) = self.ui_element_mgr() else {
            self.draw_ourselves.set(true);
            return true;
        };
        let mut show = BOOL(1);
        let mut id = 0u32;
        if unsafe { mgr.BeginUIElement(&element, &mut show, &mut id) }.is_err() {
            self.draw_ourselves.set(true);
            return true;
        }
        self.ui_element_id.set(id);
        *self.ui_element.borrow_mut() = Some(element);

        // Measured, not assumed: Telegram -- immersive, TF_TMF_IMMERSIVEMODE set -- answers
        // show = TRUE and expects us to draw, exactly as Notepad does. A host that answers FALSE
        // has not been observed yet, so this stays as TSF specifies rather than being overridden
        // on a guess about what Store applications do.
        let ours = show.as_bool();
        self.draw_ourselves.set(ours);
        log!(
            "candidate list published (id {id}); {} draws it",
            if ours { "we" } else { "the application" }
        );
        ours
    }

    fn end_ui_element(&self) {
        let element = self.ui_element.borrow_mut().take();
        if element.is_some() {
            if let Some(mgr) = self.ui_element_mgr() {
                let _ = unsafe { mgr.EndUIElement(self.ui_element_id.get()) };
            }
        }
        self.ui.borrow_mut().shown = false;
        self.draw_ourselves.set(true);
    }

    /// Put the candidate list under the text being composed.
    ///
    /// The anchor comes from the context view, which is the only thing that knows where the
    /// composition ended up on screen -- an application may have scrolled, wrapped or transformed
    /// it. When it cannot say yet -- and for the very first letter of a word it often cannot, the
    /// composition having been created a moment ago and not laid out -- the caret is used instead.
    /// The list is hidden only when neither is known.
    fn update_window(&self, ctx: &ITfContext) {
        let (candidates, cursor) = {
            let s = self.state.borrow();
            (s.candidates.clone(), s.cursor)
        };
        if candidates.is_empty() {
            self.hide_window();
            return;
        }

        // Publish first, then draw only if the host wants us to. The shared state is updated before
        // BeginUIElement so a host that reads the element during that call already sees the list.
        {
            let mut ui = self.ui.borrow_mut();
            ui.candidates = candidates.clone();
            ui.cursor = cursor;
        }
        let new_element = self.ui_element.borrow().is_none();
        let ours = self.begin_ui_element(ctx);
        if !new_element {
            if let Some(mgr) = self.ui_element_mgr() {
                let _ = unsafe { mgr.UpdateUIElement(self.ui_element_id.get()) };
            }
        }
        if !ours || !self.ui.borrow().shown {
            // The application is drawing the list, or TSF has asked for it to be hidden. Either
            // way our own window must not be on screen alongside it.
            if let Some(w) = self.window.borrow().as_ref() {
                w.hide();
            }
            if crate::in_app_container() {
                self.state.borrow_mut().engine.ui_hide();
            }
            return;
        }

        // In a sandbox the engine draws it. A window created here would never reach the desktop:
        // every call reports success and nothing is composed, which is exactly how this went
        // unnoticed until the window was looked for and was not on the desktop at all.
        if crate::in_app_container() {
            let anchor = match self.remote_anchor(ctx) {
                Some(r) => r,
                None => {
                    self.state.borrow_mut().engine.ui_hide();
                    return;
                }
            };
            let mut s = self.state.borrow_mut();
            s.engine.ui_show(&candidates, cursor, anchor);
            return;
        }

        if self.window.borrow().is_none() {
            // This DLL's own module, not the host application's: a window class registered from a
            // DLL must name that DLL, or the class outlives the code its procedure points into.
            let Some(window) = CandidateWindow::new(
                crate::module_handle().into(),
                Box::new(|| {
                    let c = Config::load();
                    (crate::config::stamp(), likhi_ui::Fonts { font_name: c.font_name, font_size: c.font_size })
                }),
            ) else {
                log!("candidate window could not be created");
                return;
            };
            // What the pause timer runs: the full ranking for whatever is being composed. Only when
            // the visible list came from the fast path, and never over an explicit choice.
            let state = self.state.clone();
            let wanted = self.config.candidates.clamp(1, 9);
            window.set_refiner(Box::new(move || {
                let mut s = state.borrow_mut();
                if !s.worth_refining() || s.buffer.is_empty() || s.composition.is_none() {
                    return None;
                }
                let buffer = s.buffer.clone();
                let context = s.context.clone();
                let reply = s.engine.suggest(&buffer, &context, wanted, COMMIT_DEADLINE_MS)?;
                s.candidates = reply.candidates;
                s.partial = reply.partial;
                s.strong = reply.strong;
                if !s.chosen {
                    s.cursor = 0;
                }
                Some(likhi_ui::Content {
                    candidates: s.candidates.clone(),
                    cursor: s.cursor,
                })
            }));
            *self.window.borrow_mut() = Some(window);
        }
        let anchor = match self.anchor(ctx) {
            Some(r) => r,
            None => {
                log!("anchor: none available; the candidate list cannot be shown");
                self.hide_window();
                return;
            }
        };
        if let Some(w) = self.window.borrow().as_ref() {
            w.show(&candidates, cursor, &anchor);
        }
        // The anchor decides where the list appears, and a rectangle in the wrong coordinate space
        // puts it off the edge of the screen while every other step still reports success. Logged
        // with the visible screen area so the two can be compared directly.
        vlog!(
            "anchor rect l={} t={} r={} b={} (virtual screen {}x{} at {},{})",
            anchor.left,
            anchor.top,
            anchor.right,
            anchor.bottom,
            unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) },
            unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) },
            unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
            unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
        );
    }

    /// Where to put the candidate list, in screen coordinates.
    ///
    /// Three sources, most precise first. The last one answers whenever anything has focus, so a
    /// `None` here means there is nothing on screen to anchor to at all.
    fn anchor(&self, ctx: &ITfContext) -> Option<RECT> {
        self.composition_rect(ctx).or_else(caret_rect).or_else(|| {
            let r = focus_window_rect();
            if r.is_some() {
                vlog!("anchor: falling back to the focused window");
            }
            r
        })
    }

    /// The same anchor, as plain numbers for the engine to draw against.
    fn remote_anchor(&self, ctx: &ITfContext) -> Option<(i32, i32, i32, i32)> {
        let r = self.anchor(ctx)?;
        vlog!(
            "remote anchor l={} t={} r={} b={}",
            r.left,
            r.top,
            r.right,
            r.bottom
        );
        Some((r.left, r.top, r.right, r.bottom))
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
            if let Err(e) = unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) } {
                // TS_E_NOLAYOUT is ordinary for a composition created a moment ago. Anything else
                // is worth seeing, because this decides whether the list can be placed at all.
                vlog!("anchor: GetTextExt failed (0x{:08X})", e.code().0 as u32);
                return Err(e);
            }
            // A zero-area rectangle means "not laid out yet".
            if rect.right <= rect.left || rect.bottom <= rect.top {
                vlog!("anchor: composition rect is empty (clipped={})", clipped.as_bool());
                return Ok(());
            }
            // A clipped rectangle used to be rejected here. That was wrong: clipped means the
            // composition is partly scrolled out of view, not that its position is unknown. An
            // application that always reports clipping was left with no anchor, and therefore no
            // candidate list at all -- silently, because this path logged nothing either.
            if clipped.as_bool() {
                vlog!("anchor: composition rect is clipped; using it anyway");
            }
            out.set(Some(rect));
            Ok(())
        });
        if ok.is_err() {
            return None;
        }
        result.get()
    }
}

/// The caret of the focused window, in screen coordinates. The fallback anchor for the candidate
/// list when TSF cannot yet say where the composition is.
fn caret_rect() -> Option<RECT> {
    unsafe {
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        // Thread 0: the foreground thread, which is the one typing.
        GetGUIThreadInfo(0, &mut info).ok()?;
        if info.hwndCaret.is_invalid() {
            // A XAML application draws its own caret and never creates a system one, so this is
            // the ordinary answer there rather than a failure.
            vlog!("anchor: the focused thread has no system caret");
            return None;
        }
        let mut top_left = POINT { x: info.rcCaret.left, y: info.rcCaret.top };
        let mut bottom_right = POINT { x: info.rcCaret.right, y: info.rcCaret.bottom };
        if !ClientToScreen(info.hwndCaret, &mut top_left).as_bool()
            || !ClientToScreen(info.hwndCaret, &mut bottom_right).as_bool()
        {
            return None;
        }
        if bottom_right.y <= top_left.y {
            return None;
        }
        Some(RECT {
            left: top_left.x,
            top: top_left.y,
            right: bottom_right.x.max(top_left.x + 1),
            bottom: bottom_right.y,
        })
    }
}

/// The focused window, as a caret-sized box at its lower-left corner. The last-resort anchor.
///
/// Neither of the other two works in a XAML application: it draws its own caret, so there is no
/// system caret to find, and its text layout is not reported through `GetTextExt`. Both come back
/// empty, and the candidate list was then never shown -- which is what "Likhi types but shows no
/// suggestions" in Telegram was. Anchoring on the window is imprecise, but it is the difference
/// between a keyboard a person can pick words in and one that silently commits the first guess.
///
/// The lower-left corner rather than the centre because a text field in a chat application sits at
/// the bottom of the window, and `place` puts the list above an anchor with no room below it.
fn focus_window_rect() -> Option<RECT> {
    unsafe {
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let hwnd = match GetGUIThreadInfo(0, &mut info) {
            Ok(()) if !info.hwndFocus.is_invalid() => info.hwndFocus,
            _ => GetForegroundWindow(),
        };
        if hwnd.is_invalid() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        if r.right <= r.left || r.bottom <= r.top {
            return None;
        }
        // A thin box the height of a line of text, inset from the corner so the list does not sit
        // flush against the window edge.
        Some(RECT {
            left: r.left + 12,
            top: (r.bottom - 56).max(r.top),
            right: r.left + 13,
            bottom: (r.bottom - 12).max(r.top + 1),
        })
    }
}

/// Tag `range` as text being composed, so the application underlines it.
///
/// Best effort: an application that does not support the property simply draws plain text, which is
/// what happened before this existed. It must never stop the word being typed.
fn mark_composing(ctx: &ITfContext, ec: u32, range: &ITfRange, atom: u32) {
    unsafe {
        let Ok(property) = ctx.GetProperty(&GUID_PROP_ATTRIBUTE) else {
            return;
        };
        let value = VARIANT::from(atom as i32);
        let _ = property.SetValue(ec, range, &value);
    }
}

/// Collapse `range` to its end and make that the selection, so typing continues after the text.
fn place_caret_after(ctx: &ITfContext, ec: u32, range: &ITfRange) -> Result<()> {
    unsafe {
        range.Collapse(ec, TF_ANCHOR_END)?;
        // TF_SELECTION holds its range as ManuallyDrop: COM will not release it for us, so the
        // reference the clone took is released by hand once the call is over.
        let mut selection = TF_SELECTION {
            range: ManuallyDrop::new(Some(range.clone())),
            style: TF_SELECTIONSTYLE {
                ase: TF_AE_NONE,
                fInterimChar: BOOL(0),
            },
        };
        let result = ctx.SetSelection(ec, std::slice::from_ref(&selection));
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
        // Before anything else, and before the thread manager goes: the keyboard is being taken
        // away from this application, and a list drawn by the engine outlives this process unless
        // it is told. Closing an application mid-word must not leave one floating on the desktop.
        self.hide_window();
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
        log!("activate in {} client={tid} flags={flags:#x}", self.app_name);
        unsafe {
            let source: ITfSource = tim.cast()?;
            let sink: ITfThreadMgrEventSink = self.to_interface();
            let cookie = source.AdviseSink(&ITfThreadMgrEventSink::IID, &sink)?;
            self.thread_mgr_cookie.set(cookie);

            let keys: ITfKeystrokeMgr = tim.cast()?;
            let key_sink: ITfKeyEventSink = self.to_interface();
            keys.AdviseKeyEventSink(tid, &key_sink, true)?;

            // The atom that names our display attribute on a range. Without it the composition is
            // drawn like committed text, so nothing tells the person Space will replace it.
            if let Ok(categories) = CoCreateInstance::<_, ITfCategoryMgr>(
                &CLSID_TF_CategoryMgr,
                None,
                CLSCTX_INPROC_SERVER,
            ) {
                match categories.RegisterGUID(&GUID_DISPLAY_ATTRIBUTE) {
                    Ok(atom) => self.attribute_atom.set(atom),
                    Err(e) => log!("no display attribute atom ({e}); composition will not underline"),
                }
            }
        }
        *self.thread_mgr.borrow_mut() = Some(tim);
        Ok(())
    }
}

// ------------------------------------------------------------------ display attribute

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(EnumDisplayAttributeInfo::new().into())
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        if guid.is_null() || unsafe { *guid } != GUID_DISPLAY_ATTRIBUTE {
            return Err(E_INVALIDARG.into());
        }
        Ok(DisplayAttributeInfo.into())
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
    fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
        // The application lost the keyboard. A list left floating over whatever came to the front
        // is the one thing about a candidate window people remember.
        if !foreground.as_bool() {
            self.hide_window();
        }
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
