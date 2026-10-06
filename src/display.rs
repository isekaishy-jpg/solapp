//! Driver-derived display choices and serialized native/presentation transitions.
mod context;

use std::fmt;

use winit::monitor::{MonitorHandle, VideoModeHandle};
use winit::window::{Fullscreen, Window};

use crate::{SAError, SAHostId, SAIdentityKind, SAPhysicalSize, SAWindowTarget};

/// Position in physical virtual-desktop pixels; negative coordinates are valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAScreenPosition {
    /// Horizontal screen coordinate.
    pub x: i32,
    /// Vertical screen coordinate.
    pub y: i32,
}

/// A validated, positive finite native scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SAScaleFactor(f64);

impl SAScaleFactor {
    /// Validates a scale before exposing it as geometry.
    pub fn new(value: f64) -> Result<Self, SAError> {
        if !value.is_finite() || value <= 0.0 {
            return Err(SAError::InvalidInput("scale must be positive and finite"));
        }
        Ok(Self(value))
    }

    /// The observed physical-to-logical scale.
    pub fn get(self) -> f64 {
        self.0
    }
}

/// Saved windowed geometry, independent of a fullscreen presentation revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAWindowedPlacement {
    /// Physical outer-window position on the virtual desktop.
    pub position: SAScreenPosition,
    /// Requested physical client size, subject to native adjustments.
    pub size: SAPhysicalSize,
    /// Whether the windowed state should be maximized.
    pub maximized: bool,
}

impl SAWindowedPlacement {
    pub(crate) fn validate(self) -> Result<(), SAError> {
        if self.size.width == 0
            || self.size.height == 0
            || self.size.width > i32::MAX as u32
            || self.size.height > i32::MAX as u32
        {
            return Err(SAError::InvalidInput(
                "windowed client size outside native range",
            ));
        }
        Ok(())
    }

