//! Serialize native wake posting with final event-loop-window access.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::{SAError, SANativeOperation};

struct State {
    closed: bool,
    proxy: Option<winit::event_loop::EventLoopProxy<()>>,
    #[cfg(test)]
    send: Option<Arc<dyn Fn() -> Result<(), SAError> + Send + Sync>>,
}

/// Every SA native wake destination shares this gate. Holding its private lock
/// through send covers winit's channel-enqueue THEN PostMessage interval. It
/// invokes no application/provider callback. Close waits for admitted sends.
#[derive(Clone)]
pub(crate) struct NativeWake {
    state: Arc<Mutex<State>>,
}

impl NativeWake {
    /// Construct before native acquisition. The no-proxy state supports the
    /// deterministic backend; no production application starts before install.
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                closed: false,
                proxy: None,
                #[cfg(test)]
                send: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Install exactly once before publishing native-capable destinations.
    pub(crate) fn install(
        &self,
        proxy: winit::event_loop::EventLoopProxy<()>,
    ) -> Result<(), SAError> {
        let mut state = self.lock();
        if state.closed {
            return Err(SAError::AdmissionClosed);
        }
        if state.proxy.is_some() {
            return Err(SAError::AlreadyRun);
        }
        state.proxy = Some(proxy);
        Ok(())
    }

    /// A durable source owns progress; this is only its native scheduling hint.
    pub(crate) fn post(&self) -> Result<(), SAError> {
        let state = self.lock();
        if state.closed {
            return Err(SAError::AdmissionClosed);
        }
        #[cfg(test)]
        if let Some(send) = &state.send {
            return send();
        }
        match &state.proxy {
            Some(proxy) => proxy.send_event(()).map_err(|error| SAError::Native {
                operation: SANativeOperation::Wake,
                message: error.to_string(),
            }),
            None => Ok(()),
        }
    }

    /// After return, no admitted send can still touch the backend wake HWND,
    /// and surviving clones cannot begin another native send.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.proxy = None;
    }

    /// Declare after acquiring the actual event-loop owner, and run the loop
    /// through a borrowed API. Reverse local destruction then closes this gate
    /// before that owner drops, including native errors and Rust unwinding.
    pub(crate) fn close_guard(&self) -> NativeWakeCloseGuard {
        NativeWakeCloseGuard(self.clone())
    }
}

pub(crate) struct NativeWakeCloseGuard(NativeWake);

impl Drop for NativeWakeCloseGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn close_waits_for_admitted_native_send_and_seals_surviving_clones() {
        let wake = NativeWake::new();
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let calls = Arc::new(AtomicUsize::new(0));
        let called = Arc::clone(&calls);
        wake.lock().send = Some(Arc::new(move || {
            called.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(())
        }));
        let sending = wake.clone();
        let sender = std::thread::spawn(move || sending.post());
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            wake.state.try_lock().is_err(),
            "send must retain the native final-access lock"
        );
        let closing = wake.clone();
        let (closed, completed) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            closing.close();
            closed.send(()).unwrap();
        });
        assert!(completed.try_recv().is_err());
        release.send(()).unwrap();
        sender.join().unwrap().unwrap();
        completed.recv_timeout(Duration::from_secs(3)).unwrap();
        closer.join().unwrap();
        assert_eq!(wake.post(), Err(SAError::AdmissionClosed));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        wake.close();
    }

    #[test]
    fn unwind_guard_closes_before_borrowed_native_owner_is_destroyed() {
        struct Owner {
            wake: NativeWake,
            destroyed: Arc<AtomicBool>,
        }
        impl Drop for Owner {
            fn drop(&mut self) {
                assert!(
                    self.wake.lock().closed,
                    "owner destruction must follow gate closure"
                );
                self.destroyed.store(true, Ordering::SeqCst);
            }
        }
        let wake = NativeWake::new();
        let destroyed = Arc::new(AtomicBool::new(false));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _owner = Owner {
                wake: wake.clone(),
                destroyed: Arc::clone(&destroyed),
            };
            let _guard = wake.close_guard();
            panic!("injected borrowed-loop panic");
        }));
        assert!(result.is_err());
        assert!(destroyed.load(Ordering::SeqCst));
        assert_eq!(wake.post(), Err(SAError::AdmissionClosed));
    }
}
