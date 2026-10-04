//! Owned association-opening requests; no application callbacks run on helpers.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

mod context;

/// Host-tagged identity of one accepted association request. Cloning a receipt
/// preserves this identity; it retains neither a helper nor destination bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAShellRequestId {
    host: crate::SAHostId,
    serial: u64,
}

impl SAShellRequestId {
    /// Host that accepted the request.
    pub fn host(self) -> crate::SAHostId {
        self.host
    }
}

/// Application-selected destination. SA performs no percent decoding, shell
/// command construction or add-on policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SAShellDestination {
    /// An application-supplied URL, passed intact to Windows associations.
    Url(String),
    /// An absolute file or directory path, including native non-Unicode paths.
    File(PathBuf),
}

/// Owned request to invoke the Windows `open` association without arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SAShellRequest {
    /// The complete application-selected destination.
    pub destination: SAShellDestination,
}

impl SAShellRequest {
    pub(crate) fn wide(&self) -> Result<Vec<u16>, SAShellFailure> {
        let mut wide = Vec::new();
        match &self.destination {
            SAShellDestination::Url(url) => {
                if url.is_empty() || url.contains('\0') {
                    return Err(SAShellFailure::InvalidInput(
                        "URL must be nonempty and contain no NUL",
                    ));
                }
                // Validate only URI shape; allowed destinations/schemes are
                // application policy. Do not reinterpret a relative file name.
                let Some((scheme, _)) = url.split_once(':') else {
                    return Err(SAShellFailure::InvalidInput("URL needs a scheme"));
                };
                if scheme.is_empty()
                    || !scheme.as_bytes()[0].is_ascii_alphabetic()
                    || !scheme
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
                {
                    return Err(SAShellFailure::InvalidInput("invalid URL scheme"));
                }
                wide.try_reserve(
                    url.len()
                        .checked_add(1)
                        .ok_or(SAShellFailure::AllocationFailed)?,
                )
                .map_err(|_| SAShellFailure::AllocationFailed)?;
                wide.extend(url.encode_utf16());
            }
            SAShellDestination::File(path) => {
                if !path.is_absolute() {
                    return Err(SAShellFailure::InvalidInput("file path must be absolute"));
                }
                let count = path.as_os_str().encode_wide().count();
                wide.try_reserve(
                    count
                        .checked_add(1)
                        .ok_or(SAShellFailure::AllocationFailed)?,
                )
                .map_err(|_| SAShellFailure::AllocationFailed)?;
                wide.extend(path.as_os_str().encode_wide());
                if wide.contains(&0) {
                    return Err(SAShellFailure::InvalidInput("file path contains NUL"));
                }
            }
        }
        wide.push(0);
        Ok(wide)
    }
}

/// Operation failure, independent of whether a launched process later exits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SAShellFailure {
    /// Destination encoding or shape is invalid.
    InvalidInput(&'static str),
    /// Bounded accepted-work capacity is full.
    CapacityFull,
    /// Host/helper admission is closed.
    Closed,
    /// A synchronous owner operation was attempted on a different thread.
    WrongThread,
    /// The host cannot issue another nonreused request identity.
    IdentityExhausted,
    /// Request storage could not be reserved.
    AllocationFailed,
    /// Creating the dedicated helper failed before request acceptance.
    Spawn(String),
    /// A Windows operation failed with an owned HRESULT and diagnostic.
    Native {
        /// Operation which failed.
        operation: &'static str,
        /// Native HRESULT.
        code: i32,
        /// Owned diagnostic.
        message: String,
    },
    /// The helper could not finish its operation normally.
    HelperPanicked,
}

impl fmt::Display for SAShellFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(f, "invalid shell request: {reason}"),
            Self::CapacityFull => f.write_str("shell accepted-work capacity is full"),
            Self::Closed => f.write_str("shell admission is closed"),
            Self::WrongThread => f.write_str("shell operation requires the host owner thread"),
            Self::IdentityExhausted => f.write_str("shell request identities exhausted"),
            Self::AllocationFailed => f.write_str("shell storage reservation failed"),
            Self::Spawn(message) => write!(f, "shell helper creation failed: {message}"),
            Self::Native {
                operation,
                code,
                message,
            } => write!(f, "{operation} failed ({code:#x}): {message}"),
            Self::HelperPanicked => f.write_str("shell helper panicked"),
        }
    }
}
impl Error for SAShellFailure {}

