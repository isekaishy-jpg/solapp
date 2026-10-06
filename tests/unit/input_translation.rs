use super::*;
use crate::identity::{SAHostId, SAWindowGeneration, WindowSlots};
use crate::text::SATextCaret;
use winit::event::{DeviceId, TouchPhase};
use winit::keyboard::KeyCode;

fn target() -> SAWindowTarget {
    let mut slots = WindowSlots::<()>::new(SAHostId::allocate().unwrap());
    SAWindowTarget {
        id: slots.reserve().unwrap(),
        generation: SAWindowGeneration::INITIAL,
    }
}

fn begin(text: &mut TextState, target: SAWindowTarget) -> crate::text::SATextSessionId {
    text.begin(
        target,
        SATextCaret {
            position: SAPhysicalPosition { x: 0.0, y: 0.0 },
            size: SAPhysicalSize {
                width: 1,
                height: 10,
            },
        },
    )
    .unwrap()
}

fn translate(
    adapter: &mut NativeInputAdapter,
    target: SAWindowTarget,
    event: &WindowEvent,
    state: &SAInputState,
    text: &mut TextState,
) -> Vec<NormalizedInput> {
    adapter
        .window_event(
            target,
            event,
            state,
            SAPhysicalSize {
                width: 800,
                height: 600,
            },
            1.5,
            text,
        )
        .unwrap()
}

#[test]
fn key_identity_repeat_modifiers_and_committed_text_are_separate() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let session = begin(&mut text, target);
    let state = SAInputState {
        modifiers: SAModifiers {
            control: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let batch = adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::KeyA),
            &Key::Character("a".into()),
            KeyLocation::Standard,
            ElementState::Pressed,
            true,
            false,
            Some("\x01"),
            &state,
            &text,
        )
        .unwrap();
    assert_eq!(batch.len(), 2);
    assert!(matches!(&batch[0].event, SAInputEvent::Key {
        physical: SAPhysicalKey::ScanCode(0x1e), logical: SALogicalKey::Character(value),
        repeat: true, modifiers: SAModifiers { control: true, .. }, ..
    } if value == "a"));
    assert_eq!(
        batch[1].event,
        SAInputEvent::Text {
            session,
            text: "\x01".into()
        }
    );
    assert!(
        batch
            .iter()
            .all(|record| record.target == target && record.source_time.is_none())
    );
    assert_eq!(
        logical_key(&Key::Dead(Some('´'))),
        SALogicalKey::Dead(Some('´'))
    );
    assert_eq!(
        logical_key(&Key::Unidentified(NativeKey::Windows(0xf1))),
        SALogicalKey::Unidentified(0xf1)
    );
    assert_eq!(
        physical_key(PhysicalKey::Unidentified(NativeKeyCode::Windows(0xe07f))),
        SAPhysicalKey::Unidentified(0xe07f)
    );
    assert_eq!(named_key(NamedKey::F35), SANamedKey::Function(35));
}

#[test]
fn synthetic_keys_never_commit_and_ime_commit_has_one_source_and_old_session() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let first = begin(&mut text, target);
    let state = SAInputState::default();
    let batch = adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::KeyA),
            &Key::Character("a".into()),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
            true,
            Some("a"),
            &state,
            &text,
        )
        .unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].origin, SAInputOrigin::FocusSynthetic);
    translate(
        &mut adapter,
        target,
        &WindowEvent::Ime(Ime::Enabled),
        &state,
        &mut text,
    );
    let during = adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::KeyA),
            &Key::Character("a".into()),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
            false,
            Some("a"),
            &state,
            &text,
        )
        .unwrap();
    assert_eq!(during.len(), 1);
    let committed = translate(
        &mut adapter,
        target,
        &WindowEvent::Ime(Ime::Commit("漢".into())),
        &state,
        &mut text,
    );
    assert_eq!(committed.len(), 1);
    assert_eq!(
        committed[0].event,
        SAInputEvent::Text {
            session: first,
            text: "漢".into()
        }
    );
    let second = begin(&mut text, target);
    let stale = translate(
        &mut adapter,
        target,
        &WindowEvent::Ime(Ime::Commit("旧".into())),
        &state,
        &mut text,
    );
    assert_eq!(
        stale[0].event,
        SAInputEvent::Text {
            session: first,
            text: "旧".into()
        }
    );
    assert!(!text.is_live(first));
    assert!(text.is_live(second));
}