    /// Keeps this placement on a currently observed monitor. If its old monitor
    /// vanished, moves it inside the nearest snapshot without changing client size.
    /// Native decoration sizes and subsequent native adjustments remain backend-owned.
    pub fn adjusted_for_monitors(self, monitors: &[SAMonitorSnapshot]) -> Result<Self, SAError> {
        self.validate()?;
        let intersects = |monitor: &SAMonitorSnapshot| {
            let left = i64::from(self.position.x);
            let top = i64::from(self.position.y);
            let right = left + i64::from(self.size.width);
            let bottom = top + i64::from(self.size.height);
            left < i64::from(monitor.position.x) + i64::from(monitor.size.width)
                && top < i64::from(monitor.position.y) + i64::from(monitor.size.height)
                && right > i64::from(monitor.position.x)
                && bottom > i64::from(monitor.position.y)
        };
        if monitors.iter().any(intersects) {
            return Ok(self);
        }
        let nearest = monitors
            .iter()
            .filter(|monitor| monitor.size.width != 0 && monitor.size.height != 0)
            .min_by_key(|monitor| {
                let x = i128::from(self.position.x) - i128::from(monitor.position.x);
                let y = i128::from(self.position.y) - i128::from(monitor.position.y);
                x * x + y * y
            })
            .ok_or(SAError::StaleIdentity)?;
        let x = i64::from(nearest.position.x)
            + i64::from(nearest.size.width.saturating_sub(self.size.width));
        let y = i64::from(nearest.position.y)
            + i64::from(nearest.size.height.saturating_sub(self.size.height));
        Ok(Self {
            position: SAScreenPosition {
                x: self
                    .position
                    .x
                    .clamp(nearest.position.x, i32::try_from(x).unwrap_or(i32::MAX)),
                y: self
                    .position
                    .y
                    .clamp(nearest.position.y, i32::try_from(y).unwrap_or(i32::MAX)),
            },
            ..self
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
enum MonitorSelection {
    Native(MonitorHandle),
    #[cfg(test)]
    Simulated(u64),
}

impl MonitorSelection {
    fn native(&self) -> Result<&MonitorHandle, SAError> {
        match self {
            Self::Native(monitor) => Ok(monitor),
            #[cfg(test)]
            Self::Simulated(_) => Err(SAError::StaleIdentity),
        }
    }
}

/// Host-scoped opaque monitor selection issued by the native driver.
/// Monitor identity may disappear; every apply revalidates its current presence.
#[derive(Clone, Eq, PartialEq)]
pub struct SAMonitorId {
    host: SAHostId,
    selection: MonitorSelection,
}

impl fmt::Debug for SAMonitorId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SAMonitorId")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// Opaque driver-derived video mode; callers cannot manufacture native modes.
#[derive(Clone, Eq, PartialEq)]
pub struct SAVideoMode {
    monitor: SAMonitorId,
    native: Option<VideoModeHandle>,
    size: SAPhysicalSize,
    bit_depth: u16,
    refresh_millihertz: u32,
}

impl SAVideoMode {
    /// The monitor from which this exact mode was issued.
    pub fn monitor(&self) -> &SAMonitorId {
        &self.monitor
    }

    /// Driver-reported physical pixel dimensions.
    pub fn size(&self) -> SAPhysicalSize {
        self.size
    }

    /// Driver-reported pixel bit depth.
    pub fn bit_depth(&self) -> u16 {
        self.bit_depth
    }

    /// Driver-reported refresh rate in millihertz, without rounding to integer Hz.
    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.refresh_millihertz
    }
}

impl fmt::Debug for SAVideoMode {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SAVideoMode")
            .field("monitor", &self.monitor)
            .field("size", &self.size)
            .field("bit_depth", &self.bit_depth)
            .field("refresh_millihertz", &self.refresh_millihertz)
            .finish_non_exhaustive()
    }
}

/// One native monitor snapshot. Its choices must be revalidated when applied.
#[derive(Clone, Debug, PartialEq)]
pub struct SAMonitorSnapshot {
    /// Opaque current monitor choice.
    pub id: SAMonitorId,
    /// Optional native display name.
    pub name: Option<String>,
    /// Physical virtual-desktop origin.
    pub position: SAScreenPosition,
    /// Observed physical monitor size.
    pub size: SAPhysicalSize,
    /// Validated native DPI scale.
    pub scale: SAScaleFactor,
    /// Current driver-derived exclusive choices.
    pub video_modes: Vec<SAVideoMode>,
}

/// An admitted mode request; Keep/Revert and settings persistence remain caller-owned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SADisplayRequest {
    /// Return to explicit placement, or the saved pre-fullscreen placement.
    Windowed {
        /// None requests restoration of the saved native windowed geometry.
        placement: Option<SAWindowedPlacement>,
    },
    /// Borderless fullscreen on an explicitly selected current monitor.
    Borderless {
        /// Driver-derived monitor choice.
        monitor: SAMonitorId,
    },
    /// Exclusive fullscreen using an explicitly selected driver mode.
    /// Winit's accepted exceptional exclusive failure may panic.
    Exclusive {
        /// Driver-derived mode, revalidated immediately before native mutation.
        mode: SAVideoMode,
    },
}

/// Mode category reported by the existing native backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SADisplayMode {
    /// Native windowed state.
    Windowed,
    /// Backend borderless fullscreen state.
    Borderless,
    /// Backend exclusive fullscreen state.
    Exclusive,
}

impl SADisplayRequest {
    /// The requested mode category; this does not certify native application.
    pub fn mode(&self) -> SADisplayMode {
        match self {
            Self::Windowed { .. } => SADisplayMode::Windowed,
            Self::Borderless { .. } => SADisplayMode::Borderless,
            Self::Exclusive { .. } => SADisplayMode::Exclusive,
        }
    }

    fn validate(&self, host: SAHostId) -> Result<(), SAError> {
        match self {
            Self::Windowed {
                placement: Some(placement),
            } => placement.validate(),
            Self::Windowed { placement: None } => Ok(()),
            Self::Borderless { monitor } if monitor.host == host => Ok(()),
            Self::Exclusive { mode } if mode.monitor.host == host => Ok(()),
            _ => Err(SAError::ForeignHost),
        }
    }

    pub(crate) fn validate_choices(
        &self,
        host: SAHostId,
        monitors: &[SAMonitorSnapshot],
    ) -> Result<(), SAError> {
        self.validate(host)?;
        match self {
            Self::Windowed { .. } => Ok(()),
            Self::Borderless { monitor } => monitors
                .iter()
                .any(|current| current.id == *monitor)
                .then_some(())
                .ok_or(SAError::StaleIdentity),
            Self::Exclusive { mode } => monitors
                .iter()
                .find(|current| current.id == mode.monitor)
                .and_then(|current| current.video_modes.iter().find(|current| *current == mode))
                .map(|_| ())
                .ok_or(SAError::StaleIdentity),
        }
    }

