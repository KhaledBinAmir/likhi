//! Likhi text service for Windows: a Text Services Framework keyboard, in Rust.
//!
//! This DLL is loaded into every application that switches to the Likhi keyboard, so two rules run
//! through the whole crate. Nothing may panic across the COM boundary (`panic = "abort"` in
//! Cargo.toml makes any slip loud rather than corrupting a host); and nothing here does the
//! linguistic work -- that stays in the Likhi engine, reached over the same local socket protocol
//! the PIME shell used, so the engine did not have to change for the shell to.

mod candidates;
mod edit;
mod engine;
mod guids;
mod log;
mod registry;
mod service;

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

fn module_path() -> String {
    let mut buf = [0u16; 1024];
    let handle = HMODULE(MODULE.load(Ordering::Relaxed));
    let n = unsafe { GetModuleFileNameW(Some(handle), &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
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

#[no_mangle]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if rclsid.is_null() || *rclsid != CLSID_LIKHI {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory.into();
        factory.query(riid, object)
    }
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
