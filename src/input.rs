//! Owned normalized input and Forever-style scoped delivery gating.

use crate::{SAError, SAPhysicalSize, SARawTime, SAWindowTarget};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

/// Position in physical client pixels, preserved at event receipt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SAPhysicalPosition {
    /// Horizontal coordinate in physical client pixels.
    pub x: f64,
    /// Vertical coordinate in physical client pixels.
    pub y: f64,
}

/// Physical keyboard identity; scan codes keep unknown keys bindable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SAPhysicalKey {
    /// Native extended scan code; interpretation belongs to the platform adapter.
    ScanCode(u32),
    /// An unidentified native code without invented semantic identity.
    Unidentified(u32),
}

/// Named logical key meanings, independent of physical binding identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SANamedKey {
    /// Escape.
    Escape,
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Space.
    Space,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Shift.
    Shift,
    /// Control.
    Control,
    /// Alt.
    Alt,
    /// Alternate graphics modifier.
    AltGraph,
    /// Super/Windows modifier.
    Super,
    /// Caps lock.
    CapsLock,
    /// Num lock.
    NumLock,
    /// Scroll lock.
    ScrollLock,
    /// Context menu.
    ContextMenu,
    /// Print screen.
    PrintScreen,
    /// Pause.
    Pause,
    /// Numbered function key, preserving the backend's observed number.
    Function(u8),
    /// Other backend semantic name; the string is not a stable binding code.
    Other(String),
}

/// Logical keyboard meaning. Character identity is not committed text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SALogicalKey {
    /// Unicode character meaning under the current layout.
    Character(String),
    /// Named semantic meaning.
    Named(SANamedKey),
    /// Dead key, optionally identifying its combining character.
    Dead(Option<char>),
    /// Unidentified native logical code.
    Unidentified(u32),
}

/// Physical keyboard location as observed by the backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAKeyLocation {
    /// No left/right/numpad distinction.
    Standard,
    /// Left modifier or key.
    Left,
    /// Right modifier or key.
    Right,
    /// Numeric keypad.
    Numpad,
}

/// Modifier state associated with receipt, not a later global query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SAModifiers {
    /// Shift is held.
    pub shift: bool,
    /// Control is held.
    pub control: bool,
    /// Alt is held.
    pub alt: bool,
    /// Super/Windows is held.
    pub super_key: bool,
}

/// A key or button transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAButtonState {
    /// Pressed or repeated press.
    Pressed,
    /// Released.
    Released,
}

/// Mouse button identity; extended unknown buttons retain their native number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SAMouseButton {
    /// Left button.
    Left,
    /// Right button.
    Right,
    /// Middle button.
    Middle,
    /// Back button.
    Back,
    /// Forward button.
    Forward,
    /// Other observed native button number.
    Other(u16),
}

/// Scroll axes and their original units, without integer truncation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SAScrollDelta {
    /// Fractional line units.
    Lines {
        /// Horizontal amount.
        x: f64,
        /// Vertical amount.
        y: f64,
    },
    /// Physical pixel units.
    Pixels {
        /// Horizontal amount.
        x: f64,
        /// Vertical amount.
        y: f64,
    },
}

/// Origin distinguishes native facts from synthetic reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAInputOrigin {
    /// Normal native window event.
    NativeWindow,
    /// Raw device input.
    RawDevice,
    /// Backend-generated focus synthesis.
    FocusSynthetic,
    /// SA-generated held-state reconciliation.
    Reconciliation,
}