#[test]
fn capture_and_focus_reconcile_held_receipt_state_without_duplicate_release() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let held = SAInputState {
        held_buttons: vec![SAMouseButton::Left],
        position: Some(SAPhysicalPosition { x: 2.0, y: 3.0 }),
        ..Default::default()
    };
    let batch = translate(
        &mut adapter,
        target,
        &WindowEvent::MouseCaptureLost,
        &held,
        &mut text,
    );
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].origin, SAInputOrigin::Reconciliation);
    assert!(matches!(
        batch[0].event,
        SAInputEvent::MouseButton {
            button: SAMouseButton::Left,
            state: SAButtonState::Released,
            position: Some(SAPhysicalPosition { x: 2.0, y: 3.0 }),
            scale: 1.5,
        }
    ));
    assert_eq!(batch[1].event, SAInputEvent::CaptureLost);
    let released = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    assert!(
        translate(
            &mut adapter,
            target,
            &released,
            &SAInputState::default(),
            &mut text
        )
        .is_empty()
    );
    let keys = SAInputState {
        held_keys: vec![SAPhysicalKey::ScanCode(0x1e)],
        ..Default::default()
    };
    let batch = translate(
        &mut adapter,
        target,
        &WindowEvent::Focused(false),
        &keys,
        &mut text,
    );
    assert_eq!(batch.len(), 2);
    assert!(matches!(
        batch[0].event,
        SAInputEvent::Key {
            state: SAButtonState::Released,
            synthetic: true,
            ..
        }
    ));
    assert_eq!(batch[1].event, SAInputEvent::Focus(false));
    let prefix_released = translate(
        &mut adapter,
        target,
        &WindowEvent::Focused(false),
        &SAInputState::default(),
        &mut text,
    );
    assert_eq!(prefix_released.len(), 1);
}

