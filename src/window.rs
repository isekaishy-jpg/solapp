//! Minimal native window startup contracts.

use crate::backend::NativeWindow;
use crate::error::SAError;
use crate::identity::SAWindowGeneration;

/// Size in physical pixels. Window creation validates both components.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAPhysicalSize {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
}

/// Owned startup input for a normal windowed window.
/// Display modes and retained rendering access are later capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SAWindowSpec {
    /// UTF-8 title. Embedded NUL is rejected before native mutation.
    pub title: String,
    /// Requested client size in physical pixels, before native adjustments.
    pub size: SAPhysicalSize,
    /// Whether the native window is initially visible.
    pub visible: bool,
}

impl Default for SAWindowSpec {
    fn default() -> Self {
        Self {
            title: String::from("Solapp"),
            size: SAPhysicalSize {
                width: 960,
                height: 540,
            },
            visible: true,
        }
    }
}

impl SAWindowSpec {
    pub(crate) fn validate(&self) -> Result<(), SAError> {
        if self.title.contains('\0') {
            return Err(SAError::InvalidInput("window title contains NUL"));
        }
        if self.size.width == 0
            || self.size.height == 0
            || self.size.width > i32::MAX as u32
            || self.size.height > i32::MAX as u32
        {
            return Err(SAError::InvalidInput(
                "window size is outside the native range",
            ));
        }
        Ok(())
    }
}

/// Host-owned native window lifecycle. This is not renderer readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAWindowState {
    /// A native window was acquired and is retained by the host.
    Live,
    /// Admission is closed; application final access may still need it.
    Retiring,
}

pub(crate) struct WindowRecord {
    pub(crate) generation: SAWindowGeneration,
    pub(crate) native: NativeWindow,
    pub(crate) access: std::sync::Arc<crate::window_access::WindowAnchor>,
    pub(crate) closing: bool,
    pub(crate) display: crate::display::DisplayState,
    pub(crate) input_mode: crate::SAInputModeState,
    pub(crate) confinement_uncertain: bool,
    pub(crate) cursor: crate::SACursorState,
}