/// An owned normalized event. Native translation is a later adapter capability.
#[derive(Clone, Debug, PartialEq)]
pub enum SAInputEvent {
    /// Physical and logical keyboard identity, separately from text.
    Key {
        /// Physical scan-code identity.
        physical: SAPhysicalKey,
        /// Current layout meaning.
        logical: SALogicalKey,
        /// Physical key location.
        location: SAKeyLocation,
        /// Press/release state.
        state: SAButtonState,
        /// Native repeat flag.
        repeat: bool,
        /// Receipt-time modifiers.
        modifiers: SAModifiers,
        /// Whether the backend synthesized this transition.
        synthetic: bool,
    },
    /// Absolute physical client coordinates.
    PointerMoved(SAPhysicalPosition),
    /// Raw relative device movement, not a client coordinate or camera policy.
    RelativeMotion {
        /// Horizontal relative movement.
        x: f64,
        /// Vertical relative movement.
        y: f64,
    },
    /// Raw Windows absolute normalized coordinates, not pixel coordinates.
    RawAbsoluteMotion {
        /// Observed horizontal normalized units, unclamped.
        x: i32,
        /// Observed vertical normalized units, unclamped.
        y: i32,
        /// Whether coordinates refer to the virtual desktop.
        virtual_desktop: bool,
    },
    /// Button transition retaining its event-associated pointer position.
    MouseButton {
        /// Observed button identity.
        button: SAMouseButton,
        /// Press/release state.
        state: SAButtonState,
        /// Last received client position, absent before an absolute observation.
        position: Option<SAPhysicalPosition>,
        /// Receipt-time native scale.
        scale: f64,
    },
    /// Scroll transition retaining axes, units and event-associated position.
    Wheel {
        /// Original fractional scroll axes and units.
        delta: SAScrollDelta,
        /// Last received client position when available.
        position: Option<SAPhysicalPosition>,
        /// Receipt-time native scale.
        scale: f64,
    },
    /// Native window focus observation.
    Focus(bool),
    /// Actual native capture loss, distinct from focus loss.
    CaptureLost,
    /// Receipt-time modifier observation.
    Modifiers(SAModifiers),
    /// Owned committed Unicode text for an exact text-target incarnation.
    Text {
        /// Session active at native receipt; never retargeted during delivery.
        session: crate::SATextSessionId,
        /// Committed Unicode text.
        text: String,
    },
    /// Owned preedit Unicode and checked UTF-8 byte selection, separate from commit.
    Preedit {
        /// Session owning this native composition.
        session: crate::SATextSessionId,
        /// Current preedit contents.
        text: String,
        /// Optional UTF-8 byte selection, on character boundaries.
        selection: Option<(usize, usize)>,
    },
    /// Native geometry observation; scale must be positive and finite.
    Geometry {
        /// Physical client size.
        size: SAPhysicalSize,
        /// Native scale observed for associated geometry/input.
        scale: f64,
    },
}

/// Honest receipt/delivery timestamps; no native occurrence time is fabricated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAInputStamp {
    /// Host-monotonic receipt order.
    pub sequence: u64,
    /// Host raw time when SA received the record.
    pub receipt: SARawTime,
    /// Host raw time when application delivery began, absent while deferred.
    pub delivery: Option<SARawTime>,
    /// Native source time only when the backend actually supplies one.
    pub source_time: Option<Duration>,
}

/// An opaque raw-device observation within one host. Native device identity may
/// repeat across reconnects; this token makes no nonreuse or hardware guarantee.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAInputDeviceId {
    pub(crate) host: crate::SAHostId,
    pub(crate) native: winit::event::DeviceId,
}
impl SAInputDeviceId {
    /// The host at which this native identity was observed.
    pub fn host(self) -> crate::SAHostId {
        self.host
    }
}

/// Fully owned deferred input. Position and text belong to this record.
#[derive(Clone, Debug, PartialEq)]
pub struct SAInputRecord {
    /// Observed native raw-device identity; ordinary window records have none.
    pub device: Option<SAInputDeviceId>,
    /// Exact native window generation at receipt.
    pub target: SAWindowTarget,
    /// Separate receipt and delivery stamps.
    pub stamp: SAInputStamp,
    /// Native/synthetic origin.
    pub origin: SAInputOrigin,
    /// Owned normalized payload.
    pub event: SAInputEvent,
}

/// Explicit state layer; platform receipt can lead application delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SAInputStateLayer {
    /// Facts already received from the backend.
    Platform,
    /// Facts applied immediately before application handlers.
    Delivered,
}

/// A window generation's input state in one declared layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SAInputState {
    /// Currently held physical keys.
    pub held_keys: Vec<SAPhysicalKey>,
    /// Currently held mouse buttons.
    pub held_buttons: Vec<SAMouseButton>,
    /// Last absolute client position.
    pub position: Option<SAPhysicalPosition>,
    /// Latest native focus observation.
    pub focused: bool,
    /// Latest observed modifiers.
    pub modifiers: SAModifiers,
    /// Most recent native scale, absent before geometry/associated input.
    pub scale: Option<f64>,
}

impl SAInputState {
    fn apply(&mut self, event: &SAInputEvent) -> Result<(), SAError> {
        match event {
            SAInputEvent::Key {
                physical,
                state,
                modifiers,
                ..
            } => {
                set_held(&mut self.held_keys, *physical, *state)?;
                self.modifiers = *modifiers;
            }
            SAInputEvent::MouseButton {
                button,
                state,
                position,
                scale,
            } => {
                set_held(&mut self.held_buttons, *button, *state)?;
                self.position = *position;
                self.scale = Some(*scale);
            }
            SAInputEvent::PointerMoved(position) => self.position = Some(*position),
            SAInputEvent::Wheel {
                position, scale, ..
            } => {
                self.position = *position;
                self.scale = Some(*scale);
            }
            SAInputEvent::Focus(focused) => {
                self.focused = *focused;
                if !focused {
                    self.held_keys.clear();
                    self.held_buttons.clear();
                    self.modifiers = SAModifiers::default();
                }
            }
            SAInputEvent::CaptureLost => self.held_buttons.clear(),
            SAInputEvent::Modifiers(modifiers) => self.modifiers = *modifiers,
            SAInputEvent::Geometry { scale, .. } => self.scale = Some(*scale),
            _ => (),
        }
        Ok(())
    }
}
fn set_held<T: Copy + PartialEq>(
    values: &mut Vec<T>,
    value: T,
    state: SAButtonState,
) -> Result<(), SAError> {
    match state {
        SAButtonState::Pressed if !values.contains(&value) => {
            values
                .try_reserve(1)
                .map_err(|_| SAError::AllocationFailed)?;
            values.push(value);
        }
        SAButtonState::Released => values.retain(|held| *held != value),
        _ => (),
    }
    Ok(())
}

