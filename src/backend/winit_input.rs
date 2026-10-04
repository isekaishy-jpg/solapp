//! Receipt-time native translation. This module never invokes application code.

use std::collections::HashMap;
use std::time::Duration;

use winit::event::{DeviceEvent, ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKey, NativeKeyCode, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::scancode::PhysicalKeyExtScancode;

use crate::text::TextState;
use crate::{
    SAButtonState, SAError, SAInputEvent, SAInputOrigin, SAInputState, SAKeyLocation, SALogicalKey,
    SAModifiers, SAMouseButton, SANamedKey, SAPhysicalKey, SAPhysicalPosition, SAPhysicalSize,
    SAScrollDelta, SAWindowTarget,
};

pub(crate) struct NormalizedInput {
    pub(crate) target: SAWindowTarget,
    pub(crate) event: SAInputEvent,
    pub(crate) origin: SAInputOrigin,
    pub(crate) source_time: Option<Duration>,
    pub(crate) device: Option<crate::SAInputDeviceId>,
}

struct KeyMeaning {
    logical: SALogicalKey,
    location: SAKeyLocation,
}

pub(crate) struct NativeInputAdapter {
    keys: HashMap<(SAWindowTarget, SAPhysicalKey), KeyMeaning>,
}

impl NativeInputAdapter {
    pub(crate) fn new() -> Self {
        Self {
            keys: HashMap::new(),
        }
    }

    /// `state` is the platform layer before this event, never the delivered layer.
    pub(crate) fn window_event(
        &mut self,
        target: SAWindowTarget,
        event: &WindowEvent,
        state: &SAInputState,
        physical_size: SAPhysicalSize,
        scale: f64,
        text: &mut TextState,
    ) -> Result<Vec<NormalizedInput>, SAError> {
        let mut batch = Vec::new();
        batch
            .try_reserve(2 + state.held_keys.len() + state.held_buttons.len())
            .map_err(|_| SAError::AllocationFailed)?;
        match event {
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                return self.key_event(
                    target,
                    event.physical_key,
                    &event.logical_key,
                    event.location,
                    event.state,
                    event.repeat,
                    *is_synthetic,
                    event.text_with_all_modifiers(),
                    state,
                    text,
                );
            }
            WindowEvent::CursorMoved { position, .. } => push(
                &mut batch,
                target,
                SAInputEvent::PointerMoved(SAPhysicalPosition {
                    x: position.x,
                    y: position.y,
                }),
                SAInputOrigin::NativeWindow,
            ),
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => {
                let button = mouse_button(*button);
                let button_state = transition(*button_state);
                // Reconciliation may already have released this button, notably
                // synchronous capture loss during intentional last-button up.
                if button_state == SAButtonState::Pressed || state.held_buttons.contains(&button) {
                    push(
                        &mut batch,
                        target,
                        SAInputEvent::MouseButton {
                            button,
                            state: button_state,
                            position: state.position,
                            scale,
                        },
                        SAInputOrigin::NativeWindow,
                    );
                }
            }
            WindowEvent::MouseWheel { delta, .. } => push(
                &mut batch,
                target,
                SAInputEvent::Wheel {
                    delta: scroll_delta(*delta),
                    position: state.position,
                    scale,
                },
                SAInputOrigin::NativeWindow,
            ),
            WindowEvent::ModifiersChanged(modifiers) => {
                let modifiers = modifiers.state();
                push(
                    &mut batch,
                    target,
                    SAInputEvent::Modifiers(SAModifiers {
                        shift: modifiers.shift_key(),
                        control: modifiers.control_key(),
                        alt: modifiers.alt_key(),
                        super_key: modifiers.super_key(),
                    }),
                    SAInputOrigin::NativeWindow,
                );
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    for physical in &state.held_keys {
                        let meaning = self.keys.remove(&(target, *physical));
                        push(
                            &mut batch,
                            target,
                            SAInputEvent::Key {
                                physical: *physical,
                                logical: meaning
                                    .as_ref()
                                    .map_or(SALogicalKey::Unidentified(0), |key| {
                                        key.logical.clone()
                                    }),
                                location: meaning
                                    .map_or(SAKeyLocation::Standard, |key| key.location),
                                state: SAButtonState::Released,
                                repeat: false,
                                modifiers: SAModifiers::default(),
                                synthetic: true,
                            },
                            SAInputOrigin::Reconciliation,
                        );
                    }
                    release_buttons(&mut batch, target, state, scale);
                }
                push(
                    &mut batch,
                    target,
                    SAInputEvent::Focus(*focused),
                    SAInputOrigin::NativeWindow,
                );
            }
            WindowEvent::MouseCaptureLost => {
                release_buttons(&mut batch, target, state, scale);
                push(
                    &mut batch,
                    target,
                    SAInputEvent::CaptureLost,
                    SAInputOrigin::NativeWindow,
                );
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => text.set_composing(target, true)?,
                Ime::Disabled => {
                    if text.composing(target)
                        && let Some(session) = text.ime_session(target)
                    {
                        push(
                            &mut batch,
                            target,
                            SAInputEvent::Preedit {
                                session,
                                text: String::new(),
                                selection: None,
                            },
                            SAInputOrigin::NativeWindow,
                        );
                    }
                    text.set_composing(target, false)?;
                }
                Ime::Preedit(value, selection) => {
                    if let Some(session) = text.ime_session(target) {
                        push(
                            &mut batch,
                            target,
                            SAInputEvent::Preedit {
                                session,
                                text: value.clone(),
                                selection: *selection,
                            },
                            SAInputOrigin::NativeWindow,
                        );
                    }
                }
                Ime::Commit(value) => {
                    if let Some(session) = text.ime_session(target) {
                        push(
                            &mut batch,
                            target,
                            SAInputEvent::Text {
                                session,
                                text: value.clone(),
                            },
                            SAInputOrigin::NativeWindow,
                        );
                    }
                }
            },
            WindowEvent::Resized(size) => push(
                &mut batch,
                target,
                SAInputEvent::Geometry {
                    size: SAPhysicalSize {
                        width: size.width,
                        height: size.height,
                    },
                    scale,
                },
                SAInputOrigin::NativeWindow,
            ),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => push(
                &mut batch,
                target,
                SAInputEvent::Geometry {
                    size: physical_size,
                    scale: *scale_factor,
                },
                SAInputOrigin::NativeWindow,
            ),
            _ => (),
        }
        Ok(batch)
    }

    // A field-based boundary permits deterministic keyboard traces without
    // constructing winit's platform-private KeyEvent payload or inventing WM_CHAR.
    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve separate native keyboard observations."
    )]
    pub(crate) fn key_event(
        &mut self,
        target: SAWindowTarget,
        physical: PhysicalKey,
        logical: &Key,
        location: KeyLocation,
        native_state: ElementState,
        repeat: bool,
        synthetic: bool,
        committed: Option<&str>,
        state: &SAInputState,
        text: &TextState,
    ) -> Result<Vec<NormalizedInput>, SAError> {
        let physical = physical_key(physical);
        let key_state = transition(native_state);
        let mut batch = Vec::new();
        batch
            .try_reserve(2)
            .map_err(|_| SAError::AllocationFailed)?;
        if key_state == SAButtonState::Released && !state.held_keys.contains(&physical) {
            self.keys.remove(&(target, physical));
            return Ok(batch);
        }
        let logical = logical_key(logical);
        let location = key_location(location);
        if key_state == SAButtonState::Pressed {
            self.keys
                .try_reserve(1)
                .map_err(|_| SAError::AllocationFailed)?;
            self.keys.insert(
                (target, physical),
                KeyMeaning {
                    logical: logical.clone(),
                    location,
                },
            );
        } else {
            self.keys.remove(&(target, physical));
        }
        push(
            &mut batch,
            target,
            SAInputEvent::Key {
                physical,
                logical,
                location,
                state: key_state,
                repeat,
                modifiers: state.modifiers,
                synthetic,
            },
            if synthetic {
                SAInputOrigin::FocusSynthetic
            } else {
                SAInputOrigin::NativeWindow
            },
        );
        if key_state == SAButtonState::Pressed
            && !synthetic
            && !text.composing(target)
            && let Some(session) = text.active(target)
            && let Some(committed) = committed.filter(|value| !value.is_empty())
        {
            push(
                &mut batch,
                target,
                SAInputEvent::Text {
                    session,
                    text: committed.to_owned(),
                },
                SAInputOrigin::NativeWindow,
            );
        }
        Ok(batch)
    }

    /// The caller selects an eligible live target at receipt. Raw keys, buttons,
    /// wheels and companion axes are redundant with the normal window path.
    pub(crate) fn device_event(
        &self,
        selected_target: Option<SAWindowTarget>,
        event: &DeviceEvent,
        device: crate::SAInputDeviceId,
    ) -> Result<Vec<NormalizedInput>, SAError> {
        let Some(target) = selected_target else {
            return Ok(Vec::new());
        };
        let event = match event {
            DeviceEvent::MouseMotion { delta } => SAInputEvent::RelativeMotion {
                x: delta.0,
                y: delta.1,
            },
            DeviceEvent::MouseMotionAbsolute {
                position,
                virtual_desktop,
            } => SAInputEvent::RawAbsoluteMotion {
                x: position.0,
                y: position.1,
                virtual_desktop: *virtual_desktop,
            },
            _ => return Ok(Vec::new()),
        };
        let mut batch = Vec::new();
        batch
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        push(&mut batch, target, event, SAInputOrigin::RawDevice);
        batch[0].device = Some(device);
        Ok(batch)
    }

    pub(crate) fn retire(&mut self, target: SAWindowTarget) {
        self.keys.retain(|(window, _), _| *window != target);
    }

    pub(crate) fn clear(&mut self) {
        self.keys.clear();
    }
}

