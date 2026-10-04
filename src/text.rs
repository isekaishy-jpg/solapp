//! Owner-local text sessions, separate from window and input identities.

use std::collections::HashMap;

use crate::{SAError, SAIdentityKind, SAPhysicalPosition, SAPhysicalSize, SAWindowTarget};

/// Exact text-target incarnation. Ending or replacing it invalidates queued text.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SATextSessionId {
    target: SAWindowTarget,
    serial: u64,
}

impl SATextSessionId {
    /// The native window generation to which this text session belongs.
    pub fn target(self) -> SAWindowTarget {
        self.target
    }
}

/// Application-supplied caret rectangle in physical client pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SATextCaret {
    /// Rectangle origin. SA does not transform application text layout.
    pub position: SAPhysicalPosition,
    /// Rectangle extent in physical pixels, allowing an empty caret extent.
    pub size: SAPhysicalSize,
}

impl SATextCaret {
    pub(crate) fn validate(self) -> Result<(), SAError> {
        if !self.position.x.is_finite() || !self.position.y.is_finite() {
            return Err(SAError::InvalidInput("text caret position is not finite"));
        }
        // The Windows backend rounds physical positions into i32 and adds the
        // extent to form an IMM RECT. Reject narrowing or rectangle overflow.
        for (position, extent) in [
            (self.position.x, self.size.width),
            (self.position.y, self.size.height),
        ] {
            let rounded = position.round();
            if rounded < f64::from(i32::MIN)
                || rounded > f64::from(i32::MAX)
                || i32::try_from(extent)
                    .ok()
                    .and_then(|extent| (rounded as i32).checked_add(extent))
                    .is_none()
            {
                return Err(SAError::InvalidInput(
                    "text caret is outside the native rectangle range",
                ));
            }
        }
        Ok(())
    }
}

struct Session {
    id: SATextSessionId,
    caret: SATextCaret,
}

pub(crate) struct TextState {
    next_serial: u64,
    sessions: HashMap<SAWindowTarget, Session>,
    // Native composition retains its original owner until Ime::Disabled, even
    // if application text focus is replaced while native events are queued.
    compositions: HashMap<SAWindowTarget, Option<SATextSessionId>>,
}

impl TextState {
    pub(crate) fn new() -> Self {
        Self {
            next_serial: 1,
            sessions: HashMap::new(),
            compositions: HashMap::new(),
        }
    }

    pub(crate) fn begin(
        &mut self,
        target: SAWindowTarget,
        caret: SATextCaret,
    ) -> Result<SATextSessionId, SAError> {
        caret.validate()?;
        let next = self
            .next_serial
            .checked_add(1)
            .ok_or(SAError::IdentityExhausted(SAIdentityKind::TextSession))?;
        self.sessions
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        let id = SATextSessionId {
            target,
            serial: self.next_serial,
        };
        self.sessions.insert(target, Session { id, caret });
        self.next_serial = next;
        Ok(id)
    }

    pub(crate) fn update(
        &mut self,
        id: SATextSessionId,
        caret: SATextCaret,
    ) -> Result<(), SAError> {
        caret.validate()?;
        let session = self
            .sessions
            .get_mut(&id.target)
            .filter(|session| session.id == id)
            .ok_or(SAError::StaleIdentity)?;
        session.caret = caret;
        Ok(())
    }

    pub(crate) fn end(&mut self, id: SATextSessionId) -> Result<(), SAError> {
        if !self.is_live(id) {
            return Err(SAError::StaleIdentity);
        }
        self.sessions.remove(&id.target);
        Ok(())
    }

    pub(crate) fn active(&self, target: SAWindowTarget) -> Option<SATextSessionId> {
        self.sessions.get(&target).map(|session| session.id)
    }

    pub(crate) fn caret(&self, id: SATextSessionId) -> Result<SATextCaret, SAError> {
        self.sessions
            .get(&id.target)
            .filter(|session| session.id == id)
            .map(|session| session.caret)
            .ok_or(SAError::StaleIdentity)
    }

    pub(crate) fn is_live(&self, id: SATextSessionId) -> bool {
        self.active(id.target) == Some(id)
    }

    pub(crate) fn composing(&self, target: SAWindowTarget) -> bool {
        self.compositions.contains_key(&target)
    }

    pub(crate) fn ime_session(&self, target: SAWindowTarget) -> Option<SATextSessionId> {
        self.compositions
            .get(&target)
            .copied()
            .unwrap_or_else(|| self.active(target))
    }

    pub(crate) fn set_composing(
        &mut self,
        target: SAWindowTarget,
        composing: bool,
    ) -> Result<(), SAError> {
        if composing {
            if !self.compositions.contains_key(&target) {
                self.compositions
                    .try_reserve(1)
                    .map_err(|_| SAError::AllocationFailed)?;
                self.compositions.insert(target, self.active(target));
            }
        } else {
            self.compositions.remove(&target);
        }
        Ok(())
    }

    pub(crate) fn retire(&mut self, target: SAWindowTarget) {
        self.sessions.remove(&target);
        self.compositions.remove(&target);
    }

    pub(crate) fn clear(&mut self) {
        self.sessions.clear();
        self.compositions.clear();
    }
}

#[cfg(test)]
#[path = "../tests/unit/text.rs"]
mod tests;
