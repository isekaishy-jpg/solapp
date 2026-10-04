//! Typed failures and rejected ownership.

use std::error::Error;
use std::fmt;

use crate::context::SAContextPhase;

/// The identity space whose checked counter cannot advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAIdentityKind {
    /// Process-local host identity.
    Host,
    /// Host-local window slots or generations.
    Window,
    /// Owner-local recipient identities.
    Recipient,
    /// Owner-local subscription identities or ordering sequence.
    Subscription,
    /// Owner-local timer identities.
    Timer,
    /// Cross-thread post receipt identities.
    Post,
    /// Host-monotonic input receipt sequence.
    Input,
    /// Host-local text target incarnation.
    TextSession,
    /// Owner-local service registrations.
    Service,
    /// Host-local frame sequence.
    Frame,
    /// Host-local serialized display requests.
    DisplayTransition,
    /// Renderer presentation revisions, independent of native generations.
    Presentation,
}

/// The native operation which failed; diagnostics remain owned by SA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SANativeOperation {
    /// Constructing the one-shot native event loop.
    CreateEventLoop,
    /// Creating a native window.
    CreateWindow,
    /// Running the native event loop.
    RunEventLoop,
    /// Creating a prepared native cursor resource.
    CreateCursor,
    /// Changing the existing backend's raw input registration.
    RegisterRawInput,
    /// Changing native pointer confinement.
    ConfinePointer,
    /// Posting an owned wake hint to the existing native loop.
    Wake,
    /// Constructing the deadline-only modal wake helper.
    CreateWakeThread,
    /// Checked owner-thread native window interoperability.
    NativeWindowAccess,
    /// Applying serialized display settings through the existing backend.
    DisplayTransition,
}

/// A typed host failure with an optional owned diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SAError {
    /// A validated setting or operation input is invalid.
    InvalidInput(&'static str),
    /// The operation is unavailable in this callback phase.
    InvalidContext(SAContextPhase),
    /// Stop has closed ordinary admission.
    AdmissionClosed,
    /// The identity belongs to a different host.
    ForeignHost,
    /// The slot or expected native generation is no longer current.
    StaleIdentity,
    /// Checked identity allocation cannot issue another distinct token.
    IdentityExhausted(SAIdentityKind),
    /// SA storage reservation failed before accepting the input.
    AllocationFailed,
    /// A host operation was attempted away from its creating thread.
    WrongThread,
    /// The host's single run has already been consumed.
    AlreadyRun,
    /// The bounded post intake has no remaining accepted-work capacity.
    CapacityFull,
    /// Explicit callback nesting reached the configured limit.
    NestingLimit,
    /// Checked raw clock/deadline arithmetic overflowed.
    TimeOverflow,
    /// Association helper failure, preserving the native operation result.
    Shell(crate::SAShellFailure),
    /// An expected platform failure, retaining operation and diagnostic.
    Native {
        /// The operation which failed.
        operation: SANativeOperation,
        /// Owned backend diagnostic; not a status discriminator.
        message: String,
    },
    /// The application explicitly rejected startup.
    Application {
        /// Application-supplied owned diagnostic.
        message: String,
    },
    /// A startup callback panicked; retirement is still attempted.
    ApplicationPanicked {
        /// The callback phase which unwound.
        phase: SAContextPhase,
        /// Owned panic diagnostic when the panic payload is a string.
        message: String,
    },
}

impl SAError {
    /// Creates an application startup failure without borrowing its message.
    pub fn application(message: impl Into<String>) -> Self {
        Self::Application {
            message: message.into(),
        }
    }
}

impl fmt::Display for SAError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::InvalidContext(phase) => write!(f, "operation unavailable in {phase:?}"),
            Self::AdmissionClosed => f.write_str("ordinary admission is closed"),
            Self::ForeignHost => f.write_str("identity belongs to another host"),
            Self::StaleIdentity => f.write_str("identity or generation is stale"),
            Self::IdentityExhausted(kind) => write!(f, "{kind:?} identity space exhausted"),
            Self::AllocationFailed => f.write_str("host storage reservation failed"),
            Self::WrongThread => f.write_str("operation requires the creating thread"),
            Self::AlreadyRun => f.write_str("host run has already been consumed"),
            Self::CapacityFull => f.write_str("post admission capacity is full"),
            Self::NestingLimit => f.write_str("callback nesting limit reached"),
            Self::TimeOverflow => f.write_str("raw time arithmetic overflowed"),
            Self::Shell(error) => error.fmt(f),
            Self::Native { operation, message } => write!(f, "{operation:?} failed: {message}"),
            Self::Application { message } => write!(f, "application startup failed: {message}"),
            Self::ApplicationPanicked { phase, message } => {
                write!(f, "application panicked in {phase:?}: {message}")
            }
        }
    }
}

impl Error for SAError {}

/// Rejection preserves the complete supplied input on the calling thread.
/// No operation was accepted; dropping this value drops the input on that thread.
pub struct SARejected<T> {
    input: T,
    reason: SAError,
}

impl<T> SARejected<T> {
    pub(crate) fn new(input: T, reason: SAError) -> Self {
        Self { input, reason }
    }
    /// Inspects the failure without borrowing host state.
    pub fn reason(&self) -> &SAError {
        &self.reason
    }
    /// Inspects the original unaccepted input.
    pub fn input(&self) -> &T {
        &self.input
    }
    /// Recovers original input and typed failure without copying either.
    pub fn into_parts(self) -> (T, SAError) {
        (self.input, self.reason)
    }
}

impl<T> fmt::Debug for SARejected<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SARejected")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}
