//! An owner-thread Windows x64 application host.
//!
//! The host borrows application state for one run; applications may contain
//! non-`Send` values and externally scoped cleanup contexts. Startup means the
//! application callback ran, not that a renderer or resource domain is ready.
//! Stop closes ordinary admission and retains windows until the application
//! reports its host-dependent obligations settled.
//!
//! Owner-local dispatch, bounded owned posts and checked raw-deadline timers
//! are supported. Native keyboard, mouse and session-stamped text pass through
//! the growing input gate. Prepared native cursors and independent input modes
//! are supported. Source-specific service continuations, owned wake hints,
//! checked application time and explicitly selected frame pacing are supported.
//! Retained native access and serialized display changes support direct renderer
//! composition; renderer completion remains the application's contract. Topology,
//! explicit worker planning and shell association services are available.
//! Shutdown observations distinguish pending obligations without certifying
//! provider completion. The private winit backend owns the native event loop.
//! A second event loop in the same process is not promised.
#![deny(unsafe_code)]
#![deny(missing_docs)]

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("solapp currently supports Windows x64 only");

mod backend;
mod context;
mod cursor;
mod dispatch;
mod display;
mod error;
mod host;
mod identity;
mod input;
mod input_mode;
mod pacing;
mod post;
mod service;
mod shell;
mod shutdown;
mod text;
mod time;
mod timer;
mod topology;
mod window;
mod window_access;
#[cfg(feature = "solworker")]
mod worker;

pub use context::{SAAdmissionState, SAContext, SAContextPhase};
pub use cursor::{SACursor, SACursorSelection, SAPreparedCursor, SASystemCursor};
pub use dispatch::{
    SADispatchReport, SAEvent, SAEventFilter, SAHandler, SAPriority, SAPropagation,
    SARecipientState,
};
pub use display::{
    SADisplayGeometry, SADisplayMode, SADisplayObserved, SADisplayReceipt, SADisplayRequest,
    SADisplayTransition, SADisplayTransitionId, SADisplayTransitionState, SAMonitorId,
    SAMonitorSnapshot, SAPresentationRevision, SAPresentationStatus, SAScaleFactor,
    SAScreenPosition, SAVideoMode, SAWindowedPlacement,
};
pub use error::{SAError, SAIdentityKind, SANativeOperation, SARejected};
pub use host::{
    SAApplication, SAExitReport, SAHost, SAHostConfig, SAHostState, SAStopOutcome, SAStopProgress,
    SAStopReason,
};
pub use identity::{SAHostId, SAWindowGeneration, SAWindowId, SAWindowTarget};
pub use identity::{SAPostId, SARecipientGeneration, SARecipientId, SASubscriptionId, SATimerId};
pub use input::{
    SAButtonState, SAInputDeviceId, SAInputDrainReport, SAInputEvent, SAInputOrigin, SAInputRecord,
    SAInputStamp, SAInputState, SAInputStateLayer, SAKeyLocation, SALogicalKey, SAModifiers,
    SAMouseButton, SANamedKey, SAPhysicalKey, SAPhysicalPosition, SAScrollDelta,
};
pub use input_mode::{
    SACursorState, SAInputMode, SAInputModeState, SARawInputPolicy, SARawInputState,
};
pub use pacing::{SAFrame, SAFramePhase, SAPacingPolicy, SAPacingState};
pub use post::{SAPostDiscardReason, SAPostOutcome, SAPostReceipt, SAProxy};
pub use service::{
    SAServiceBudget, SAServiceId, SAServicePoint, SAServicePoints, SAServiceReport,
    SAServiceRequest, SAServiceSpec, SAServiceState, SAServiceVisit, SAWake,
};
pub use shell::{
    SAShellDestination, SAShellFailure, SAShellOutcome, SAShellReceipt, SAShellRejected,
    SAShellRequest, SAShellRequestId,
};
pub use shutdown::SAShutdownSnapshot;
pub use text::{SATextCaret, SATextSessionId};
pub use time::{SAApplicationTime, SAClockSnapshot, SARawDeadline, SARawTime};
pub use timer::{SATimerCancel, SATimerPumpReport};
pub use topology::{SACoreAffinity, SACpuCore, SACpuTopology, SATopologyError};
pub use window::{SAPhysicalSize, SAWindowSpec, SAWindowState};
pub use window_access::{SANativeWindowRef, SAWindowAccess};
#[cfg(feature = "solworker")]
pub use worker::{SAWorkerPlan, SAWorkerRequest};

#[cfg(test)]
#[path = "../examples/service_composition.rs"]
mod composition_tests;
#[cfg(test)]
#[path = "../tests/unit/dispatch.rs"]
mod dispatch_tests;
#[cfg(test)]
#[path = "../tests/unit/fatal_retirement.rs"]
mod fatal_retirement_tests;
#[cfg(test)]
#[path = "../tests/unit/foundation.rs"]
mod foundation_tests;
#[cfg(test)]
#[path = "../tests/unit/input.rs"]
mod input_tests;
#[cfg(test)]
#[path = "../tests/unit/lifecycle.rs"]
mod lifecycle_tests;
#[cfg(test)]
#[path = "../tests/unit/native_integration.rs"]
mod native_integration_tests;
#[cfg(test)]
#[path = "../tests/unit/pacing.rs"]
mod pacing_tests;
#[cfg(test)]
#[path = "../tests/unit/posts.rs"]
mod post_tests;
#[cfg(test)]
#[path = "../tests/unit/services.rs"]
mod service_tests;
#[cfg(test)]
#[path = "../tests/unit/timers.rs"]
mod timer_tests;
#[cfg(test)]
#[path = "../tests/unit/window_driver.rs"]
mod window_driver_tests;

#[cfg(test)]
#[path = "../tests/unit/shutdown.rs"]
mod shutdown_tests;
#[cfg(test)]
#[path = "../tests/unit/workloads.rs"]
mod workload_tests;
