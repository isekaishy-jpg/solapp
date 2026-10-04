//! Retained native generations and owner-thread borrowed interoperability.

use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroIsize;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ThreadId};

use winit::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, WindowHandle,
};
use winit::window::Window;

use crate::{SAError, SANativeOperation, SAWindowTarget};

/// Transferable retention of one actual native window generation.
///
/// Release this lease only after renderer surface/presentation use has ended.
/// Metadata is transferable; native extraction requires the creating owner thread.
/// This lease does not implement raw-window-handle traits.
pub struct SAWindowAccess {
    // Taking this field in Drop releases the lease before its wake is signalled.
    anchor: Option<Arc<WindowAnchor>>,
}

impl SAWindowAccess {
    fn anchor(&self) -> &Arc<WindowAnchor> {
        self.anchor.as_ref().expect("live lease retains its anchor")
    }

    /// The exact native generation retained by this access.
    pub fn target(&self) -> SAWindowTarget {
        self.anchor().target
    }

    /// Whether SA has observed unexpected native destruction of this generation.
    /// This snapshot is not permission to invoke native APIs on another thread.
    pub fn is_native_alive(&self) -> bool {
        self.anchor().is_alive()
    }

    /// Borrows a native view on the creating thread while this lease remains alive.
    /// Copied raw integers confer no ownership or proof of renderer completion.
    pub fn native_ref(&self) -> Result<SANativeWindowRef<'_>, SAError> {
        let anchor = self.anchor();
        anchor.check_native_access()?;
        let window = anchor.window.as_deref().ok_or(SAError::Native {
            operation: SANativeOperation::NativeWindowAccess,
            message: String::from("simulated window has no native interoperability"),
        })?;
        Ok(SANativeWindowRef {
            access: self,
            window,
            owner_bound: PhantomData,
        })
    }
}

impl Clone for SAWindowAccess {
    fn clone(&self) -> Self {
        Self {
            anchor: Some(Arc::clone(self.anchor())),
        }
    }
}

impl fmt::Debug for SAWindowAccess {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SAWindowAccess")
            .field("target", &self.target())
            .field("native_alive", &self.is_native_alive())
            .finish_non_exhaustive()
    }
}

impl Drop for SAWindowAccess {
    fn drop(&mut self) {
        if let Some(anchor) = self.anchor.take() {
            let wake = Arc::clone(&anchor.wake);
            drop(anchor);
            // A durable owner wake follows the final lease decrement. Core must
            // keep its root through zero external leases and native destruction.
            wake();
        }
    }
}

/// A short-lived native view tied to a retained lease and its creating thread.
///
/// This view is neither Send nor Sync. Native APIs retain their own unsafe
/// obligations; SA does not certify GPU completion or allow HWND destruction.
pub struct SANativeWindowRef<'access> {
    access: &'access SAWindowAccess,
    window: &'access Window,
    owner_bound: PhantomData<Rc<()>>,
}

impl SANativeWindowRef<'_> {
    /// The exact retained generation from which these native handles originate.
    pub fn target(&self) -> SAWindowTarget {
        self.access.target()
    }

    /// Copies the Win32 HWND while retaining this checked borrowed view.
    /// The integer alone does not retain the native window.
    pub fn hwnd(&self) -> Result<NonZeroIsize, SAError> {
        self.win32_handle().map(|handle| handle.hwnd)
    }

    /// Copies the observed Win32 HINSTANCE, if supplied by the backend.
    pub fn hinstance(&self) -> Result<Option<NonZeroIsize>, SAError> {
        self.win32_handle().map(|handle| handle.hinstance)
    }

    fn win32_handle(&self) -> Result<winit::raw_window_handle::Win32WindowHandle, SAError> {
        let handle = self.window_handle().map_err(|error| SAError::Native {
            operation: SANativeOperation::NativeWindowAccess,
            message: error.to_string(),
        })?;
        match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(handle),
            _ => Err(SAError::Native {
                operation: SANativeOperation::NativeWindowAccess,
                message: String::from("native window is not a Win32 window"),
            }),
        }
    }
}

impl HasWindowHandle for SANativeWindowRef<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.access
            .anchor()
            .check_native_access()
            .map_err(|_| HandleError::Unavailable)?;
        self.window.window_handle()
    }
}

impl HasDisplayHandle for SANativeWindowRef<'_> {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.access
            .anchor()
            .check_native_access()
            .map_err(|_| HandleError::Unavailable)?;
        self.window.display_handle()
    }
}

pub(crate) struct WindowAnchor {
    target: SAWindowTarget,
    owner: ThreadId,
    window: Option<Arc<Window>>,
    accepting: AtomicBool,
    alive: AtomicBool,
    native_fault_retained: AtomicBool,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl WindowAnchor {
    pub(crate) fn new(
        target: SAWindowTarget,
        window: Arc<Window>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        Self::construct(target, Some(window), wake)
    }

    fn construct(
        target: SAWindowTarget,
        window: Option<Arc<Window>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            target,
            owner: thread::current().id(),
            window,
            accepting: AtomicBool::new(true),
            alive: AtomicBool::new(true),
            native_fault_retained: AtomicBool::new(false),
            wake,
        })
    }

    #[cfg(test)]
    pub(crate) fn simulated(
        target: SAWindowTarget,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        Self::construct(target, None, wake)
    }

    pub(crate) fn acquire(self: &Arc<Self>) -> Result<SAWindowAccess, SAError> {
        self.check_native_access()?;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(SAError::AdmissionClosed);
        }
        Ok(SAWindowAccess {
            anchor: Some(Arc::clone(self)),
        })
    }

    pub(crate) fn begin_retirement(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    pub(crate) fn external_count(self: &Arc<Self>) -> usize {
        // Exactly one permanent owner root is required. Once retiring and this
        // count is zero, no external lease remains from which a clone can arise.
        Arc::strong_count(self) - 1
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate_and_retain_native_root(&self) {
        self.accepting.store(false, Ordering::Release);
        self.alive.store(false, Ordering::Release);
        if !self.native_fault_retained.swap(true, Ordering::AcqRel)
            && let Some(window) = &self.window
        {
            // Winit Drop would repost destruction to an already dead/reused HWND.
            // Keep a permanent fault root so foreign last-lease Drop cannot do it.
            std::mem::forget(Arc::clone(window));
        }
        (self.wake)();
    }

    fn check_native_access(&self) -> Result<(), SAError> {
        if thread::current().id() != self.owner {
            return Err(SAError::WrongThread);
        }
        if !self.is_alive() {
            return Err(SAError::StaleIdentity);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/unit/window_lifetime.rs"]
mod tests;