#[test]
fn pointer_wheel_association_and_raw_source_units_do_not_use_delivery_queries() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let event = WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.25, -0.5),
        phase: TouchPhase::Moved,
    };
    let unknown = translate(
        &mut adapter,
        target,
        &event,
        &SAInputState::default(),
        &mut text,
    );
    assert!(matches!(
        unknown[0].event,
        SAInputEvent::Wheel {
            position: None,
            delta: SAScrollDelta::Lines { x: 0.25, y: -0.5 },
            scale: 1.5
        }
    ));
    let state = SAInputState {
        position: Some(SAPhysicalPosition { x: 10.0, y: 20.0 }),
        ..Default::default()
    };
    let located = translate(&mut adapter, target, &event, &state, &mut text);
    assert!(matches!(
        located[0].event,
        SAInputEvent::Wheel {
            position: Some(SAPhysicalPosition { x: 10.0, y: 20.0 }),
            ..
        }
    ));
    let device = crate::SAInputDeviceId {
        host: target.id.host(),
        native: DeviceId::dummy(),
    };
    let raw = adapter
        .device_event(
            Some(target),
            &DeviceEvent::MouseMotionAbsolute {
                position: (0, 65535),
                virtual_desktop: true,
            },
            device,
        )
        .unwrap();
    assert_eq!(
        raw[0].event,
        SAInputEvent::RawAbsoluteMotion {
            x: 0,
            y: 65535,
            virtual_desktop: true
        }
    );
    assert_eq!(raw[0].origin, SAInputOrigin::RawDevice);
    assert_eq!(raw[0].device, Some(device));
    assert!(located.iter().all(|record| record.device.is_none()));
    assert!(
        adapter
            .device_event(
                None,
                &DeviceEvent::MouseMotion { delta: (1.0, 2.0) },
                device
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        adapter
            .device_event(
                Some(target),
                &DeviceEvent::Motion {
                    axis: 0,
                    value: 1.0
                },
                device,
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        adapter
            .device_event(
                Some(target),
                &DeviceEvent::Button {
                    button: 0,
                    state: ElementState::Pressed
                },
                device,
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        adapter
            .device_event(
                Some(target),
                &DeviceEvent::MouseWheel {
                    delta: MouseScrollDelta::LineDelta(0.0, 1.0)
                },
                device,
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn focus_fallback_retains_last_observed_key_meaning_and_location() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let empty = SAInputState::default();
    let pressed = adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::ControlRight),
            &Key::Named(NamedKey::Control),
            KeyLocation::Right,
            ElementState::Pressed,
            false,
            false,
            None,
            &empty,
            &text,
        )
        .unwrap();
    assert_eq!(pressed.len(), 1);
    let state = SAInputState {
        held_keys: vec![SAPhysicalKey::ScanCode(0xe01d)],
        ..Default::default()
    };
    let batch = translate(
        &mut adapter,
        target,
        &WindowEvent::Focused(false),
        &state,
        &mut text,
    );
    assert!(matches!(
        batch[0].event,
        SAInputEvent::Key {
            physical: SAPhysicalKey::ScanCode(0xe01d),
            logical: SALogicalKey::Named(SANamedKey::Control),
            location: SAKeyLocation::Right,
            state: SAButtonState::Released,
            repeat: false,
            synthetic: true,
            ..
        }
    ));
    assert!(
        adapter
            .key_event(
                target,
                PhysicalKey::Code(KeyCode::ControlRight),
                &Key::Named(NamedKey::Control),
                KeyLocation::Right,
                ElementState::Released,
                false,
                true,
                None,
                &empty,
                &text
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn output_backing_matches_complete_event_and_ignores_held_key_population() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let held = SAInputState {
        held_keys: (0..4096).map(SAPhysicalKey::ScanCode).collect(),
        held_buttons: vec![SAMouseButton::Left, SAMouseButton::Right],
        ..Default::default()
    };
    let pointer = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(12.0, 24.0),
    };
    for state in [&SAInputState::default(), &held] {
        let (batch, counts) = crate::allocation_probe::measure(|| {
            translate(&mut adapter, target, &pointer, state, &mut text)
        });
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.capacity(), 1);
        assert_eq!(counts.allocations, 1);
        assert_eq!(counts.reallocations, 0);
        println!(
            "input pointer held={} output={} capacity={} allocations={} reallocations={}",
            state.held_keys.len(),
            batch.len(),
            batch.capacity(),
            counts.allocations,
            counts.reallocations
        );
    }
    for event in [
        WindowEvent::CloseRequested,
        WindowEvent::CursorEntered {
            device_id: DeviceId::dummy(),
        },
        WindowEvent::Ime(Ime::Preedit("unused".into(), None)),
        WindowEvent::Ime(Ime::Commit("unused".into())),
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Middle,
        },
    ] {
        let (batch, counts) = crate::allocation_probe::measure(|| {
            translate(&mut adapter, target, &event, &held, &mut text)
        });
        assert_eq!(batch.capacity(), 0);
        assert_eq!(counts.allocations, 0);
        assert_eq!(counts.reallocations, 0);
        println!(
            "input ignored/inactive event={event:?} allocations={}",
            counts.allocations
        );
    }
    let focus = translate(
        &mut adapter,
        target,
        &WindowEvent::Focused(false),
        &held,
        &mut text,
    );
    assert_eq!(focus.len(), 4099);
    assert_eq!(focus.capacity(), 4099);
    assert!(focus[..4096].iter().all(|record| matches!(
        record.event,
        SAInputEvent::Key {
            state: SAButtonState::Released,
            ..
        }
    ) && record.origin == SAInputOrigin::Reconciliation));
    assert!(matches!(
        focus[4096].event,
        SAInputEvent::MouseButton {
            button: SAMouseButton::Left,
            ..
        }
    ));
    assert!(matches!(
        focus[4097].event,
        SAInputEvent::MouseButton {
            button: SAMouseButton::Right,
            ..
        }
    ));
    assert_eq!(focus[4098].event, SAInputEvent::Focus(false));
    let capture = translate(
        &mut adapter,
        target,
        &WindowEvent::MouseCaptureLost,
        &held,
        &mut text,
    );
    assert_eq!(capture.len(), 3);
    assert_eq!(capture.capacity(), 3);
    assert_eq!(capture[2].event, SAInputEvent::CaptureLost);
}

#[test]
fn batch_reservation_failure_precedes_reconciliation_and_ime_mutation() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    begin(&mut text, target);
    let pressed = adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::ControlRight),
            &Key::Named(NamedKey::Control),
            KeyLocation::Right,
            ElementState::Pressed,
            false,
            false,
            None,
            &SAInputState::default(),
            &text,
        )
        .unwrap();
    assert_eq!(pressed.len(), 1);
    let state = SAInputState {
        held_keys: vec![SAPhysicalKey::ScanCode(0xe01d)],
        held_buttons: vec![SAMouseButton::Left, SAMouseButton::Right],
        ..Default::default()
    };
    adapter.fail_next_batch_reservation.set(true);
    let failed = adapter.window_event(
        target,
        &WindowEvent::Focused(false),
        &state,
        SAPhysicalSize {
            width: 800,
            height: 600,
        },
        1.5,
        &mut text,
    );
    assert!(matches!(failed, Err(SAError::AllocationFailed)));
    assert!(
        adapter
            .keys
            .contains_key(&(target, SAPhysicalKey::ScanCode(0xe01d)))
    );
    assert_eq!(state.held_buttons.len(), 2);
    let retry = translate(
        &mut adapter,
        target,
        &WindowEvent::Focused(false),
        &state,
        &mut text,
    );
    assert_eq!(retry.len(), 4);
    assert!(matches!(
        retry[0].event,
        SAInputEvent::Key {
            logical: SALogicalKey::Named(SANamedKey::Control),
            location: SAKeyLocation::Right,
            ..
        }
    ));
    assert!(adapter.keys.is_empty());
    translate(
        &mut adapter,
        target,
        &WindowEvent::Ime(Ime::Enabled),
        &state,
        &mut text,
    );
    adapter.fail_next_batch_reservation.set(true);
    assert!(matches!(
        adapter.window_event(
            target,
            &WindowEvent::Ime(Ime::Disabled),
            &state,
            SAPhysicalSize {
                width: 1,
                height: 1
            },
            1.0,
            &mut text
        ),
        Err(SAError::AllocationFailed)
    ));
    assert!(text.composing(target));
    let cleared = translate(
        &mut adapter,
        target,
        &WindowEvent::Ime(Ime::Disabled),
        &state,
        &mut text,
    );
    assert_eq!(cleared.len(), 1);
    assert!(!text.composing(target));
}