fn push(
    batch: &mut Vec<NormalizedInput>,
    target: SAWindowTarget,
    event: SAInputEvent,
    origin: SAInputOrigin,
) {
    batch.push(NormalizedInput {
        target,
        event,
        origin,
        source_time: None,
        device: None,
    });
}

fn release_buttons(
    batch: &mut Vec<NormalizedInput>,
    target: SAWindowTarget,
    state: &SAInputState,
    scale: f64,
) {
    for button in &state.held_buttons {
        push(
            batch,
            target,
            SAInputEvent::MouseButton {
                button: *button,
                state: SAButtonState::Released,
                position: state.position,
                scale,
            },
            SAInputOrigin::Reconciliation,
        );
    }
}

fn transition(state: ElementState) -> SAButtonState {
    match state {
        ElementState::Pressed => SAButtonState::Pressed,
        ElementState::Released => SAButtonState::Released,
    }
}

fn physical_key(key: PhysicalKey) -> SAPhysicalKey {
    match key {
        PhysicalKey::Unidentified(NativeKeyCode::Windows(code)) => {
            SAPhysicalKey::Unidentified(u32::from(code))
        }
        _ => key
            .to_scancode()
            .map_or(SAPhysicalKey::Unidentified(0), SAPhysicalKey::ScanCode),
    }
}