    pub(crate) fn native_fullscreen(&self, window: &Window) -> Result<Option<Fullscreen>, SAError> {
        match self {
            Self::Windowed { .. } => Ok(None),
            Self::Borderless { monitor } => {
                let selected = monitor.selection.native()?;
                let current = window
                    .available_monitors()
                    .find(|monitor| monitor == selected)
                    .ok_or(SAError::StaleIdentity)?;
                Ok(Some(Fullscreen::Borderless(Some(current))))
            }
            Self::Exclusive { mode } => {
                let selected = mode.monitor.selection.native()?;
                let monitor = window
                    .available_monitors()
                    .find(|monitor| monitor == selected)
                    .ok_or(SAError::StaleIdentity)?;
                let selected_mode = mode.native.as_ref().ok_or(SAError::StaleIdentity)?;
                let current = monitor
                    .video_modes()
                    .find(|mode| mode == selected_mode)
                    .ok_or(SAError::StaleIdentity)?;
                Ok(Some(Fullscreen::Exclusive(current)))
            }
        }
    }
}

/// Queried native geometry, including minimized zero-size client areas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SADisplayGeometry {
    /// Physical outer position, absent if the native query was unavailable.
    pub position: Option<SAScreenPosition>,
    /// Observed physical client size; zero is valid while minimized.
    pub size: SAPhysicalSize,
    /// Validated observed native scale.
    pub scale: SAScaleFactor,
    /// Backend-observed minimized state, if available.
    pub minimized: Option<bool>,
    /// Backend-observed maximized state.
    pub maximized: bool,
}

/// Backend-observed state, separate from requested settings and SR readiness.
/// `backend_mode` comes from winit's stored fullscreen state. It does not
/// independently certify hardware scanout, exclusive acceptance, or GPU completion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SADisplayObserved {
    /// Existing backend's reported fullscreen category.
    pub backend_mode: SADisplayMode,
    /// Independently queried native geometry/DPI snapshot.
    pub geometry: SADisplayGeometry,
}

impl SADisplayObserved {
    pub(crate) fn query(window: &Window) -> Result<Self, SAError> {
        let size = window.inner_size();
        let position = window
            .outer_position()
            .ok()
            .map(|position| SAScreenPosition {
                x: position.x,
                y: position.y,
            });
        Ok(Self {
            backend_mode: match window.fullscreen() {
                None => SADisplayMode::Windowed,
                Some(Fullscreen::Borderless(_)) => SADisplayMode::Borderless,
                Some(Fullscreen::Exclusive(_)) => SADisplayMode::Exclusive,
            },
            geometry: SADisplayGeometry {
                position,
                size: SAPhysicalSize {
                    width: size.width,
                    height: size.height,
                },
                scale: SAScaleFactor::new(window.scale_factor())?,
                minimized: window.is_minimized(),
                maximized: window.is_maximized(),
            },
        })
    }

    fn windowed_placement(self) -> Option<SAWindowedPlacement> {
        let placement = SAWindowedPlacement {
            position: self.geometry.position?,
            size: self.geometry.size,
            maximized: self.geometry.maximized,
        };
        placement.validate().ok()?;
        Some(placement)
    }
}

pub(crate) fn monitor_snapshots(
    host: SAHostId,
    window: &Window,
) -> Result<Vec<SAMonitorSnapshot>, SAError> {
    let mut monitors = Vec::new();
    for native in window.available_monitors() {
        monitors
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        let id = SAMonitorId {
            host,
            selection: MonitorSelection::Native(native.clone()),
        };
        let mut modes = Vec::new();
        for mode in native.video_modes() {
            modes
                .try_reserve(1)
                .map_err(|_| SAError::AllocationFailed)?;
            let size = mode.size();
            modes.push(SAVideoMode {
                monitor: id.clone(),
                size: SAPhysicalSize {
                    width: size.width,
                    height: size.height,
                },
                bit_depth: mode.bit_depth(),
                refresh_millihertz: mode.refresh_rate_millihertz(),
                native: Some(mode),
            });
        }
        let position = native.position();
        let size = native.size();
        monitors.push(SAMonitorSnapshot {
            id,
            name: native.name(),
            position: SAScreenPosition {
                x: position.x,
                y: position.y,
            },
            size: SAPhysicalSize {
                width: size.width,
                height: size.height,
            },
            scale: SAScaleFactor::new(native.scale_factor())?,
            video_modes: modes,
        });
    }
    Ok(monitors)
}

