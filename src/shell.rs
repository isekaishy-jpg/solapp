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
    fn validate(&self) -> Result<(), SAShellFailure> {
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
            }
            SAShellDestination::File(path) => {
                if !path.is_absolute() {
                    return Err(SAShellFailure::InvalidInput("file path must be absolute"));
                }
                if path.as_os_str().encode_wide().any(|unit| unit == 0) {
                    return Err(SAShellFailure::InvalidInput("file path contains NUL"));
                }
            }
        }
        Ok(())
    }

    fn prepare(&self) -> Result<PreparedDestination, SAShellFailure> {
        self.validate()?;
        #[cfg(test)]
        PREPARATIONS.with(|count| count.set(count.get() + 1));
        #[cfg(test)]
        if FAIL_NEXT_PREPARATION.with(|fail| fail.replace(false)) {
            return Err(SAShellFailure::AllocationFailed);
        }
        let count = match &self.destination {
            SAShellDestination::Url(url) => url.encode_utf16().count(),
            SAShellDestination::File(path) => path.as_os_str().encode_wide().count(),
        };
        let mut wide = Vec::new();
        wide.try_reserve_exact(
            count
                .checked_add(1)
                .ok_or(SAShellFailure::AllocationFailed)?,
        )
        .map_err(|_| SAShellFailure::AllocationFailed)?;
        match &self.destination {
            SAShellDestination::Url(url) => wide.extend(url.encode_utf16()),
            SAShellDestination::File(path) => wide.extend(path.as_os_str().encode_wide()),
        }
        wide.push(0);
        Ok(PreparedDestination { wide })
    }
}

