//! Host admission and final access for association opening.

use super::{SAShellFailure, SAShellReceipt, SAShellRejected, SAShellRequest, ShellHelper};
use crate::host::{Core, SAApplication, SAStopReason};
use crate::{SAContext, SAError};

impl<A: SAApplication> SAContext<'_, A> {
    /// Opens an application-selected association synchronously on the owner.
    ///
    /// This may block in Windows or an installed association handler and may
    /// enter native modal processing. It does not pump nested application work.
    /// Use `request_shell` when the owner must remain responsive. Success means
    /// native association acceptance, not external process start or exit.
    pub fn open_shell(&mut self, request: &SAShellRequest) -> Result<(), SAShellFailure> {
        self.core
            .check_owner()
            .map_err(|_| SAShellFailure::WrongThread)?;
        if !self.core.ordinary_open() {
            return Err(SAShellFailure::Closed);
        }
        let destination = request.prepare()?;
        crate::backend::windows::shell::Apartment::new()?.launch(&destination)
    }

    /// Accepts an owned association request on the bounded dedicated helper.
    /// Rejection returns the original request; an accepted receipt survives
    /// host closure without retaining the application or a native window.
    pub fn request_shell(
        &mut self,
        request: SAShellRequest,
    ) -> Result<SAShellReceipt, SAShellRejected> {
        let admission = self
            .core
            .check_owner()
            .map_err(|_| SAShellFailure::WrongThread)
            .and_then(|()| {
                if self.core.ordinary_open() {
                    Ok(())
                } else {
                    Err(SAShellFailure::Closed)
                }
            })
            .and_then(|()| request.validate());
        if let Err(reason) = admission {
            return Err(SAShellRejected { request, reason });
        }
        if self.core.shell.is_none() {
            match ShellHelper::new(self.core.id, self.core.shell_capacity) {
                Ok(helper) => self.core.shell = Some(helper),
                Err(reason) => return Err(SAShellRejected { request, reason }),
            }
        }
        self.core.shell.as_ref().unwrap().try_launch(request)
    }

    /// Number of accepted requests whose native call or payload reclamation
    /// has not finished. Zero alone does not prove helper apartment retirement.
    pub fn shell_pending(&self) -> usize {
        self.core.shell.as_ref().map_or(0, ShellHelper::pending)
    }
}

impl<A: SAApplication> Core<A> {
    /// Retirement is polled independently of closed ordinary post admission.
    pub(crate) fn poll_shell_retirement(&mut self) -> bool {
        let Some(helper) = &mut self.shell else {
            return true;
        };
        helper.close();
        match helper.poll_closed() {
            Ok(true) => {
                self.shell = None;
                true
            }
            Ok(false) => false,
            Err(error) => {
                // A crashed helper must not fabricate terminal receipts or
                // allow pending native final access to be treated as settled.
                self.fail(SAError::Shell(error), SAStopReason::BackendFailed);
                crate::host::fail_stop("shell helper failed before final access was established")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendOps, test::TestBackend};
    use crate::{SAContextPhase, SAHostState, SAShellDestination, SAShellOutcome, SAStopProgress};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct App {
        stops: usize,
    }
    impl SAApplication for App {
        type Message = ();
        type LocalEvent = ();
        fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
            cx.create_window(crate::SAWindowSpec::default())
                .map_err(|rejected| rejected.into_parts().1)?;
            Ok(())
        }
        fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
            self.stops += 1;
            SAStopProgress::Settled
        }
    }
    fn request() -> SAShellRequest {
        SAShellRequest {
            destination: SAShellDestination::Url("https://example.invalid/a%20b".into()),
        }
    }

