//! What `regsvr32` does to this DLL: make it a COM class, then make that class a keyboard.
//!
//! Three things have to exist for the keyboard to appear in Win+Space, and every one of them was a
//! separate failure with the PIME shell, so each is written explicitly here rather than delegated:
//!
//! 1. the COM class, under `HKLM\SOFTWARE\Classes\CLSID\{clsid}`, pointing at this DLL;
//! 2. the language profile, which `ITfInputProcessorProfileMgr::RegisterProfile` writes under
//!    `HKLM\SOFTWARE\Microsoft\CTF\TIP` -- this is the entry Windows lists as a keyboard;
//! 3. the categories, which tell TSF what kind of text service this is.
//!
//! The 32-bit build registered by the 32-bit `regsvr32` lands under `WOW6432Node` automatically.

use std::ffi::c_void;

use windows::core::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::*;

use crate::guids::*;

/// A keyboard that draws its own candidate UI, works in secure desktops (the password prompt) and
/// in immersive (Store) applications, and may show a tray item.
const CATEGORIES: &[GUID] = &[
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    GUID_TFCAT_TIPCAP_SECUREMODE,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn clsid_key() -> String {
    format!("SOFTWARE\\Classes\\CLSID\\{{{:?}}}", CLSID_LIKHI)
}

fn set_string(subkey: &str, name: Option<&str>, value: &str) -> Result<()> {
    let sub = wide(subkey);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let val = wide(value);
        let bytes = std::slice::from_raw_parts(val.as_ptr() as *const u8, val.len() * 2);
        let name_w = name.map(wide);
        let name_p = name_w
            .as_ref()
            .map(|w| PCWSTR(w.as_ptr()))
            .unwrap_or(PCWSTR::null());
        let r = RegSetValueExW(key, name_p, None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
        r.ok()
    }
}

/// `dll_path` is this DLL; `icon_path` is the file shown beside the keyboard's name.
pub fn register(dll_path: &str, icon_path: &str) -> Result<()> {
    let key = clsid_key();
    set_string(&key, None, DESCRIPTION)?;
    set_string(&format!("{key}\\InprocServer32"), None, dll_path)?;
    set_string(&format!("{key}\\InprocServer32"), Some("ThreadingModel"), "Apartment")?;

    unsafe {
        let profiles: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        let desc = wide(DESCRIPTION);
        let icon = wide(icon_path);
        // The substitute layout is the whole point of this call: see guids.rs.
        profiles.RegisterProfile(
            &CLSID_LIKHI,
            LANGID_BN_BD,
            &GUID_PROFILE,
            &desc[..desc.len() - 1],
            &icon[..icon.len() - 1],
            0,
            HKL(HKL_SUBSTITUTE_US as *mut c_void),
            0,
            true,
            0,
        )?;

        let categories: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for category in CATEGORIES {
            categories.RegisterCategory(&CLSID_LIKHI, category, &CLSID_LIKHI)?;
        }
    }
    Ok(())
}

pub fn unregister() -> Result<()> {
    // Best effort throughout: a half-registered service must still be removable.
    unsafe {
        if let Ok(categories) = CoCreateInstance::<_, ITfCategoryMgr>(
            &CLSID_TF_CategoryMgr,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            for category in CATEGORIES {
                let _ = categories.UnregisterCategory(&CLSID_LIKHI, category, &CLSID_LIKHI);
            }
        }
        if let Ok(profiles) = CoCreateInstance::<_, ITfInputProcessorProfileMgr>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            let _ = profiles.UnregisterProfile(&CLSID_LIKHI, LANGID_BN_BD, &GUID_PROFILE, 0);
        }
        if let Ok(legacy) = CoCreateInstance::<_, ITfInputProcessorProfiles>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            let _ = legacy.Unregister(&CLSID_LIKHI);
        }
        let key = wide(&clsid_key());
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()));
    }
    Ok(())
}