#[test]
fn keyboard_backing_is_one_or_two_records_and_suppressed_release_has_none() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    begin(&mut text, target);
    let state = SAInputState::default();
    // Warm the key-meaning map before measuring only the normalizer allocations.
    adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::Enter),
            &Key::Named(NamedKey::Enter),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
            false,
            None,
            &state,
            &text,
        )
        .unwrap();
    let (batch, counts) = crate::allocation_probe::measure(|| {
        adapter
            .key_event(
                target,
                PhysicalKey::Code(KeyCode::Enter),
                &Key::Named(NamedKey::Enter),
                KeyLocation::Standard,
                ElementState::Pressed,
                true,
                false,
                None,
                &state,
                &text,
            )
            .unwrap()
    });
    assert_eq!(batch.capacity(), 1);
    assert_eq!(counts.allocations, 1);
    assert_eq!(counts.reallocations, 0);
    println!(
        "input keyboard field boundary no-text allocations={} capacity={} (warm meaning map)",
        counts.allocations,
        batch.capacity()
    );
    let text_batch = adapter
        .key_event(
            target,
            PhysicalKey::Code(KeyCode::Enter),
            &Key::Named(NamedKey::Enter),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
            false,
            Some("\r"),
            &state,
            &text,
        )
        .unwrap();
    assert_eq!(text_batch.capacity(), 2);
    assert_eq!(text_batch.len(), 2);
    adapter.fail_next_batch_reservation.set(true);
    let failed = adapter.key_event(
        target,
        PhysicalKey::Code(KeyCode::KeyA),
        &Key::Character("a".into()),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
        false,
        Some("a"),
        &state,
        &text,
    );
    assert!(matches!(failed, Err(SAError::AllocationFailed)));
    assert!(
        !adapter
            .keys
            .contains_key(&(target, SAPhysicalKey::ScanCode(0x1e)))
    );
    let (released, counts) = crate::allocation_probe::measure(|| {
        adapter
            .key_event(
                target,
                PhysicalKey::Code(KeyCode::Enter),
                &Key::Named(NamedKey::Enter),
                KeyLocation::Standard,
                ElementState::Released,
                false,
                false,
                None,
                &state,
                &text,
            )
            .unwrap()
    });
    assert_eq!(released.capacity(), 0);
    assert_eq!(counts.allocations, 0);
    assert!(adapter.keys.is_empty());
    println!(
        "input suppressed keyboard release allocations={} capacity={}",
        counts.allocations,
        released.capacity()
    );
}

