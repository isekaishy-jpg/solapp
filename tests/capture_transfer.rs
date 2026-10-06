//! Native posted-message regression. The parent watchdog remains outside the
//! child's app callback, so a native reentry deadlock cannot hide its deadline.
//! Synthetic own-window messages do not measure physical input frequency.
#![cfg(windows)]

use solapp::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[link(name = "user32")]
unsafe extern "system" {
    fn GetCapture() -> isize;
    fn GetWindowThreadProcessId(window: isize, process: *mut u32) -> u32;
    fn PostMessageW(window: isize, message: u32, wparam: usize, lparam: isize) -> i32;
    fn SendMessageW(window: isize, message: u32, wparam: usize, lparam: isize) -> isize;
    fn ReleaseCapture() -> i32;
    fn DestroyWindow(window: isize) -> i32;
    fn IsWindow(window: isize) -> i32;
    fn ShowCursor(show: i32) -> i32;
}
const LEFT_DOWN: u32 = 0x0201;
const LEFT_UP: u32 = 0x0202;
const RIGHT_DOWN: u32 = 0x0204;
const RIGHT_UP: u32 = 0x0205;
const MIDDLE_DOWN: u32 = 0x0207;
const MIDDLE_UP: u32 = 0x0208;
const X_DOWN: u32 = 0x020B;
const X_UP: u32 = 0x020C;
const MOVE: u32 = 0x0200;
const POINT: isize = 1 | (1 << 16);

fn count() -> i32 {
    unsafe {
        let raised = ShowCursor(1);
        assert_eq!(ShowCursor(0), raised - 1);
        raised - 1
    }
}

fn post(window: isize, message: u32, wparam: usize) {
    post_at(window, message, wparam, POINT);
}

fn post_at(window: isize, message: u32, wparam: usize, point: isize) {
    assert_ne!(unsafe { PostMessageW(window, message, wparam, point) }, 0);
}

struct App {
    action: String,
    down: u32,
    up: u32,
    wparam: usize,
    targets: Vec<SAWindowTarget>,
    leases: Vec<SAWindowAccess>,
    handles: Vec<isize>,
    lost: usize,
    b_lost_before_press: bool,
    b_releases: usize,
    phase: u8,
    services_after_transfer: usize,
    baseline: i32,
}

impl SAApplication for App {
    type Message = ();
    type LocalEvent = ();

    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        for label in ["A", "B"] {
            let target = cx
                .create_window(SAWindowSpec {
                    title: format!("SA capture {} {label}", std::process::id()),
                    visible: false,
                    ..SAWindowSpec::default()
                })
                .map_err(|rejected| rejected.into_parts().1)?;
            let lease = cx.acquire_window(target)?;
            let handle = lease.native_ref()?.hwnd()?.get();
            let mut pid = 0;
            assert_ne!(unsafe { GetWindowThreadProcessId(handle, &mut pid) }, 0);
            assert_eq!(pid, std::process::id());
            self.targets.push(target);
            self.leases.push(lease);
            self.handles.push(handle);
        }
        let recipient = cx.create_recipient()?;
        cx.subscribe(
            recipient,
            SAEventFilter::Input,
            SAPriority::default(),
            route,
        )?;
        cx.register_service(SAServiceSpec {
            points: SAServicePoints::ALL,
            budget: SAServiceBudget { records: 1 },
            fallback_interval: Duration::from_millis(1),
        })?;
        // Dispatch starts after `started` returns: CaptureLost can then call the
        // application directly while B's acquisition is still on the native stack.
        post(self.handles[0], LEFT_DOWN, 1);
        Ok(())
    }

    fn service(&mut self, cx: &mut SAContext<'_, Self>, _: SAServiceRequest) -> SAServiceReport {
        if self.phase == 5 {
            self.services_after_transfer += 1;
            assert_eq!(unsafe { GetCapture() }, 0);
            assert_eq!(count(), self.baseline);
            if matches!(
                self.action.as_str(),
                "release" | "transfer" | "reacquire" | "reacquire_outer_first"
            ) {
                for layer in [SAInputStateLayer::Platform, SAInputStateLayer::Delivered] {
                    assert_eq!(
                        cx.input_state(self.targets[1], layer)
                            .unwrap()
                            .unwrap()
                            .held_buttons,
                        [],
                        "B's later explicit release clears its real press"
                    );
                }
                assert_eq!(
                    self.b_releases,
                    if self.action.starts_with("reacquire") {
                        2
                    } else {
                        1
                    },
                    "each B press is released exactly once"
                );
                println!(
                    "ORDER: B CaptureLost(empty) -> B presses -> balanced releases -> later service(empty)"
                );
            }
            cx.request_stop();
        }
        SAServiceReport::Quiescent
    }

    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        self.leases.clear();
        SAStopProgress::Settled
    }
}