    #[test]
    fn slow_shell_final_access_keeps_window_after_application_settles() {
        let mut core = Core::<App>::new().unwrap();
        let mut backend = TestBackend::default();
        let mut app = App { stops: 0 };
        core.start(&mut app, BackendOps::Test(&mut backend));
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        core.shell = Some(
            ShellHelper::spawn(core.id, 1, move || {
                move |_: &super::super::PreparedRequest| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    Ok(())
                }
            })
            .unwrap(),
        );
        let receipt = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
            .request_shell(request())
            .unwrap();
        assert_eq!(receipt.id().host(), core.id);
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        core.request_stop(SAStopReason::Application);
        for _ in 0..3 {
            core.poll_stop(&mut app, BackendOps::Test(&mut backend));
            assert_eq!(core.state, SAHostState::Stopping);
            assert!(backend.complete_destruction().is_none());
        }
        assert_eq!(app.stops, 1);
        let pending = core.shutdown_snapshot();
        assert_eq!(pending.shell_requests, 1);
        assert!(pending.shell_helper_retiring);
        assert!(pending.application_settled);
        assert_eq!(pending.retained_window_roots, 1);
        let mut cx = SAContext::new(
            &mut core,
            BackendOps::Unavailable,
            SAContextPhase::Retirement,
        );
        assert_eq!(cx.shell_pending(), 1);
        let rejected = cx.request_shell(request()).unwrap_err();
        assert_eq!(rejected.request, request());
        assert_eq!(rejected.reason, SAShellFailure::Closed);
        assert_eq!(cx.open_shell(&request()), Err(SAShellFailure::Closed));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while core.state == SAHostState::Stopping {
            assert!(Instant::now() < deadline);
            core.poll_stop(&mut app, BackendOps::Test(&mut backend));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(receipt.outcome(), SAShellOutcome::Complete(Ok(())));
        assert!(core.shell.is_none());
        assert_eq!(app.stops, 1);
        while let Some(key) = backend.complete_destruction() {
            core.native_destroyed(key);
        }
        assert_eq!(core.state, SAHostState::Closed);
        drop(core);
        assert_eq!(receipt.outcome(), SAShellOutcome::Complete(Ok(())));
    }

    #[test]
    fn invalid_shell_request_does_not_start_a_helper_or_consume_input() {
        let mut core = Core::<App>::new().unwrap();
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Startup);
        let invalid = SAShellRequest {
            destination: SAShellDestination::Url("invalid".into()),
        };
        let rejected = cx.request_shell(invalid.clone()).unwrap_err();
        assert_eq!(rejected.request, invalid);
        assert!(matches!(rejected.reason, SAShellFailure::InvalidInput(_)));
        assert!(core.shell.is_none());
    }

    #[test]
    fn zero_shell_requests_do_not_release_window_before_executor_final_drop_and_join() {
        struct DropLatch {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Drop for DropLatch {
            fn drop(&mut self) {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(3)).unwrap();
            }
        }
        let mut core = Core::<App>::new().unwrap();
        let mut backend = TestBackend::default();
        let mut app = App { stops: 0 };
        core.start(&mut app, BackendOps::Test(&mut backend));
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        core.shell = Some(
            ShellHelper::spawn(core.id, 1, move || {
                let final_access = DropLatch {
                    entered,
                    release: released,
                };
                move |_: &super::super::PreparedRequest| {
                    let _retained = &final_access;
                    Ok(())
                }
            })
            .unwrap(),
        );
        let receipt = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
            .request_shell(request())
            .unwrap();
        core.request_stop(SAStopReason::Application);
        core.poll_stop(&mut app, BackendOps::Test(&mut backend));
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(receipt.outcome(), SAShellOutcome::Complete(Ok(())));
        for _ in 0..3 {
            core.poll_stop(&mut app, BackendOps::Test(&mut backend));
            let pending = core.shutdown_snapshot();
            assert!(pending.application_settled);
            assert_eq!(pending.shell_requests, 0);
            assert!(pending.shell_helper_retiring);
            assert_eq!(pending.retained_window_roots, 1);
            assert_eq!(pending.pending_native_destructions, 0);
            assert_eq!(pending.state, SAHostState::Stopping);
            assert!(backend.complete_destruction().is_none());
        }
        assert_eq!(app.stops, 1);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while core.state == SAHostState::Stopping {
            assert!(Instant::now() < deadline);
            core.poll_stop(&mut app, BackendOps::Test(&mut backend));
            std::thread::sleep(Duration::from_millis(1));
        }
        let pending = core.shutdown_snapshot();
        assert!(!pending.shell_helper_retiring);
        assert_eq!(pending.retained_window_roots, 0);
        assert_eq!(pending.pending_native_destructions, 1);
        while let Some(key) = backend.complete_destruction() {
            core.native_destroyed(key);
        }
        assert_eq!(core.state, SAHostState::Closed);
        assert_eq!(core.shutdown_snapshot().pending_native_destructions, 0);
        assert_eq!(app.stops, 1);
    }
}
