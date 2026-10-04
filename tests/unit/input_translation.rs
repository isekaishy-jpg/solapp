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
