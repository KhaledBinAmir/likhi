//! The composition underline.
//!
//! Text being composed has to look provisional. Without a display attribute the application draws
//! our Latin exactly like text the person already committed, so there is nothing to say that Space
//! will replace it -- which is the single most confusing thing a new user meets.
//!
//! TSF asks for this through a display attribute provider: we publish one attribute, and mark the
//! composition range with its GUID atom. The application decides how to honour it; every one worth
//! supporting draws the underline.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::TextServices::*;

use crate::guids::GUID_DISPLAY_ATTRIBUTE;

/// A dotted underline in the text's own colour, with no background of its own.
///
/// Deliberately not a coloured background: the shell cannot know what the application's background
/// is, and the pilot's PIME shell showed dark composition text on a dark editor because it tried.
/// Leaving colour to the application and marking only the underline cannot produce that.
fn attribute() -> TF_DISPLAYATTRIBUTE {
    TF_DISPLAYATTRIBUTE {
        crText: TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        },
        crBk: TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        },
        lsStyle: TF_LS_DOT,
        fBoldLine: BOOL(0),
        crLine: TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        },
        bAttr: TF_ATTR_INPUT,
    }
}

#[implement(ITfDisplayAttributeInfo)]
pub struct DisplayAttributeInfo;

impl ITfDisplayAttributeInfo_Impl for DisplayAttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_DISPLAY_ATTRIBUTE)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Likhi composition"))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe { *pda = attribute() };
        Ok(())
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        // Not configurable from outside; the style is ours.
        Err(E_NOTIMPL.into())
    }

    fn Reset(&self) -> Result<()> {
        Ok(())
    }
}

/// The one-item collection TSF enumerates to discover the attribute above.
#[implement(IEnumTfDisplayAttributeInfo)]
pub struct EnumDisplayAttributeInfo {
    done: std::cell::Cell<bool>,
}

impl EnumDisplayAttributeInfo {
    pub fn new() -> Self {
        EnumDisplayAttributeInfo {
            done: std::cell::Cell::new(false),
        }
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for EnumDisplayAttributeInfo_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(EnumDisplayAttributeInfo::new().into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        let mut fetched = 0u32;
        if ulcount > 0 && !self.done.get() && !rginfo.is_null() {
            unsafe { *rginfo = Some(DisplayAttributeInfo.into()) };
            self.done.set(true);
            fetched = 1;
        }
        if !pcfetched.is_null() {
            unsafe { *pcfetched = fetched };
        }
        if fetched == 0 {
            return Err(S_FALSE.into());
        }
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.done.set(false);
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        if ulcount > 0 {
            self.done.set(true);
        }
        Ok(())
    }
}