fn route(app: &mut App, cx: &mut SAContext<'_, App>, event: &SAEvent<'_, (), ()>) -> SAPropagation {
    let SAEvent::Input(record) = event else {
        return SAPropagation::Continue;
    };
    let a = app.handles[0];
    let b = app.handles[1];
    if record.target == app.targets[1]
        && record.event == SAInputEvent::CaptureLost
        && app.phase == 3
        && matches!(
            app.action.as_str(),
            "release" | "transfer" | "reacquire" | "reacquire_outer_first"
        )
    {
        app.b_lost_before_press = true;
        for layer in [SAInputStateLayer::Platform, SAInputStateLayer::Delivered] {
            assert!(
                cx.input_state(app.targets[1], layer)
                    .unwrap()
                    .unwrap()
                    .held_buttons
                    .is_empty()
            );
        }
    }
    if app.phase == 4 && matches!(record.event, SAInputEvent::PointerMoved(_)) {
        if app.action.starts_with("reacquire") {
            assert_eq!(
                unsafe { GetCapture() },
                b,
                "first up must preserve the other held button's capture"
            );
            assert_eq!(count(), app.baseline - 1);
            let (held, last_up) = if app.action == "reacquire" {
                (SAMouseButton::Left, LEFT_UP)
            } else {
                (SAMouseButton::Right, RIGHT_UP)
            };
            for layer in [SAInputStateLayer::Platform, SAInputStateLayer::Delivered] {
                assert_eq!(
                    cx.input_state(app.targets[1], layer)
                        .unwrap()
                        .unwrap()
                        .held_buttons,
                    [held]
                );
            }
            assert_eq!(app.b_releases, 1);
            app.phase = 6;
            post(b, last_up, 0);
            post_at(a, MOVE, 0, 2 | (2 << 16));
            return SAPropagation::Continue;
        }
        let expected_capture = if app.action == "transfer" { a } else { 0 };
        assert_eq!(
            unsafe { GetCapture() },
            expected_capture,
            "B release preserves callback-selected capture"
        );
        assert_eq!(count(), app.baseline);
        if app.action == "transfer" {
            app.phase = 6;
            post(a, LEFT_UP, 0);
            post_at(a, MOVE, 0, 2 | (2 << 16));
        } else {
            app.phase = 5;
        }
    } else if app.phase == 6 && matches!(record.event, SAInputEvent::PointerMoved(_)) {
        assert_eq!(unsafe { GetCapture() }, 0);
        assert_eq!(count(), app.baseline);
        app.phase = 5;
    }
    if record.target == app.targets[0]
        && record.event == SAInputEvent::CaptureLost
        && app.phase == 3
    {
        app.lost += 1;
        assert_eq!(
            app.lost, 1,
            "same-owner acquisition must not emit capture loss"
        );
        assert_eq!(unsafe { GetCapture() }, b);
        // This operation needs B's window-state mutex, reproducing the old deadlock.
        println!("CaptureLost A callback requests B hidden; native capture is B");
        cx.request_cursor_visibility(app.targets[1], false).unwrap();
        println!("CaptureLost A callback changed B visibility");
        assert_eq!(count(), app.baseline - 1);
        match app.action.as_str() {
            "release" => {
                assert_ne!(unsafe { ReleaseCapture() }, 0);
            }
            "reacquire" | "reacquire_outer_first" => {
                assert_ne!(unsafe { ReleaseCapture() }, 0);
                unsafe { SendMessageW(b, RIGHT_DOWN, 2, POINT) };
                assert_eq!(unsafe { GetCapture() }, b);
            }
            "transfer" => {
                // A different native owner wins during B's acquisition callback.
                // Its event is buffered by the existing runner and delivered later.
                unsafe { SendMessageW(a, LEFT_DOWN, 1, POINT) };
                assert_eq!(unsafe { GetCapture() }, a);
            }
            "retire" => {
                // Exercise WindowData's recursive retirement while SetCapture is
                // suspended. This external destruction violates the SA lease
                // contract; the expected host fault must still finish cleanup.
                assert_ne!(unsafe { DestroyWindow(b) }, 0);
                assert_eq!(unsafe { IsWindow(b) }, 0);
            }
            "stop" => {
                cx.request_stop();
            }
            "visibility" => (),
            _ => panic!("unknown action"),
        }
        return SAPropagation::Continue;
    }
    if let SAInputEvent::MouseButton {
        state: SAButtonState::Pressed,
        button,
        ..
    } = record.event
    {
        if record.target == app.targets[0] {
            match app.phase {
                0 => {
                    assert_eq!(unsafe { GetCapture() }, a);
                    app.phase = 1;
                    post(a, RIGHT_DOWN, 2);
                }
                1 => {
                    assert_eq!(unsafe { GetCapture() }, a);
                    assert_eq!(app.lost, 0);
                    app.phase = 2;
                    // One release must preserve capture while the other button is held.
                    post(a, RIGHT_UP, 1);
                }
                _ => (),
            }
        } else if record.target == app.targets[1] && app.phase == 3 {
            assert_eq!(app.lost, 1);
            if app.action.starts_with("reacquire") {
                assert!(app.b_lost_before_press);
                // Reentrant right-down is delivered before the outer left-down.
                if button == SAMouseButton::Right {
                    return SAPropagation::Continue;
                }
                for layer in [SAInputStateLayer::Platform, SAInputStateLayer::Delivered] {
                    let held = &cx
                        .input_state(app.targets[1], layer)
                        .unwrap()
                        .unwrap()
                        .held_buttons;
                    assert_eq!(held.len(), 2);
                    assert!(held.contains(&SAMouseButton::Left));
                    assert!(held.contains(&SAMouseButton::Right));
                }
            }
            if matches!(app.action.as_str(), "release" | "transfer") {
                assert!(app.b_lost_before_press);
                for layer in [SAInputStateLayer::Platform, SAInputStateLayer::Delivered] {
                    assert_eq!(
                        cx.input_state(app.targets[1], layer)
                            .unwrap()
                            .unwrap()
                            .held_buttons,
                        [SAMouseButton::Left]
                    );
                }
            }
            app.phase = 4;
            match app.action.as_str() {
                "reacquire" | "reacquire_outer_first" => {
                    assert_eq!(unsafe { GetCapture() }, b);
                    let (first_up, held) = if app.action == "reacquire" {
                        (RIGHT_UP, 1)
                    } else {
                        (LEFT_UP, 2)
                    };
                    post(b, first_up, held);
                    post(a, MOVE, 0);
                }
                "visibility" => {
                    assert_eq!(unsafe { GetCapture() }, b);
                    assert_eq!(count(), app.baseline - 1);
                    post(b, app.up, app.wparam & 0xffff_0000);
                    post(a, MOVE, 0);
                }
                "transfer" => {
                    assert_eq!(unsafe { GetCapture() }, a);
                    assert_eq!(count(), app.baseline);
                    post(b, LEFT_UP, 0);
                    post(a, MOVE, 0);
                }
                "release" => {
                    assert_eq!(unsafe { GetCapture() }, 0);
                    assert_eq!(count(), app.baseline);
                    post(b, LEFT_UP, 0);
                    post(a, MOVE, 0);
                }
                "stop" => (),
                _ => unreachable!(),
            }
        }
    }
    if let SAInputEvent::MouseButton {
        state: SAButtonState::Released,
        button,
        ..
    } = record.event
    {
        if record.target == app.targets[1] && app.action.starts_with("reacquire") {
            app.b_releases += 1;
            if app.phase == 4 {
                assert_eq!(
                    record.origin,
                    SAInputOrigin::NativeWindow,
                    "first up must not reconcile a still-held button"
                );
            }
        }
        if record.target == app.targets[1] && matches!(app.action.as_str(), "release" | "transfer")
        {
            app.b_releases += 1;
            assert_eq!(record.origin, SAInputOrigin::NativeWindow);
        }
        if record.origin != SAInputOrigin::NativeWindow {
            return SAPropagation::Continue;
        }
        if app.phase == 2 && record.target == app.targets[0] && button == SAMouseButton::Right {
            assert_eq!(
                unsafe { GetCapture() },
                a,
                "multiple-button count preserved"
            );
            app.phase = 3;
            post(b, app.down, app.wparam);
        }
    }
    SAPropagation::Continue
}

