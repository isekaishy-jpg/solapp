//! Regression witness using public SA and two own hidden Windows windows.
//! Native messages synthesize client/capture state; this does not certify pixels
//! or hardware movement. No focus change, pointer move or global input is used.

use solapp::*;
use std::time::{Duration, Instant};

// Test-only Win32 ABI. Message/hit-test values follow winuser.h; all HWNDs are
// extracted on the owner thread and checked against this process before use.
#[link(name = "user32")]
unsafe extern "system" {
    fn ShowCursor(show: i32) -> i32;
    fn GetCapture() -> isize;
    fn GetWindowThreadProcessId(window: isize, process: *mut u32) -> u32;
    fn PostMessageW(window: isize, message: u32, wparam: usize, lparam: isize) -> i32;
    fn SendMessageW(window: isize, message: u32, wparam: usize, lparam: isize) -> isize;
}
const WM_SETCURSOR: u32 = 0x0020;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_MOUSELEAVE: u32 = 0x02A3;
const HTCLIENT: isize = 1;
const HTCAPTION: isize = 2;

fn thread_cursor_count() -> i32 {
    // The temporary increment is balanced before returning the original count.
    unsafe {
        let incremented = ShowCursor(1);
        assert_eq!(ShowCursor(0), incremented - 1);
        incremented - 1
    }
}

struct CountRestore(i32);
impl Drop for CountRestore {
    fn drop(&mut self) {
        for _ in 0..32 {
            let current = thread_cursor_count();
            if current == self.0 {
                return;
            }
            unsafe { ShowCursor(i32::from(current < self.0)) };
        }
        panic!("could not restore original cursor count");
    }
}

struct App {
    targets: Vec<SAWindowTarget>,
    leases: Vec<SAWindowAccess>,
    baseline: i32,
    started: Instant,
    checked: bool,
}

impl SAApplication for App {
    type Message = ();
    type LocalEvent = ();

    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        for label in ["A", "B"] {
            let target = cx
                .create_window(SAWindowSpec {
                    title: format!("SA visibility regression {} {label}", std::process::id()),
                    visible: false,
                    ..SAWindowSpec::default()
                })
                .map_err(|rejected| rejected.into_parts().1)?;
            let lease = cx.acquire_window(target)?;
            let hwnd = lease.native_ref()?.hwnd()?.get();
            let mut pid = 0;
            assert_ne!(unsafe { GetWindowThreadProcessId(hwnd, &mut pid) }, 0);
            assert_eq!(pid, std::process::id());
            self.targets.push(target);
            self.leases.push(lease);
        }
        let recipient = cx.create_recipient()?;
        cx.subscribe(
            recipient,
            SAEventFilter::Input,
            SAPriority::default(),
            route,
        )?;
        cx.request_cursor_visibility(self.targets[0], false)?;
        let hwnd = self.leases[0].native_ref()?.hwnd()?.get();
        assert_ne!(
            unsafe { PostMessageW(hwnd, WM_MOUSEMOVE, 0, 1 | (1 << 16)) },
            0
        );
        cx.register_service(SAServiceSpec {
            points: SAServicePoints::ALL,
            budget: SAServiceBudget { records: 1 },
            fallback_interval: Duration::from_millis(10),
        })?;
        Ok(())
    }

    fn service(&mut self, _: &mut SAContext<'_, Self>, _: SAServiceRequest) -> SAServiceReport {
        assert!(self.started.elapsed() < Duration::from_secs(5));
        SAServiceReport::Quiescent
    }

    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        self.leases.clear();
        SAStopProgress::Settled
    }
}

fn client_message(hwnd: isize, hit: isize) {
    unsafe { SendMessageW(hwnd, WM_SETCURSOR, hwnd as usize, hit) };
}

