//! Identity of the Likhi text service.
//!
//! These are deliberately different from the GUIDs the PIME-based shell registers, so the two can be
//! installed side by side while this one is being developed: Windows tells them apart by CLSID and
//! profile, and neither registration disturbs the other.

use windows::core::GUID;

/// The COM class of the text service. This is what `regsvr32` registers and what TSF instantiates
/// inside every application that switches to the Likhi keyboard.
pub const CLSID_LIKHI: GUID = GUID::from_u128(0x1D24C804_FAD0_4B32_AEDD_1317F4E6221E);

/// The language profile: one keyboard entry under Bangla (Bangladesh) in Win+Space.
pub const GUID_PROFILE: GUID = GUID::from_u128(0x502AB3FE_5B7C_43E9_89D1_BE885846AE0D);

/// Our display attribute: the underline on text being composed.
pub const GUID_DISPLAY_ATTRIBUTE: GUID = GUID::from_u128(0x0F003D71_BA44_40A2_9A62_699FEBE259FA);

/// Identifies our candidate list to a host that renders UI elements itself. A host uses it to tell
/// one kind of element from another -- a candidate list from a reading window -- so it must be ours
/// alone and must not change.
pub const GUID_CANDIDATE_UI: GUID = GUID::from_u128(0x6C5CAD59_398F_494E_9C08_58644FA8EF20);

/// bn-BD.
pub const LANGID_BN_BD: u16 = 0x0845;

/// The keyboard layout this text service sits on: US English, 0x0409 layout for the 0x0409 language.
///
/// A text service sits on top of a physical layout, and Windows attaches Bengali INSCRIPT to bn-BD
/// by default, which maps the letter keys straight onto Bangla letters. The PIME shell had to work
/// around that by reading virtual key codes; here we declare the substitute layout at registration
/// and the problem cannot arise: every key arrives as the Latin character a US keyboard would give.
pub const HKL_SUBSTITUTE_US: isize = 0x0409_0409;

/// The name Windows shows in the keyboard picker.
///
/// In Bangla, because this is a Bangla keyboard and the people choosing it read Bangla; the picker
/// puts it beside the other Bangla entries, where an English label is the odd one out.
pub const DESCRIPTION: &str = "লিখি — বাংলা ফোনেটিক";