fn logical_key(key: &Key) -> SALogicalKey {
    match key {
        Key::Character(value) => SALogicalKey::Character(value.to_string()),
        Key::Dead(value) => SALogicalKey::Dead(*value),
        Key::Named(value) => SALogicalKey::Named(named_key(*value)),
        Key::Unidentified(NativeKey::Windows(value)) => {
            SALogicalKey::Unidentified(u32::from(*value))
        }
        Key::Unidentified(_) => SALogicalKey::Unidentified(0),
    }
}

fn key_location(location: KeyLocation) -> SAKeyLocation {
    match location {
        KeyLocation::Standard => SAKeyLocation::Standard,
        KeyLocation::Left => SAKeyLocation::Left,
        KeyLocation::Right => SAKeyLocation::Right,
        KeyLocation::Numpad => SAKeyLocation::Numpad,
    }
}

fn named_key(key: NamedKey) -> SANamedKey {
    use NamedKey as N;
    use SANamedKey as S;
    match key {
        N::Escape => S::Escape,
        N::Enter => S::Enter,
        N::Tab => S::Tab,
        N::Space => S::Space,
        N::Backspace => S::Backspace,
        N::Delete => S::Delete,
        N::Insert => S::Insert,
        N::Home => S::Home,
        N::End => S::End,
        N::PageUp => S::PageUp,
        N::PageDown => S::PageDown,
        N::ArrowLeft => S::ArrowLeft,
        N::ArrowRight => S::ArrowRight,
        N::ArrowUp => S::ArrowUp,
        N::ArrowDown => S::ArrowDown,
        N::Shift => S::Shift,
        N::Control => S::Control,
        N::Alt => S::Alt,
        N::AltGraph => S::AltGraph,
        N::Super => S::Super,
        N::CapsLock => S::CapsLock,
        N::NumLock => S::NumLock,
        N::ScrollLock => S::ScrollLock,
        N::ContextMenu => S::ContextMenu,
        N::PrintScreen => S::PrintScreen,
        N::Pause => S::Pause,
        N::F1 => S::Function(1),
        N::F2 => S::Function(2),
        N::F3 => S::Function(3),
        N::F4 => S::Function(4),
        N::F5 => S::Function(5),
        N::F6 => S::Function(6),
        N::F7 => S::Function(7),
        N::F8 => S::Function(8),
        N::F9 => S::Function(9),
        N::F10 => S::Function(10),
        N::F11 => S::Function(11),
        N::F12 => S::Function(12),
        N::F13 => S::Function(13),
        N::F14 => S::Function(14),
        N::F15 => S::Function(15),
        N::F16 => S::Function(16),
        N::F17 => S::Function(17),
        N::F18 => S::Function(18),
        N::F19 => S::Function(19),
        N::F20 => S::Function(20),
        N::F21 => S::Function(21),
        N::F22 => S::Function(22),
        N::F23 => S::Function(23),
        N::F24 => S::Function(24),
        N::F25 => S::Function(25),
        N::F26 => S::Function(26),
        N::F27 => S::Function(27),
        N::F28 => S::Function(28),
        N::F29 => S::Function(29),
        N::F30 => S::Function(30),
        N::F31 => S::Function(31),
        N::F32 => S::Function(32),
        N::F33 => S::Function(33),
        N::F34 => S::Function(34),
        N::F35 => S::Function(35),
        _ => S::Other(format!("{key:?}")),
    }
}

fn mouse_button(button: MouseButton) -> SAMouseButton {
    match button {
        MouseButton::Left => SAMouseButton::Left,
        MouseButton::Right => SAMouseButton::Right,
        MouseButton::Middle => SAMouseButton::Middle,
        MouseButton::Back => SAMouseButton::Back,
        MouseButton::Forward => SAMouseButton::Forward,
        MouseButton::Other(value) => SAMouseButton::Other(value),
    }
}

fn scroll_delta(delta: MouseScrollDelta) -> SAScrollDelta {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => SAScrollDelta::Lines {
            x: f64::from(x),
            y: f64::from(y),
        },
        MouseScrollDelta::PixelDelta(position) => SAScrollDelta::Pixels {
            x: position.x,
            y: position.y,
        },
    }
}

#[cfg(test)]
#[path = "../../tests/unit/input_translation.rs"]
mod tests;
