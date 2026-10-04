//! A deadline-only helper posts the existing winit wake during native modal loops.
//! It never touches application state or invokes service/frame/provider callbacks.

use crate::{SAError, SANativeOperation};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

struct State {
    deadline: Option<Instant>,
    closed: bool,
}
pub(crate) struct DeadlineWake {
    shared: Arc<(Mutex<State>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl DeadlineWake {
    pub(crate) fn new(wake: super::wake::NativeWake) -> Result<Self, SAError> {
        let shared = Arc::new((
            Mutex::new(State {
                deadline: None,
                closed: false,
            }),
            Condvar::new(),
        ));
        let owned = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name(String::from("solapp-deadline"))
            .spawn(move || {
                let (mutex, changed) = &*owned;
                let mut state = mutex
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                loop {
                    if state.closed {
                        break;
                    }
                    let Some(deadline) = state.deadline else {
                        state = changed
                            .wait(state)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        continue;
                    };
                    let now = Instant::now();
                    if now < deadline {
                        let (next, _) = changed
                            .wait_timeout(state, deadline.duration_since(now))
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state = next;
                        continue;
                    }
                    // Consume once while locked, then post outside the lock. The
                    // owner rechecks source state and schedules any continuation.
                    state.deadline = None;
                    drop(state);
                    let _ = wake.post();
                    state = mutex
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            })
            .map_err(|error| SAError::Native {
                operation: SANativeOperation::CreateWakeThread,
                message: error.to_string(),
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub(crate) fn arm(&self, deadline: Option<Instant>) {
        let mut state = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.deadline != deadline {
            state.deadline = deadline;
            self.shared.1.notify_one();
        }
    }
}
impl Drop for DeadlineWake {
    fn drop(&mut self) {
        {
            let mut state = self
                .shared
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.closed = true;
            self.shared.1.notify_one();
        }
        // This helper owns no external/provider work; closure wakes its only wait.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