#[test]
fn ime_without_session_changes_composition_without_output_and_geometry_remains_native() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let state = SAInputState::default();
    let enabled = translate(
        &mut adapter,
        target,
        &WindowEvent::Ime(Ime::Enabled),
        &state,
        &mut text,
    );
    assert_eq!(enabled.capacity(), 0);
    assert!(text.composing(target));
    for ime in [
        Ime::Preedit("x".into(), Some((0, 1))),
        Ime::Commit("x".into()),
        Ime::Disabled,
    ] {
        let (batch, counts) = crate::allocation_probe::measure(|| {
            translate(
                &mut adapter,
                target,
                &WindowEvent::Ime(ime),
                &state,
                &mut text,
            )
        });
        assert_eq!(batch.capacity(), 0);
        assert_eq!(counts.allocations, 0);
    }
    assert!(!text.composing(target));
    let resized = translate(
        &mut adapter,
        target,
        &WindowEvent::Resized(winit::dpi::PhysicalSize::new(100, 200)),
        &state,
        &mut text,
    );
    assert_eq!(resized.capacity(), 1);
    assert_eq!(
        resized[0].event,
        SAInputEvent::Geometry {
            size: SAPhysicalSize {
                width: 100,
                height: 200
            },
            scale: 1.5
        }
    );
    assert_eq!(resized[0].origin, SAInputOrigin::NativeWindow);
}

#[test]
fn mouse_modifier_and_relative_motion_records_keep_observed_units_and_origins() {
    let target = target();
    let mut adapter = NativeInputAdapter::new();
    let mut text = TextState::new();
    let state = SAInputState {
        position: Some(SAPhysicalPosition { x: -5.5, y: 200.25 }),
        ..Default::default()
    };
    let pressed = translate(
        &mut adapter,
        target,
        &WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Other(7),
        },
        &state,
        &mut text,
    );
    assert_eq!(pressed.capacity(), 1);
    assert_eq!(
        pressed[0].event,
        SAInputEvent::MouseButton {
            button: SAMouseButton::Other(7),
            state: SAButtonState::Pressed,
            position: state.position,
            scale: 1.5,
        }
    );
    let modifiers = translate(
        &mut adapter,
        target,
        &WindowEvent::ModifiersChanged(
            (winit::keyboard::ModifiersState::SHIFT | winit::keyboard::ModifiersState::ALT).into(),
        ),
        &state,
        &mut text,
    );
    assert_eq!(modifiers.capacity(), 1);
    assert_eq!(
        modifiers[0].event,
        SAInputEvent::Modifiers(SAModifiers {
            shift: true,
            control: false,
            alt: true,
            super_key: false,
        })
    );
    let device = crate::SAInputDeviceId {
        host: target.id.host(),
        native: DeviceId::dummy(),
    };
    let relative = adapter
        .device_event(
            Some(target),
            &DeviceEvent::MouseMotion {
                delta: (-0.25, 3.5),
            },
            device,
        )
        .unwrap();
    assert_eq!(relative.capacity(), 1);
    assert_eq!(
        relative[0].event,
        SAInputEvent::RelativeMotion { x: -0.25, y: 3.5 }
    );
    assert_eq!(relative[0].origin, SAInputOrigin::RawDevice);
    assert_eq!(relative[0].device, Some(device));
    assert!(
        pressed
            .iter()
            .chain(&modifiers)
            .all(|record| record.origin == SAInputOrigin::NativeWindow && record.device.is_none())
    );
}