/// One bounded input drain's observation; leftover records retain their order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SAInputDrainReport {
    /// Records consumed, including stale-generation discards.
    pub consumed: usize,
    /// Records still queued after this visit.
    pub remaining: usize,
}

pub(crate) struct Input {
    pub(crate) enabled: bool,
    pub(crate) queue: VecDeque<SAInputRecord>,
    next_sequence: u64,
    #[cfg(test)]
    pub(crate) fail_next_queue_reservation: bool,
    #[cfg(test)]
    pub(crate) fail_next_delivery_reservation: bool,
    platform: HashMap<SAWindowTarget, SAInputState>,
    delivered: HashMap<SAWindowTarget, SAInputState>,
}
impl Input {
    pub(crate) fn new() -> Self {
        Self {
            enabled: true,
            queue: VecDeque::new(),
            next_sequence: 1,
            #[cfg(test)]
            fail_next_queue_reservation: false,
            #[cfg(test)]
            fail_next_delivery_reservation: false,
            platform: HashMap::new(),
            delivered: HashMap::new(),
        }
    }
    pub(crate) fn state(
        &self,
        target: SAWindowTarget,
        layer: SAInputStateLayer,
    ) -> Option<&SAInputState> {
        match layer {
            SAInputStateLayer::Platform => self.platform.get(&target),
            SAInputStateLayer::Delivered => self.delivered.get(&target),
        }
    }
    pub(crate) fn receive(
        &mut self,
        target: SAWindowTarget,
        event: SAInputEvent,
        origin: SAInputOrigin,
        receipt: SARawTime,
        source_time: Option<Duration>,
        device: Option<SAInputDeviceId>,
    ) -> Result<(), SAError> {
        validate(&event)?;
        let sequence = self.next_sequence;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or(SAError::IdentityExhausted(crate::SAIdentityKind::Input))?;
        self.platform
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        self.platform.entry(target).or_default().apply(&event)?;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_queue_reservation) {
            return Err(SAError::AllocationFailed);
        }
        self.queue
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        self.queue.push_back(SAInputRecord {
            device,
            target,
            stamp: SAInputStamp {
                sequence,
                receipt,
                delivery: None,
                source_time,
            },
            origin,
            event,
        });
        Ok(())
    }
    pub(crate) fn deliver(
        &mut self,
        record: &mut SAInputRecord,
        time: SARawTime,
    ) -> Result<(), SAError> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_delivery_reservation) {
            return Err(SAError::AllocationFailed);
        }
        self.delivered
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        self.delivered
            .entry(record.target)
            .or_default()
            .apply(&record.event)?;
        record.stamp.delivery = Some(time);
        Ok(())
    }
    pub(crate) fn retire(&mut self, target: SAWindowTarget) {
        self.queue.retain(|record| record.target != target);
        self.platform.remove(&target);
        self.delivered.remove(&target);
    }

    pub(crate) fn retire_all(&mut self) {
        self.queue.clear();
        self.platform.clear();
        self.delivered.clear();
    }
}
fn validate(event: &SAInputEvent) -> Result<(), SAError> {
    let finite = |x: f64, y: f64| x.is_finite() && y.is_finite();
    let position_valid = |position: Option<SAPhysicalPosition>| {
        position.is_none_or(|position| finite(position.x, position.y))
    };
    let scale_valid = |scale: f64| scale.is_finite() && scale > 0.0;
    let valid = match event {
        SAInputEvent::PointerMoved(position) => finite(position.x, position.y),
        SAInputEvent::RelativeMotion { x, y } => finite(*x, *y),
        SAInputEvent::MouseButton {
            position, scale, ..
        } => position_valid(*position) && scale_valid(*scale),
        SAInputEvent::Wheel {
            delta,
            position,
            scale,
        } => {
            let (SAScrollDelta::Lines { x, y } | SAScrollDelta::Pixels { x, y }) = delta;
            finite(*x, *y) && position_valid(*position) && scale_valid(*scale)
        }
        SAInputEvent::Geometry { scale, .. } => scale_valid(*scale),
        SAInputEvent::Preedit {
            text, selection, ..
        } => selection.is_none_or(|(start, end)| {
            start <= end && text.is_char_boundary(start) && text.is_char_boundary(end)
        }),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(SAError::InvalidInput(
            "invalid normalized input coordinates, scale or text selection",
        ))
    }
}
