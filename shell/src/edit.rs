//! Edit sessions: the only way to touch a document in TSF.
//!
//! An application's text is never modified directly. The text service asks the context for an edit
//! session, and TSF calls back into `DoEditSession` with an *edit cookie* that authorises reads and
//! writes for the duration of that call and no longer. Every change to the composition in this
//! crate goes through here, so the rule is enforced in one place.

use std::cell::RefCell;

use windows::core::*;
use windows::Win32::UI::TextServices::*;

type Body = Box<dyn FnOnce(u32) -> Result<()>>;

#[implement(ITfEditSession)]
pub struct EditSession {
    body: RefCell<Option<Body>>,
}

impl EditSession {
    /// Run `body` inside a synchronous read-write edit session on `context`.
    ///
    /// Synchronous is what a keystroke handler needs: the key has to be consumed and the document
    /// updated before the next one arrives, and TSF grants a synchronous session when the request
    /// comes from inside a key event, which is the only place this crate asks from.
    pub fn run(
        context: &ITfContext,
        client_id: u32,
        body: impl FnOnce(u32) -> Result<()> + 'static,
    ) -> Result<()> {
        let session: ITfEditSession = EditSession {
            body: RefCell::new(Some(Box::new(body))),
        }
        .into();
        let hr = unsafe {
            context.RequestEditSession(client_id, &session, TF_ES_SYNC | TF_ES_READWRITE)
        }?;
        hr.ok()
    }

    /// Same, but read-only: asking where the composition is on screen must not claim a write lock,
    /// which an application is entitled to refuse.
    pub fn run_read(
        context: &ITfContext,
        client_id: u32,
        body: impl FnOnce(u32) -> Result<()> + 'static,
    ) -> Result<()> {
        let session: ITfEditSession = EditSession {
            body: RefCell::new(Some(Box::new(body))),
        }
        .into();
        let hr =
            unsafe { context.RequestEditSession(client_id, &session, TF_ES_SYNC | TF_ES_READ) }?;
        hr.ok()
    }
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        match self.body.borrow_mut().take() {
            Some(body) => body(ec),
            None => Ok(()),
        }
    }
}
