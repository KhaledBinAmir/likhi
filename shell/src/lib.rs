//! Likhi text service for Windows: a Text Services Framework keyboard, in Rust.
//!
//! This DLL is loaded into every application that switches to the Likhi keyboard, so two rules run
//! through the whole crate. Nothing may panic across the COM boundary (`panic = "abort"` in
//! Cargo.toml makes any slip loud rather than corrupting a host); and nothing here does the
//! linguistic work -- that stays in the Likhi engine, reached over the same local socket protocol
//! the PIME shell used, so the engine did not have to change for the shell to.

mod candidates;
mod config;
mod display;
mod edit;
mod engine;
mod guids;
mod log;
mod registry;
mod service;
mod uielement;

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::TextServices::ITfTextInputProcessorEx;

use crate::guids::CLSID_LIKHI;
use crate::service::TextService;

static MODULE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// This DLL's own module handle. A window class registered from a DLL must name the DLL, not the
/// host executable, or the class refers to code that can go away underneath it.
pub fn module_handle() -> HMODULE {
    HMODULE(MODULE.load(Ordering::Relaxed))
}

pub(crate) fn module_path() -> String {
    // Long enough for paths well past MAX_PATH; a truncated path would register a DLL that does
    // not exist, which is a keyboard that silently never loads.
    let mut buf = [0u16; 4096];
    let n = unsafe { GetModuleFileNameW(Some(module_handle()), &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let service: ITfTextInputProcessorEx = TextService::new().into();
        unsafe { service.query(riid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

#[no_mangle]
pub extern "system" fn DllMain(instance: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(instance.0, Ordering::Relaxed);
        // We keep no per-thread state, so skip the attach/detach notifications for every thread.
        let _ = unsafe { DisableThreadLibraryCalls(HMODULE(instance.0)) };
    }
    BOOL(1)
}

/// `unsafe` because it dereferences pointers COM hands it, and the signature should say so; the
/// export is the same either way.
///
/// # Safety
/// Called by COM with `rclsid` and `riid` pointing at valid GUIDs and `object` at writable storage
/// for one interface pointer. Nothing else calls it.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || *rclsid != CLSID_LIKHI {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = ClassFactory.into();
    factory.query(riid, object)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Stay loaded for the life of the host. Unloading a text service while TSF may still hold a
    // pointer to it is a crash that lands in someone else's application.
    S_FALSE
}

#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let dll = module_path();
    // The icon shown in Win+Space sits beside the DLL.
    let icon = std::path::Path::new(&dll)
        .with_file_name("likhi.ico")
        .to_string_lossy()
        .into_owned();
    match registry::register(&dll, &icon) {
        Ok(()) => {
            log!("registered {dll}");
            S_OK
        }
        Err(e) => {
            log!("register failed: {e}");
            e.code()
        }
    }
}

#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    match registry::unregister() {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}