/// Unaccepted shell request remains owned by its caller.
#[derive(Debug)]
pub struct SAShellRejected {
    /// Original, unconsumed request.
    pub request: SAShellRequest,
    /// Admission or validation failure.
    pub reason: SAShellFailure,
}

/// Durable state of an accepted association request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SAShellOutcome {
    /// Owned request is queued or executing.
    Pending,
    /// A queued request was cancelled before the helper claimed it.
    Cancelled,
    /// Native call and request cleanup finished. Success reports only native
    /// association acceptance, not external launch completion or process exit.
    Complete(Result<(), SAShellFailure>),
}

/// Transferable observation/cancellation handle; retaining it keeps no native
/// window, request bytes, application context or helper thread alive.
#[derive(Clone, Debug)]
pub struct SAShellReceipt {
    id: SAShellRequestId,
    state: Arc<Mutex<ReceiptState>>,
}
#[derive(Debug)]
struct ReceiptState {
    claimed: bool,
    cancel_requested: bool,
    outcome: SAShellOutcome,
}

impl SAShellReceipt {
    /// Stable identity of this accepted request, shared by all receipt clones.
    pub fn id(&self) -> SAShellRequestId {
        self.id
    }

    /// Reads durable state without borrowing a payload or application.
    pub fn outcome(&self) -> SAShellOutcome {
        lock(&self.state).outcome.clone()
    }

    /// Requests cancellation only while still queued. `true` means launch will
    /// not begin; terminal Cancelled follows helper reclamation. `false` means
    /// already claimed or terminal and cannot undo a native launch.
    pub fn cancel(&self) -> bool {
        let mut state = lock(&self.state);
        if state.claimed || state.outcome != SAShellOutcome::Pending {
            return false;
        }
        state.cancel_requested = true;
        true
    }
}

struct Pending {
    request: SAShellRequest,
    receipt: SAShellReceipt,
}
struct State {
    host: crate::SAHostId,
    serial: u64,
    open: bool,
    capacity: usize,
    accepted: usize,
    queue: VecDeque<Pending>,
    done: bool,
}
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}

/// Private helper controller. Owner polling supplies progress independently of
/// ordinary public post admission. No application callback runs on this thread.
pub(crate) struct ShellHelper {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ShellHelper {
    pub(crate) fn new(host: crate::SAHostId, capacity: usize) -> Result<Self, SAShellFailure> {
        Self::spawn(host, capacity, || {
            let apartment = crate::backend::windows::shell::Apartment::new();
            move |request: &SAShellRequest| match &apartment {
                Ok(apartment) => apartment.launch(request),
                Err(error) => Err(error.clone()),
            }
        })
    }

