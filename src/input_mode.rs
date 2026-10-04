//! Requested input policy and last confirmed native outcomes.

use crate::{SACursorSelection, SAError};

/// Independent relative-motion delivery and pointer-confinement requests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SAInputMode {
    /// Select this window as the raw-motion recipient while platform-focused.
    pub relative_motion: bool,
    /// Confine the pointer to the native client area when supported.
    pub confine_pointer: bool,
}

/// Requested mode and individually confirmed native state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SAInputModeState {
    /// Latest request, including a request that failed.
    pub requested: SAInputMode,
    /// Last successful operations; relative delivery also requires platform focus.
    pub confirmed: SAInputMode,
    /// Last native mode failure, cleared by a completely successful request.
    pub failure: Option<SAError>,
}

/// Policy for the existing backend's single raw-device registrar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SARawInputPolicy {
    /// Remove raw registration.
    Never,
    /// Register native devices for delivery while this application is focused.
    WhenFocused,
}

/// Global raw registration request versus the last confirmed native call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SARawInputState {
    /// Latest application policy request. Retirement can confirm `Never`
    /// without overwriting this request history.
    pub requested: SARawInputPolicy,
    /// Last confirmed native policy; absent before SA's first native call.
    pub confirmed: Option<SARawInputPolicy>,
    /// Diagnostic from the latest failed registration attempt.
    pub failure: Option<SAError>,
}
impl Default for SARawInputState {
    fn default() -> Self {
        Self {
            requested: SARawInputPolicy::Never,
            confirmed: None,
            failure: None,
        }
    }
}

/// Retained cursor selection and independent visibility policy.
#[derive(Clone, Debug)]
pub struct SACursorState {
    /// Window-local selected resource, retained even when the pointer is elsewhere.
    pub selection: SACursorSelection,
    /// Application visibility request, independent of relative-motion suppression.
    pub requested_visible: bool,
    /// Relative-motion policy currently suppresses the cursor.
    pub input_suppressed: bool,
}
impl Default for SACursorState {
    fn default() -> Self {
        Self {
            selection: SACursorSelection::Default,
            requested_visible: true,
            input_suppressed: false,
        }
    }
}