// Only validated preparation constructs this private terminated native buffer.
pub(crate) struct PreparedDestination {
    wide: Vec<u16>,
}
impl PreparedDestination {
    pub(crate) fn wide(&self) -> &[u16] {
        &self.wide
    }
}
struct PreparedRequest {
    request: SAShellRequest,
    destination: PreparedDestination,
}
impl PreparedRequest {
    fn reject(self, reason: SAShellFailure) -> SAShellRejected {
        // Dispose of private backing outside the transport lock and preserve
        // exactly the original public request.
        let Self {
            request,
            destination,
        } = self;
        drop(destination);
        SAShellRejected { request, reason }
    }
}
#[cfg(test)]
thread_local! {
    static PREPARATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FAIL_NEXT_PREPARATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
    request: PreparedRequest,
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
impl State {
    fn admission_failure(&self) -> Option<SAShellFailure> {
        if !self.open {
            Some(SAShellFailure::Closed)
        } else if self.accepted >= self.capacity {
            Some(SAShellFailure::CapacityFull)
        } else {
            None
        }
    }
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
            move |request: &PreparedRequest| match &apartment {
                Ok(apartment) => apartment.launch(&request.destination),
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
        Execute: FnMut(&PreparedRequest) -> Result<(), SAShellFailure>,
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
        if let Err(reason) = request.validate() {
            return Err(SAShellRejected { request, reason });
        }
        // This is advisory only: it publishes no receipt or reservation. Full
        // helpers reject before private encoding, so CapacityFull can precede
        // an incidental AllocationFailed that preparation once exposed.
        if let Some(reason) = lock(&self.shared.state).admission_failure() {
            return Err(SAShellRejected { request, reason });
        }
        let destination = match request.prepare() {
            Ok(destination) => destination,
            Err(reason) => return Err(SAShellRejected { request, reason }),
        };
        let prepared = PreparedRequest {
            request,
            destination,
        };
        self.admit(prepared)
    }

    fn admit(&self, prepared: PreparedRequest) -> Result<SAShellReceipt, SAShellRejected> {
        let mut state = lock(&self.shared.state);
        if let Some(reason) = state.admission_failure() {
            drop(state);
            return Err(prepared.reject(reason));
        }
        let Some(serial) = state.serial.checked_add(1) else {
            drop(state);
            return Err(prepared.reject(SAShellFailure::IdentityExhausted));
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
            request: prepared,
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
        let encoded = request().prepare().unwrap().wide;
        assert_eq!(
            String::from_utf16(&encoded[..encoded.len() - 1]).unwrap(),
            "https://example.invalid/a%20b"
        );
        for url in ["", "relative/file", "1bad:route", "https://x\0y"] {
            assert!(
                SAShellRequest {
                    destination: SAShellDestination::Url(url.into())
                }
                .prepare()
                .is_err()
            );
        }
        assert!(
            SAShellRequest {
                destination: SAShellDestination::File(PathBuf::from("relative.txt"))
            }
            .prepare()
            .is_err()
        );
        use std::os::windows::ffi::OsStringExt;
        let native = std::ffi::OsString::from_wide(&[67, 58, 92, 0xd800]);
        assert_eq!(
            SAShellRequest {
                destination: SAShellDestination::File(native.into())
            }
            .prepare()
            .unwrap()
            .wide,
            [67, 58, 92, 0xd800, 0]
        );
    }

    #[test]
    fn bounded_intake_cancellation_and_close_retain_claimed_work() {
        let (started, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 2, move || {
            move |_: &PreparedRequest| {
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
            |_: &PreparedRequest| -> Result<(), SAShellFailure> {
                panic!("injected executor fault")
            }
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
        let mut helper = ShellHelper::spawn(host, 1, || |_: &PreparedRequest| Ok(())).unwrap();
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
            move |_: &PreparedRequest| Err(failure.clone())
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

    #[test]
    fn validation_has_no_allocations_and_preparation_preserves_native_units() {
        use std::os::windows::ffi::OsStringExt;
        let cases = [
            SAShellRequest {
                destination: SAShellDestination::Url("custom+thing:é/漢/%20?q=🙂".into()),
            },
            SAShellRequest {
                destination: SAShellDestination::File(PathBuf::from(r"C:\absolute\é.txt")),
            },
            SAShellRequest {
                destination: SAShellDestination::File(
                    std::ffi::OsString::from_wide(&[67, 58, 92, 0xd800]).into(),
                ),
            },
        ];
        for request in cases {
            PREPARATIONS.with(|count| count.set(0));
            let (valid, counts) = crate::allocation_probe::measure(|| request.validate());
            assert_eq!(valid, Ok(()));
            assert_eq!(counts.allocations, 0);
            assert_eq!(counts.reallocations, 0);
            assert_eq!(PREPARATIONS.with(|count| count.get()), 0);
            let (prepared, counts) =
                crate::allocation_probe::measure(|| request.prepare().unwrap());
            let expected: Vec<_> = match &request.destination {
                SAShellDestination::Url(url) => url.encode_utf16().chain(Some(0)).collect(),
                SAShellDestination::File(path) => {
                    path.as_os_str().encode_wide().chain(Some(0)).collect()
                }
            };
            assert_eq!(prepared.wide(), expected);
            assert_eq!(counts.allocations, 1);
            assert_eq!(counts.reallocations, 0);
            assert_eq!(PREPARATIONS.with(|count| count.get()), 1);
            println!(
                "shell validation allocations=0 preparation allocations={} preparations=1 units={}",
                counts.allocations,
                expected.len()
            );
        }
        let invalid = SAShellRequest {
            destination: SAShellDestination::File(
                std::ffi::OsString::from_wide(&[67, 58, 92, 0xd800, 0]).into(),
            ),
        };
        let (result, counts) = crate::allocation_probe::measure(|| invalid.validate());
        assert_eq!(
            result,
            Err(SAShellFailure::InvalidInput("file path contains NUL"))
        );
        assert_eq!(counts.allocations, 0);
    }

    #[test]
    fn helper_encodes_accepted_requests_once_and_rejects_full_or_invalid_before_preparation() {
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 1, move || {
            move |prepared: &PreparedRequest| {
                assert_eq!(prepared.request, request());
                assert_eq!(
                    String::from_utf16(
                        &prepared.destination.wide()[..prepared.destination.wide().len() - 1]
                    )
                    .unwrap(),
                    "https://example.invalid/a%20b"
                );
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            }
        })
        .unwrap();
        let accepted = request();
        PREPARATIONS.with(|count| count.set(0));
        let (receipt, counts) =
            crate::allocation_probe::measure(|| helper.try_launch(accepted).unwrap());
        assert_eq!(PREPARATIONS.with(|count| count.get()), 1);
        assert_eq!(
            counts.allocations, 2,
            "one destination backing plus one receipt"
        );
        assert_eq!(counts.reallocations, 0);
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        println!(
            "shell accepted preparations=1 admission allocations={} (buffer+receipt)",
            counts.allocations
        );
        let original = request();
        PREPARATIONS.with(|count| count.set(0));
        FAIL_NEXT_PREPARATION.with(|fail| fail.set(true));
        let submitted = original.clone();
        let (rejected, counts) =
            crate::allocation_probe::measure(|| helper.try_launch(submitted).unwrap_err());
        assert_eq!(rejected.request, original);
        assert_eq!(rejected.reason, SAShellFailure::CapacityFull);
        assert_eq!(PREPARATIONS.with(|count| count.get()), 0);
        assert!(FAIL_NEXT_PREPARATION.with(|fail| fail.get()));
        assert_eq!(counts.allocations, 0);
        assert_eq!(counts.reallocations, 0);
        let invalid = SAShellRequest {
            destination: SAShellDestination::Url("relative/path".into()),
        };
        let submitted = invalid.clone();
        let (rejected, counts) =
            crate::allocation_probe::measure(|| helper.try_launch(submitted).unwrap_err());
        assert_eq!(counts.allocations, 0);
        assert_eq!(counts.reallocations, 0);
        assert_eq!(rejected.request, invalid);
        assert!(matches!(rejected.reason, SAShellFailure::InvalidInput(_)));
        assert_eq!(PREPARATIONS.with(|count| count.get()), 0);
        helper.close();
        let submitted = original.clone();
        let (rejected, counts) =
            crate::allocation_probe::measure(|| helper.try_launch(submitted).unwrap_err());
        assert_eq!(counts.allocations, 0);
        assert_eq!(counts.reallocations, 0);
        assert_eq!(rejected.request, original);
        assert_eq!(rejected.reason, SAShellFailure::Closed);
        assert_eq!(PREPARATIONS.with(|count| count.get()), 0);
        FAIL_NEXT_PREPARATION.with(|fail| fail.set(false));
        println!(
            "shell already-full/invalid/closed preparations=0 allocations=0 (CapacityFull precedes injected preparation failure)"
        );
        release.send(()).unwrap();
        retire(&mut helper);
        assert_eq!(receipt.outcome(), SAShellOutcome::Complete(Ok(())));
    }

    #[test]
    fn preparation_failure_returns_original_without_receipt_identity_or_queue_commit() {
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 1, || {
            |_: &PreparedRequest| Ok(())
        })
        .unwrap();
        let original = request();
        FAIL_NEXT_PREPARATION.with(|fail| fail.set(true));
        let rejected = helper.try_launch(original.clone()).unwrap_err();
        assert_eq!(rejected.request, original);
        assert_eq!(rejected.reason, SAShellFailure::AllocationFailed);
        let state = lock(&helper.shared.state);
        assert_eq!(state.serial, 0);
        assert_eq!(state.accepted, 0);
        assert!(state.queue.is_empty());
        drop(state);
        let receipt = helper.try_launch(original).unwrap();
        assert_eq!(receipt.id.serial, 1);
        retire(&mut helper);
        assert_eq!(receipt.outcome(), SAShellOutcome::Complete(Ok(())));
    }

    #[test]
    fn final_admission_rechecks_open_capacity_and_identity_after_outside_lock_preparation() {
        let original = request();
        for reason in [
            SAShellFailure::Closed,
            SAShellFailure::CapacityFull,
            SAShellFailure::IdentityExhausted,
        ] {
            // A closed helper is never reopened: its waiting thread may already
            // have observed closure, including after a spurious condvar wake.
            let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 1, || {
                |_: &PreparedRequest| Ok(())
            })
            .unwrap();
            assert_eq!(lock(&helper.shared.state).admission_failure(), None);
            let prepared = PreparedRequest {
                request: original.clone(),
                destination: original.prepare().unwrap(),
            };
            {
                let mut state = lock(&helper.shared.state);
                match reason {
                    SAShellFailure::Closed => state.open = false,
                    SAShellFailure::CapacityFull => state.accepted = state.capacity,
                    SAShellFailure::IdentityExhausted => state.serial = u64::MAX,
                    _ => unreachable!(),
                }
            }
            let (rejected, counts) =
                crate::allocation_probe::measure(|| helper.admit(prepared).unwrap_err());
            assert_eq!(rejected.request, original);
            assert_eq!(rejected.reason, reason);
            assert_eq!(counts.allocations, 0);
            assert_eq!(
                counts.deallocations, 1,
                "private encoded backing disposed, original request retained"
            );
            {
                let mut state = lock(&helper.shared.state);
                assert!(state.queue.is_empty());
                // Clear only the fixture's synthetic accepted count.
                state.accepted = 0;
            }
            retire(&mut helper);
        }
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 1, || {
            |_: &PreparedRequest| Ok(())
        })
        .unwrap();
        let receipt = helper.try_launch(original).unwrap();
        assert_eq!(receipt.id.serial, 1);
        retire(&mut helper);
        assert_eq!(receipt.outcome(), SAShellOutcome::Complete(Ok(())));
    }

    #[test]
    fn helper_init_panic_still_settles_accepted_owned_preparation_and_retires() {
        let mut helper = ShellHelper::spawn(crate::SAHostId::allocate().unwrap(), 1, || {
            panic!("injected helper init panic");
            #[allow(unreachable_code)]
            |_: &PreparedRequest| Ok(())
        })
        .unwrap();
        let receipt = helper.try_launch(request()).unwrap();
        retire(&mut helper);
        assert_eq!(
            receipt.outcome(),
            SAShellOutcome::Complete(Err(SAShellFailure::HelperPanicked))
        );
        assert_eq!(helper.pending(), 0);
    }
}