#[cfg(test)]
pub(crate) fn simulated_monitors(host: SAHostId) -> Vec<SAMonitorSnapshot> {
    let id = SAMonitorId {
        host,
        selection: MonitorSelection::Simulated(1),
    };
    vec![SAMonitorSnapshot {
        id: id.clone(),
        name: Some(String::from("simulated native display")),
        position: SAScreenPosition { x: 0, y: 0 },
        size: SAPhysicalSize {
            width: 1920,
            height: 1080,
        },
        scale: SAScaleFactor(1.0),
        video_modes: vec![SAVideoMode {
            monitor: id,
            native: None,
            size: SAPhysicalSize {
                width: 1920,
                height: 1080,
            },
            bit_depth: 32,
            refresh_millihertz: 59940,
        }],
    }]
}

/// Exact target and serial of one admitted display transition.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SADisplayTransitionId {
    target: SAWindowTarget,
    serial: u64,
}

impl SADisplayTransitionId {
    /// The exact native generation to which this transition belongs.
    pub fn target(self) -> SAWindowTarget {
        self.target
    }
}

/// Renderer revision, independent of native generation and transition identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAPresentationRevision(u64);

/// Admission receipt; neither native application nor rendering readiness is implied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SADisplayReceipt {
    /// Exact admitted transition.
    pub transition: SADisplayTransitionId,
    /// Exact renderer revision required by the acknowledgment.
    pub presentation_revision: SAPresentationRevision,
}

/// Progress of an admitted transition. Readiness is not user confirmation or persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SADisplayTransitionState {
    /// Pending native application; this request alone may be superseded.
    Requested,
    /// Native application has started and current obligations must settle.
    Applying,
    /// Backend application returned; the exact SR revision awaits acknowledgment.
    AwaitingPresentation,
    /// Backend mode was observed and SR/application reported exact revision ready.
    /// This acknowledgment is an external contract, not SA's proof of GPU completion.
    Ready,
    /// Typed native or renderer failure, retaining its diagnostic.
    Failed(SAError),
    /// Replaced or retired before application began.
    Superseded,
}

/// Owned transition status suitable for owner delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SADisplayTransition {
    /// Exact transition and renderer revision.
    pub receipt: SADisplayReceipt,
    /// Owned application-supplied requested settings.
    pub request: SADisplayRequest,
    /// Current transition progress.
    pub state: SADisplayTransitionState,
}

/// Application/SR report for an exact renderer revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SAPresentationStatus {
    /// SR has made this revision ready while retaining actual required window access.
    Ready,
    /// SR failed this revision; application owns any revert policy.
    Failed(SAError),
}

pub(crate) struct DisplayAdmission {
    pub(crate) receipt: SADisplayReceipt,
    pub(crate) superseded: Option<SADisplayTransition>,
}

pub(crate) struct DisplayApply {
    pub(crate) transition: SADisplayTransition,
    // Implicit maximized restoration must use winit's saved WINDOWPLACEMENT;
    // observed maximized geometry is not the hidden normal restore rectangle.
    pub(crate) windowed_placement: Option<SAWindowedPlacement>,
}

pub(crate) struct DisplayState {
    target: SAWindowTarget,
    next_serial: u64,
    next_revision: u64,
    accepting: bool,
    observed: SADisplayObserved,
    saved_windowed: Option<SAWindowedPlacement>,
    pending: Option<SADisplayTransition>,
    active: Option<SADisplayTransition>,
    settled: Option<SADisplayTransition>,
}

impl DisplayState {
    pub(crate) fn new(target: SAWindowTarget, observed: SADisplayObserved) -> Self {
        Self {
            target,
            next_serial: 1,
            next_revision: 1,
            accepting: true,
            observed,
            saved_windowed: None,
            pending: None,
            active: None,
            settled: None,
        }
    }