    fn spawn<Init, Execute>(
        host: crate::SAHostId,
        capacity: usize,
        init: Init,
    ) -> Result<Self, SAShellFailure>
    where
        Init: FnOnce() -> Execute + Send + 'static,
        Execute: FnMut(&SAShellRequest) -> Result<(), SAShellFailure>,
    {
        if capacity == 0 {
            return Err(SAShellFailure::InvalidInput(
                "shell capacity must be positive",
            ));
        }
        let mut queue = VecDeque::new();
        queue
            .try_reserve(capacity)
            .map_err(|_| SAShellFailure::AllocationFailed)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                host,
                serial: 0,
                open: true,
                capacity,
                accepted: 0,
                queue,
                done: false,
            }),
            ready: Condvar::new(),
        });
        let run = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(String::from("solapp-shell"))
            .spawn(move || {
                let mut execute = std::panic::catch_unwind(std::panic::AssertUnwindSafe(init));
                // Never drop an arbitrary panic payload while restoring obligations.
                if let Err(payload) = &mut execute {
                    let payload = std::mem::replace(payload, Box::new(()));
                    std::mem::forget(payload);
                }
                loop {
                    let pending = {
                        let mut state = lock(&run.state);
                        while state.queue.is_empty() && state.open {
                            state = run
                                .ready
                                .wait(state)
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                        match state.queue.pop_front() {
                            Some(pending) => pending,
                            None => break,
                        }
                    };
                    let cancelled = {
                        let mut state = lock(&pending.receipt.state);
                        state.claimed = true;
                        state.cancel_requested
                    };
                    let outcome = if cancelled {
                        SAShellOutcome::Cancelled
                    } else {
                        let result = match &mut execute {
                            Ok(execute) => {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    execute(&pending.request)
                                }))
                            }
                            Err(_) => Ok(Err(SAShellFailure::HelperPanicked)),
                        };
                        SAShellOutcome::Complete(match result {
                            Ok(result) => result,
                            Err(payload) => {
                                std::mem::forget(payload);
                                Err(SAShellFailure::HelperPanicked)
                            }
                        })
                    };
                    drop(pending.request);
                    lock(&pending.receipt.state).outcome = outcome;
                    lock(&run.state).accepted -= 1;
                }
                // Apartment cleanup must finish before final access is reported.
                drop(execute);
                lock(&run.state).done = true;
            })
            .map_err(|error| SAShellFailure::Spawn(error.to_string()))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub(crate) fn try_launch(
        &self,
        request: SAShellRequest,
    ) -> Result<SAShellReceipt, SAShellRejected> {
        if let Err(reason) = request.wide() {
            return Err(SAShellRejected { request, reason });
        }
        let mut state = lock(&self.shared.state);
        let reason = if !state.open {
            Some(SAShellFailure::Closed)
        } else if state.accepted >= state.capacity {
            Some(SAShellFailure::CapacityFull)
        } else {
            None
        };
        if let Some(reason) = reason {
            drop(state);
            return Err(SAShellRejected { request, reason });
        }
        let Some(serial) = state.serial.checked_add(1) else {
            return Err(SAShellRejected {
                request,
                reason: SAShellFailure::IdentityExhausted,
            });
        };
        let receipt = SAShellReceipt {
            id: SAShellRequestId {
                host: state.host,
                serial,
            },
            state: Arc::new(Mutex::new(ReceiptState {
                claimed: false,
                cancel_requested: false,
                outcome: SAShellOutcome::Pending,
            })),
        };
        state.serial = serial;
        state.queue.push_back(Pending {
            request,
            receipt: receipt.clone(),
        });
        state.accepted += 1;
        drop(state);
        self.shared.ready.notify_one();
        Ok(receipt)
    }

    pub(crate) fn close(&self) {
        lock(&self.shared.state).open = false;
        // Accepted requests settle normally unless their receipt was cancelled.
        self.shared.ready.notify_one();
    }

    pub(crate) fn pending(&self) -> usize {
        lock(&self.shared.state).accepted
    }

    pub(crate) fn poll_closed(&mut self) -> Result<bool, SAShellFailure> {
        if let Some(thread) = &self.thread {
            if !thread.is_finished() {
                return Ok(false);
            }
            let result = self.thread.take().unwrap().join();
            if let Err(payload) = result {
                std::mem::forget(payload);
                return Err(SAShellFailure::HelperPanicked);
            }
        }
        Ok(lock(&self.shared.state).done)
    }
}