fn route(app: &mut App, cx: &mut SAContext<'_, App>, event: &SAEvent<'_, (), ()>) -> SAPropagation {
    if let SAEvent::Input(record) = event
        && matches!(record.event, SAInputEvent::PointerMoved(_))
        && record.target == app.targets[0]
        && !app.checked
    {
        // Complete the synchronous assertions before TrackMouseEvent's later
        // real-pointer leave notification can retire the synthetic client claim.
        app.checked = true;
        let a = app.targets[0];
        let b = app.targets[1];
        let ah = app.leases[0].native_ref().unwrap().hwnd().unwrap().get();
        let bh = app.leases[1].native_ref().unwrap().hwnd().unwrap().get();
        let hidden = app.baseline - 1;
        assert_eq!(thread_cursor_count(), hidden, "A client entry must hide");
        cx.request_cursor_visibility(b, true).unwrap();
        assert_eq!(thread_cursor_count(), hidden, "unrelated visible request");
        cx.request_cursor_visibility(b, false).unwrap();
        assert_eq!(thread_cursor_count(), hidden, "unrelated hidden request");
        cx.request_input_mode(
            b,
            SAInputMode {
                relative_motion: true,
                confine_pointer: false,
            },
        )
        .unwrap();
        assert_eq!(
            thread_cursor_count(),
            hidden,
            "unrelated relative suppression"
        );
        cx.request_input_mode(b, SAInputMode::default()).unwrap();
        assert_eq!(
            thread_cursor_count(),
            hidden,
            "unrelated suppression restoration"
        );
        assert!(!cx.cursor_state(a).unwrap().requested_visible);
        println!("PASS: unrelated visibility/input-mode requests preserve A hide");

        // A captured hidden window remains authoritative outside its client.
        unsafe { SendMessageW(ah, WM_LBUTTONDOWN, 1, 1 | (1 << 16)) };
        assert_eq!(unsafe { GetCapture() }, ah);
        unsafe { SendMessageW(ah, WM_MOUSELEAVE, 0, 0) };
        assert_eq!(
            thread_cursor_count(),
            hidden,
            "own capture survives client leave"
        );
        cx.request_input_mode(
            a,
            SAInputMode {
                relative_motion: true,
                confine_pointer: false,
            },
        )
        .unwrap();
        cx.request_cursor_visibility(a, true).unwrap();
        // These hidden windows have no OS focus; relative selection alone must
        // not activate the SA focus-dependent suppression policy.
        assert!(!cx.cursor_state(a).unwrap().input_suppressed);
        assert_eq!(
            thread_cursor_count(),
            app.baseline,
            "nonfocused relative request does not suppress own visible capture"
        );
        cx.request_input_mode(a, SAInputMode::default()).unwrap();
        assert_eq!(
            thread_cursor_count(),
            app.baseline,
            "own capture restores visibility"
        );
        cx.request_cursor_visibility(a, false).unwrap();
        assert_eq!(thread_cursor_count(), hidden);

        cx.request_cursor_visibility(b, true).unwrap();
        assert_eq!(thread_cursor_count(), hidden);
        unsafe { SendMessageW(bh, WM_LBUTTONDOWN, 1, 1 | (1 << 16)) };
        assert_eq!(unsafe { GetCapture() }, bh);
        assert_eq!(
            thread_cursor_count(),
            app.baseline,
            "visible capture transfer restores A"
        );
        cx.request_cursor_visibility(b, false).unwrap();
        assert_eq!(thread_cursor_count(), hidden, "new capture owner hides");
        unsafe { SendMessageW(ah, WM_MOUSELEAVE, 0, 0) };
        assert_eq!(thread_cursor_count(), hidden, "late old-owner leave");
        unsafe { SendMessageW(bh, WM_LBUTTONUP, 0, 1 | (1 << 16)) };
        assert_eq!(unsafe { GetCapture() }, 0);
        assert_eq!(
            thread_cursor_count(),
            app.baseline,
            "capture loss outside client restores"
        );
        println!("PASS: capture transfer/loss and nonfocused relative requests balance visibility");

        client_message(bh, HTCLIENT);
        assert_eq!(
            thread_cursor_count(),
            hidden,
            "client message applies retained B hide"
        );
        client_message(ah, HTCAPTION);
        assert_eq!(thread_cursor_count(), hidden, "unrelated nonclient message");
        client_message(bh, HTCAPTION);
        assert_eq!(
            thread_cursor_count(),
            app.baseline,
            "owner nonclient restores"
        );
        client_message(bh, HTCLIENT);
        assert_eq!(thread_cursor_count(), hidden);
        cx.request_close(a).unwrap();
        assert_eq!(thread_cursor_count(), hidden, "unrelated close request");
        println!("PASS: client/nonclient authority and unrelated close preserve ownership");
        cx.request_stop();
    }
    SAPropagation::Continue
}

fn main() {
    let baseline = thread_cursor_count();
    let _restore = CountRestore(baseline);
    let mut app = App {
        targets: Vec::new(),
        leases: Vec::new(),
        baseline,
        started: Instant::now(),
        checked: false,
    };
    let mut host = SAHost::new(SAHostConfig::default()).unwrap();
    let result = host.run(&mut app).unwrap();
    assert!(app.checked);
    assert_eq!(result.windows_retired, 2);
    assert_eq!(host.state(), SAHostState::Closed);
    assert_eq!(thread_cursor_count(), baseline);
    println!("PASS: own windows destroyed and original native cursor count restored");
}
