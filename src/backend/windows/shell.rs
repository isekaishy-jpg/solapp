//! Shell association calls with scoped STA initialization and owned wide input.

use std::marker::PhantomData;
use std::rc::Rc;

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use crate::shell::{SAShellFailure, SAShellRequest};

pub(crate) struct Apartment {
    _owner: PhantomData<Rc<()>>,
}

impl Apartment {
    pub(crate) fn new() -> Result<Self, SAShellFailure> {
        // SAFETY: no reserved pointer; the call initializes only the current
        // thread. Successful S_OK and S_FALSE are both balanced by this guard.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|error| native("initialize shell apartment", error))?;
        Ok(Self {
            _owner: PhantomData,
        })
    }

    pub(crate) fn launch(&self, request: &SAShellRequest) -> Result<(), SAShellFailure> {
        let wide = request.wide()?;
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            // NOASYNC covers file/DDE completion where supported. URI handlers
            // may still continue externally; success never means process exit.
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: w!("open"),
            lpFile: PCWSTR(wide.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: initialized structure and terminated input live through the
        // call; no optional class, arguments, parent or process handle requested.
        unsafe { ShellExecuteExW(&mut info) }.map_err(|error| native("open association", error))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: guard is !Send and exists only after successful initialization.
        unsafe { CoUninitialize() };
    }
}

fn native(operation: &'static str, error: windows::core::Error) -> SAShellFailure {
    SAShellFailure::Native {
        operation,
        code: error.code().0,
        message: error.to_string(),
    }
}

/// Private abort-child setup only; never changes the host process's error mode.
#[cfg(test)]
pub(crate) fn suppress_child_abort_reporting() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetErrorMode(mode: u32) -> u32;
    }
    #[link(name = "ucrt")]
    unsafe extern "C" {
        fn _set_abort_behavior(flags: u32, mask: u32) -> u32;
    }
    // Win32 winbase.h values: SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
    // SEM_NOOPENFILEERRORBOX. CRT stdlib.h: _WRITE_ABORT_MSG, _CALL_REPORTFAULT.
    // Win32 uses its system ABI; the MSVC universal CRT uses the C ABI.
    // SAFETY: scalar-only APIs change reporting in this isolated test child.
    // No borrowed pointer, application callback or native window is involved.
    unsafe {
        SetErrorMode(0x0001 | 0x0002 | 0x8000);
        _set_abort_behavior(0, 0x0001 | 0x0002);
    }
}