impl Drop for ShellHelper {
    fn drop(&mut self) {
        self.close();
        // The owner must normally poll retirement. This fallback keeps accepted
        // owned requests alive through final native access, even on early Drop.
        if let Some(thread) = self.thread.take()
            && let Err(payload) = thread.join()
        {
            std::mem::forget(payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn request() -> SAShellRequest {
        SAShellRequest {
            destination: SAShellDestination::Url(String::from("https://example.invalid/a%20b")),
        }
    }

    fn retire(helper: &mut ShellHelper) {
        helper.close();
        let limit = Instant::now() + Duration::from_secs(3);
        while !helper.poll_closed().unwrap() {
            assert!(Instant::now() < limit, "shell test helper did not retire");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn encoding_preserves_destination_and_rejects_ambiguous_input() {
        let encoded = request().wide().unwrap();
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            "https://example.invalid/a%20b"
        );
        for url in ["", "relative/file", "1bad:route", "https://x\0y"] {
            assert!(
                SAShellRequest {
                    destination: SAShellDestination::Url(url.into())
                }
                .wide()
                .is_err()
            );
        }
        assert!(
            SAShellRequest {
                destination: SAShellDestination::File(PathBuf::from("relative.txt"))
            }
            .wide()
            .is_err()
        );
        use std::os::windows::ffi::OsStringExt;
        let native = std::ffi::OsString::from_wide(&[67, 58, 92, 0xd800]);
        assert_eq!(
            SAShellRequest {
                destination: SAShellDestination::File(native.into())
            }
            .wide()
            .unwrap(),
            [67, 58, 92, 0xd800, 0]
        );
    }

    #[test]
    fn bounded_intake_cancellation_and_close_retain_claimed_work() {
        let (started, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 2, move || {
            move |_: &SAShellRequest| {
                started.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(())
            }
        })
        .unwrap();
        let first = helper.try_launch(request()).unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!first.cancel());
        let queued = helper.try_launch(request()).unwrap();
        assert_ne!(first.id(), queued.id());
        assert_eq!(first.id(), first.clone().id());
        assert!(queued.cancel());
        assert_eq!(
            helper.try_launch(request()).unwrap_err().reason,
            SAShellFailure::CapacityFull
        );
        helper.close();
        assert_eq!(
            helper.try_launch(request()).unwrap_err().reason,
            SAShellFailure::Closed
        );
        assert_eq!(helper.pending(), 2);
        assert!(!helper.poll_closed().unwrap());
        release.send(()).unwrap();
        retire(&mut helper);
        assert_eq!(first.outcome(), SAShellOutcome::Complete(Ok(())));
        assert_eq!(queued.outcome(), SAShellOutcome::Cancelled);
        assert_eq!(helper.pending(), 0);
        assert!(
            observed.try_recv().is_err(),
            "cancelled request must not execute"
        );
    }

    #[test]
    fn helper_fault_settles_receipt_without_losing_retirement() {
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 1, || {
            |_: &SAShellRequest| -> Result<(), SAShellFailure> { panic!("injected executor fault") }
        })
        .unwrap();
        let receipt = helper.try_launch(request()).unwrap();
        retire(&mut helper);
        assert_eq!(
            receipt.outcome(),
            SAShellOutcome::Complete(Err(SAShellFailure::HelperPanicked))
        );
    }

    #[test]
    fn native_apartment_can_start_and_retire_without_launching() {
        thread::spawn(|| {
            drop(crate::backend::windows::shell::Apartment::new().unwrap());
        })
        .join()
        .unwrap();
        let mut helper = ShellHelper::new(crate::SAHostId::allocate().unwrap(), 1).unwrap();
        retire(&mut helper);
    }

    #[test]
    fn exhausted_identity_rejects_original_owned_request() {
        let host = crate::SAHostId::allocate().unwrap();
        let mut helper = ShellHelper::spawn(host, 1, || |_: &SAShellRequest| Ok(())).unwrap();
        lock(&helper.shared.state).serial = u64::MAX;
        let original = request();
        let rejected = helper.try_launch(original.clone()).unwrap_err();
        assert_eq!(rejected.request, original);
        assert_eq!(rejected.reason, SAShellFailure::IdentityExhausted);
        assert_eq!(helper.pending(), 0);
        retire(&mut helper);
    }

    #[test]
    fn apartment_startup_failure_settles_every_accepted_request() {
        let failure = SAShellFailure::Native {
            operation: "initialize shell apartment",
            code: 0x80010106_u32 as i32,
            message: "injected apartment failure".into(),
        };
        let expected = failure.clone();
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 2, move || {
            move |_: &SAShellRequest| Err(failure.clone())
        })
        .unwrap();
        let first = helper.try_launch(request()).unwrap();
        let second = helper.try_launch(request()).unwrap();
        retire(&mut helper);
        for receipt in [first, second] {
            assert_eq!(
                receipt.outcome(),
                SAShellOutcome::Complete(Err(expected.clone()))
            );
        }
        assert_eq!(helper.pending(), 0);
    }
}