fn capture_transfer_child() {
    let case = std::env::var("SOLAPP_CAPTURE_CASE").expect("watchdog supplies case");
    let (action, button) = case.split_once(':').unwrap();
    let (down, up, wparam) = match button {
        "left" => (LEFT_DOWN, LEFT_UP, 1),
        "right" => (RIGHT_DOWN, RIGHT_UP, 2),
        "middle" => (MIDDLE_DOWN, MIDDLE_UP, 0x10),
        "back" => (X_DOWN, X_UP, 1 << 16),
        "forward" => (X_DOWN, X_UP, 2 << 16),
        _ => panic!("unknown button"),
    };
    let mut app = App {
        action: action.to_owned(),
        down,
        up,
        wparam,
        targets: Vec::new(),
        leases: Vec::new(),
        handles: Vec::new(),
        lost: 0,
        b_lost_before_press: false,
        b_releases: 0,
        phase: 0,
        services_after_transfer: 0,
        baseline: count(),
    };
    let mut host = SAHost::new(SAHostConfig::default()).unwrap();
    let result = host.run(&mut app);
    assert_eq!(app.lost, 1);
    if action != "stop" && action != "retire" {
        assert!(
            app.services_after_transfer > 0,
            "services progress after transfer"
        );
    }
    if action == "retire" {
        assert!(
            matches!(
                result,
                Err(SAError::Native {
                    operation: SANativeOperation::RunEventLoop,
                    ..
                })
            ),
            "external destruction must report its native host fault"
        );
    } else {
        assert_eq!(result.unwrap().windows_retired, 2);
    }
    assert_eq!(host.state(), SAHostState::Closed);
    assert!(
        app.handles
            .iter()
            .all(|&handle| unsafe { IsWindow(handle) } == 0)
    );
    assert_eq!(
        count(),
        app.baseline,
        "retirement restores cursor contribution"
    );
    println!("PASS: {case}, native progress and both windows retired");
}

fn capture_transfer_watchdog() {
    for case in [
        "visibility:left",
        "visibility:right",
        "visibility:middle",
        "visibility:back",
        "visibility:forward",
        "release:left",
        "transfer:left",
        "reacquire:left",
        "reacquire_outer_first:left",
        "retire:left",
        "stop:left",
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--capture-child")
            .env("SOLAPP_CAPTURE_CASE", case)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "native child failed for {case}: {status}");
                break;
            }
            if start.elapsed() >= Duration::from_secs(8) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("native capture child hung for {case} (8-second parent watchdog)");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn main() {
    if std::env::args().any(|argument| argument == "--capture-child") {
        capture_transfer_child();
    } else {
        capture_transfer_watchdog();
    }
}
