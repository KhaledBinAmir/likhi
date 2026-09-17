//! The candidate list published as a TSF UI element.
//!
//! A text service that draws its own window works on the desktop and nowhere else. Store apps --
//! Unigram, WhatsApp, Mail -- activate a text service with TF_TMF_IMMERSIVEMODE, and a classic
//! popup is not composited over their surface: the list simply never appears, and the person sees
//! the first suggestion committed with no way to reach the others. That is exactly what the pilot
//! reported.
//!
//! Windows' own IME does not draw a window there. It *publishes* the list, through
//! `ITfUIElementMgr::BeginUIElement`, and the host decides who draws:
//!
//! * the host answers "show it yourself" -- an ordinary desktop application, which has no idea how
//!   to render a candidate list -- and we draw our Direct2D window as before;
//! * the host answers "I will draw it" -- an immersive one -- and it reads the list back through
//!   this interface and renders it in its own style, correctly placed and composited.
//!
//! Either way the list appears, which is what "it should show up everywhere, like Windows' own"
//! means in practice. The same data drives both paths.

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::TextServices::*;

use crate::guids::GUID_CANDIDATE_UI;

/// Everything the host can ask about the list. Shared with the text service, which updates it and
/// then tells TSF the element changed.
#[derive(Default)]
pub struct UiState {
    pub candidates: Vec<String>,
    pub cursor: usize,
    pub document: Option<ITfDocumentMgr>,
    /// Whether the host has asked for the element to be visible. TSF may hide it for reasons of its
    /// own -- a full-screen game, a secure desktop -- and we must honour that.
    pub shown: bool,
}

#[implement(ITfUIElement, ITfCandidateListUIElement)]
pub struct CandidateUi {
    state: Rc<RefCell<UiState>>,
}

impl CandidateUi {
    pub fn new(state: Rc<RefCell<UiState>>) -> Self {
        CandidateUi { state }
    }
}

impl ITfUIElement_Impl for CandidateUi_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Likhi candidates"))
    }

    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_CANDIDATE_UI)
    }

    fn Show(&self, bshow: BOOL) -> Result<()> {
        self.state.borrow_mut().shown = bshow.as_bool();
        Ok(())
    }

    fn IsShown(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.state.borrow().shown))
    }
}

impl ITfCandidateListUIElement_Impl for CandidateUi_Impl {
    /// What changed since the host last looked. We rebuild the list wholesale on every keystroke,
    /// so everything except the page layout is potentially different.
    fn GetUpdatedFlags(&self) -> Result<u32> {
        Ok(TF_CLUIE_DOCUMENTMGR
            | TF_CLUIE_COUNT
            | TF_CLUIE_SELECTION
            | TF_CLUIE_STRING
            | TF_CLUIE_CURRENTPAGE)
    }

    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        self.state
            .borrow()
            .document
            .clone()
            .ok_or_else(|| E_FAIL.into())
    }

    fn GetCount(&self) -> Result<u32> {
        Ok(self.state.borrow().candidates.len() as u32)
    }

    fn GetSelection(&self) -> Result<u32> {
        Ok(self.state.borrow().cursor as u32)
    }

    fn GetString(&self, uindex: u32) -> Result<BSTR> {
        let state = self.state.borrow();
        match state.candidates.get(uindex as usize) {
            Some(word) => Ok(BSTR::from(word.as_str())),
            // Out of range is a question, not a failure: answer with nothing rather than an error
            // the host has to interpret.
            None => Ok(BSTR::new()),
        }
    }

    /// One page, always. Likhi shows at most nine candidates and never pages; telling the host so
    /// is simpler than pretending otherwise, and it stops the host drawing paging controls that
    /// would do nothing.
    fn GetPageIndex(&self, pindex: *mut u32, usize_: u32, pupagecnt: *mut u32) -> Result<()> {
        if pupagecnt.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe { *pupagecnt = 1 };
        // A null buffer means the host is only asking how many pages there are.
        if !pindex.is_null() && usize_ >= 1 {
            unsafe { *pindex = 0 };
        }
        Ok(())
    }

    fn SetPageIndex(&self, _pindex: *const u32, _upagecnt: u32) -> Result<()> {
        // The list is ours to lay out; a host cannot repaginate it.
        Err(E_NOTIMPL.into())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        Ok(0)
    }
}
