//! Packet and failure witnesses for the Solapp Windows extensions. Public success and
//! synchronous capture delivery are exercised by the isolated host witness.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP,
};
use windows_sys::Win32::UI::Input::{
    RAWINPUT, RAWINPUTHEADER, RAWINPUT_0, RAWMOUSE, RAWMOUSE_0, RAWMOUSE_0_0, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    RI_MOUSE_BUTTON_1_DOWN, RI_MOUSE_BUTTON_1_UP, RI_MOUSE_HWHEEL, RI_MOUSE_WHEEL,
};

use super::runner::EventLoopRunner;
use super::{handle_raw_input, wrap_device_id, ActiveEventLoop, RootAEL, ThreadMsgTargetData};
use crate::cursor::{CursorImage, OnlyCursorImageSource};
use crate::event::{DeviceEvent, ElementState, Event, MouseScrollDelta};
use crate::event_loop::DeviceEvents;
use crate::platform::windows::ActiveEventLoopExtWindows;
use crate::platform_impl::platform::icon::WinCursor;
use crate::window::CustomCursorSource;

fn invalid_native_target() -> RootAEL {
    // This runner has no native target or installed callback. The operations
    // under test do not dispatch messages or transfer ownership to this HWND.
    let target = -1;
    RootAEL {
        p: ActiveEventLoop {
            thread_id: unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
            thread_msg_target: target,
            runner_shared: Rc::new(EventLoopRunner::new(target)),
        },
        _marker: PhantomData,
    }
}

fn native_failure_source() -> CustomCursorSource {
    // Bypass image validation only inside this private witness to drive a native
    // failure deterministically, without adding a production injection API.
    CustomCursorSource {
        inner: OnlyCursorImageSource(CursorImage {
            rgba: Vec::new(),
            width: 0,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
        }),
    }
}

#[test]
fn native_cursor_failure_is_an_error_and_legacy_api_keeps_placeholder() {
    let event_loop = invalid_native_target();
    assert!(event_loop
        .try_create_custom_cursor(native_failure_source())
        .is_err());
    let legacy = event_loop.create_custom_cursor(native_failure_source());
    assert!(matches!(legacy.inner, WinCursor::Failed));
}

#[test]
fn raw_registration_failure_returns_windows_error_and_legacy_api_stays_void() {
    let event_loop = invalid_native_target();
    let error = event_loop
        .try_listen_device_events(DeviceEvents::Always)
        .unwrap_err();
    assert!(error.raw_os_error().is_some_and(|code| code != 0));
    // Preserve the legacy ignore-result contract even when native registration fails.
    event_loop.listen_device_events(DeviceEvents::Always);
}

fn mouse_packet_events(
    flags: u16,
    position: (i32, i32),
    button_flags: u16,
    wheel_data: i16,
) -> Vec<DeviceEvent> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let output = Rc::clone(&events);
    let runner = Rc::new(EventLoopRunner::new(0));
    // SAFETY: this callback owns all captured state; it borrows no stack data
    // and is explicitly cleared before the runner is released.
    unsafe {
        runner.set_event_handler(move |event| {
            if let Event::DeviceEvent { device_id, event } = event {
                assert_eq!(device_id, wrap_device_id(0x1234));
                output.borrow_mut().push(event);
            }
        });
    }
    let userdata = ThreadMsgTargetData {
        event_loop_runner: Rc::clone(&runner),
    };
    let data = RAWINPUT {
        header: RAWINPUTHEADER {
            dwType: RIM_TYPEMOUSE,
            dwSize: std::mem::size_of::<RAWINPUT>() as u32,
            hDevice: 0x1234,
            wParam: 0,
        },
        data: RAWINPUT_0 {
            mouse: RAWMOUSE {
                usFlags: flags,
                Anonymous: RAWMOUSE_0 {
                    Anonymous: RAWMOUSE_0_0 {
                        usButtonFlags: button_flags,
                        usButtonData: wheel_data as u16,
                    },
                },
                ulRawButtons: 0,
                lLastX: position.0,
                lLastY: position.1,
                ulExtraInformation: 0,
            },
        },
    };
    // SAFETY: the initialized RAWINPUT mouse union matches its RIM_TYPEMOUSE header.
    unsafe { handle_raw_input(&userdata, data) };
    runner.clear_event_handler();
    let result = events.borrow().clone();
    result
}

#[test]
fn raw_absolute_packets_preserve_coordinates_without_relative_motion() {
    for (flags, position, virtual_desktop) in [
        (MOUSE_MOVE_ABSOLUTE, (0, 0), false),
        (MOUSE_MOVE_ABSOLUTE, (65535, 65535), false),
        (
            MOUSE_MOVE_ABSOLUTE | MOUSE_VIRTUAL_DESKTOP,
            (12000, 32000),
            true,
        ),
        // Retain observed values without installing a validation or camera policy.
        (MOUSE_MOVE_ABSOLUTE | 0x08, (-1, 65536), false),
    ] {
        assert_eq!(
            mouse_packet_events(flags as u16, position, 0, 0),
            vec![DeviceEvent::MouseMotionAbsolute {
                position,
                virtual_desktop
            }],
        );
    }
}

#[test]
fn raw_relative_packets_keep_existing_axes_and_delta() {
    assert_eq!(
        mouse_packet_events(0, (3, -4), 0, 0),
        vec![
            DeviceEvent::Motion {
                axis: 0,
                value: 3.0
            },
            DeviceEvent::Motion {
                axis: 1,
                value: -4.0
            },
            DeviceEvent::MouseMotion { delta: (3.0, -4.0) },
        ],
    );
    assert!(mouse_packet_events(0, (0, 0), 0, 0).is_empty());
}

#[test]
fn mixed_raw_absolute_packets_keep_button_and_wheel_handling() {
    for (button_flags, wheel_data, delta, state) in [
        (
            RI_MOUSE_WHEEL | RI_MOUSE_BUTTON_1_DOWN,
            60,
            (0.0, 0.5),
            ElementState::Pressed,
        ),
        (
            RI_MOUSE_HWHEEL | RI_MOUSE_BUTTON_1_UP,
            -60,
            (0.5, 0.0),
            ElementState::Released,
        ),
    ] {
        assert_eq!(
            mouse_packet_events(
                (MOUSE_MOVE_ABSOLUTE | MOUSE_VIRTUAL_DESKTOP) as u16,
                (20000, 30000),
                button_flags as u16,
                wheel_data,
            ),
            vec![
                DeviceEvent::MouseMotionAbsolute {
                    position: (20000, 30000),
                    virtual_desktop: true,
                },
                DeviceEvent::MouseWheel {
                    delta: MouseScrollDelta::LineDelta(delta.0, delta.1)
                },
                DeviceEvent::Button { button: 0, state },
            ],
        );
    }
}