    pub(crate) fn request(
        &mut self,
        request: SADisplayRequest,
    ) -> Result<DisplayAdmission, SAError> {
        if !self.accepting {
            return Err(SAError::AdmissionClosed);
        }
        request.validate(self.target.id.host())?;
        let next_serial = self
            .next_serial
            .checked_add(1)
            .ok_or(SAError::IdentityExhausted(
                SAIdentityKind::DisplayTransition,
            ))?;
        let next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(SAError::IdentityExhausted(SAIdentityKind::Presentation))?;
        let receipt = SADisplayReceipt {
            transition: SADisplayTransitionId {
                target: self.target,
                serial: self.next_serial,
            },
            presentation_revision: SAPresentationRevision(self.next_revision),
        };
        let mut superseded = self.pending.replace(SADisplayTransition {
            receipt,
            request,
            state: SADisplayTransitionState::Requested,
        });
        if let Some(previous) = &mut superseded {
            previous.state = SADisplayTransitionState::Superseded;
        }
        self.next_serial = next_serial;
        self.next_revision = next_revision;
        Ok(DisplayAdmission {
            receipt,
            superseded,
        })
    }

    pub(crate) fn start_next(&mut self) -> Option<DisplayApply> {
        if self.active.is_some() {
            return None;
        }
        let mut transition = self.pending.take()?;
        if self.observed.backend_mode == SADisplayMode::Windowed
            && transition.request.mode() != SADisplayMode::Windowed
        {
            self.saved_windowed = self.observed.windowed_placement();
        }
        let windowed_placement = match &transition.request {
            SADisplayRequest::Windowed { placement } => {
                placement.or(self.saved_windowed.filter(|saved| !saved.maximized))
            }
            _ => None,
        };
        transition.state = SADisplayTransitionState::Applying;
        self.active = Some(transition.clone());
        Some(DisplayApply {
            transition,
            windowed_placement,
        })
    }

    pub(crate) fn applied(
        &mut self,
        receipt: SADisplayReceipt,
        observed: SADisplayObserved,
    ) -> Result<(), SAError> {
        let active = self
            .active
            .as_mut()
            .filter(|transition| {
                transition.receipt == receipt
                    && transition.state == SADisplayTransitionState::Applying
            })
            .ok_or(SAError::StaleIdentity)?;
        self.observed = observed;
        active.state = SADisplayTransitionState::AwaitingPresentation;
        if active.request.mode() == SADisplayMode::Windowed
            && observed.backend_mode == SADisplayMode::Windowed
        {
            self.saved_windowed = None;
        }
        Ok(())
    }

    pub(crate) fn fail(
        &mut self,
        receipt: SADisplayReceipt,
        reason: SAError,
    ) -> Result<SADisplayTransition, SAError> {
        if !self
            .active
            .as_ref()
            .is_some_and(|transition| transition.receipt == receipt)
        {
            return Err(SAError::StaleIdentity);
        }
        let mut transition = self.active.take().unwrap();
        transition.state = SADisplayTransitionState::Failed(reason);
        self.settled = Some(transition.clone());
        Ok(transition)
    }

    pub(crate) fn report(
        &mut self,
        receipt: SADisplayReceipt,
        status: SAPresentationStatus,
    ) -> Result<SADisplayTransition, SAError> {
        let active = self
            .active
            .as_ref()
            .filter(|transition| {
                transition.receipt == receipt
                    && transition.state == SADisplayTransitionState::AwaitingPresentation
            })
            .ok_or(SAError::StaleIdentity)?;
        if matches!(status, SAPresentationStatus::Ready)
            && self.observed.backend_mode != active.request.mode()
        {
            return Err(SAError::InvalidInput(
                "backend mode has not matched this request",
            ));
        }
        let mut transition = self.active.take().unwrap();
        transition.state = match status {
            SAPresentationStatus::Ready => SADisplayTransitionState::Ready,
            SAPresentationStatus::Failed(reason) => SADisplayTransitionState::Failed(reason),
        };
        self.settled = Some(transition.clone());
        Ok(transition)
    }

    pub(crate) fn observe(&mut self, observed: SADisplayObserved) {
        self.observed = observed;
    }
    pub(crate) fn observed(&self) -> SADisplayObserved {
        self.observed
    }
    pub(crate) fn pending(&self) -> Option<&SADisplayTransition> {
        self.pending.as_ref()
    }
    pub(crate) fn active(&self) -> Option<&SADisplayTransition> {
        self.active.as_ref()
    }
    pub(crate) fn settled(&self) -> Option<&SADisplayTransition> {
        self.settled.as_ref()
    }

    pub(crate) fn begin_retirement(&mut self) -> Option<SADisplayTransition> {
        self.accepting = false;
        let mut pending = self.pending.take()?;
        pending.state = SADisplayTransitionState::Superseded;
        Some(pending)
    }
}

#[cfg(test)]
#[path = "../tests/unit/display_transitions.rs"]
mod tests;
