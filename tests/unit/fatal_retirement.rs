//! Process isolation qualifies the fatal contract; no native window is created.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::backend::{BackendOps, test::TestBackend};
use crate::{SAApplication, SAContext, SAError, SAStopProgress};

const CHILD_NAME: &str = "fatal_retirement_tests::fatal_retirement_child";
const CHILD_MODE: &str = "SOLAPP_FATAL_RETIREMENT_CHILD";
const ENTERED: &str = "SA_FATAL_FIXTURE_STOPPING_ENTERED";
const RETURNED: &str = "SA_FATAL_FIXTURE_NORMAL_RETURN";
const STATE_DROPPED: &str = "SA_FATAL_FIXTURE_BORROWED_STATE_DROPPED";
const PAYLOAD_DROPPED: &str = "SA_FATAL_FIXTURE_PANIC_PAYLOAD_DROPPED";

struct BorrowedState;
impl Drop for BorrowedState {
    fn drop(&mut self) {
        eprintln!("{STATE_DROPPED}");
    }
}

struct PanicPayload;
impl Drop for PanicPayload {
    fn drop(&mut self) {
        eprintln!("{PAYLOAD_DROPPED}");
        panic!("retirement panic payload destructor must never run");
    }
}

struct App<'a> {
    borrowed: &'a mut BorrowedState,
}
impl SAApplication for App<'_> {
    type Message = ();
    type LocalEvent = ();

    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        cx.request_stop();
        Ok(())
    }

    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        // Touch the real borrow before injecting a panic with arbitrary Drop.
        let _borrowed = &mut *self.borrowed;
        eprintln!("{ENTERED}");
        std::panic::panic_any(PanicPayload)
    }
}

#[test]
#[ignore = "private abort child; invoked only by the bounded parent test"]
fn fatal_retirement_child() {
    // Running ignored tests manually must not accidentally abort that runner.
    assert_eq!(std::env::var(CHILD_MODE).as_deref(), Ok("1"));
    crate::backend::windows::shell::suppress_child_abort_reporting();
    let mut borrowed = BorrowedState;
    let mut app = App {
        borrowed: &mut borrowed,
    };
    let mut core = crate::host::Core::<App<'_>>::new().unwrap();
    let mut backend = TestBackend::default();
    core.start(&mut app, BackendOps::Test(&mut backend));
    core.poll_stop(&mut app, BackendOps::Test(&mut backend));
    eprintln!("{RETURNED}");
    panic!("fatal retirement returned to its borrowed application state");
}

#[test]
fn stopping_panic_aborts_before_returning_or_dropping_borrowed_state() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            CHILD_NAME,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MODE, "1")
        .env("RUST_BACKTRACE", "0")
        .env("RUST_LIB_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "fatal retirement child timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "abort child returned success");
    assert!(
        stderr.contains(ENTERED),
        "child never entered stopping: {stderr}"
    );
    assert!(
        stderr.contains(
            "Solapp fatal retirement failure: application retirement panicked before final access settled"
        ),
        "explicit fail-stop was not observed: {stderr}"
    );
    for forbidden in [RETURNED, STATE_DROPPED, PAYLOAD_DROPPED] {
        assert!(
            !stderr.contains(forbidden),
            "unexpected {forbidden}: {stderr}"
        );
    }
    eprintln!(
        "qualified fatal retirement child: {}; explicit fail-stop, no return or Drop markers",
        output.status
    );
}
