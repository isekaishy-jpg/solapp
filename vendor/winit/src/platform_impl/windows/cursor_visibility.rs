//! Solapp modification: balance one visibility contribution per owning GUI thread.
//!
//! Stored window requests do not themselves establish client/capture authority.

#[derive(Clone, Copy, Default)]
pub(super) struct CursorVisibility {
    owner: Option<isize>,
    hidden: bool,
}

impl CursorVisibility {
    /// Returns a new hidden contribution only when the native counter must change.
    pub(super) fn refresh(
        &mut self,
        window: isize,
        in_client: bool,
        hidden: bool,
        claim_client: bool,
        capture: Option<isize>,
    ) -> Option<bool> {
        let controls = match capture {
            Some(captured) => captured == window,
            None => in_client && (claim_client || self.owner == Some(window)),
        };
        if controls {
            self.owner = Some(window);
            self.set_hidden(hidden)
        } else {
            self.release(window)
        }
    }

    pub(super) fn release(&mut self, window: isize) -> Option<bool> {
        if self.owner == Some(window) {
            self.clear()
        } else {
            None
        }
    }

    pub(super) fn clear(&mut self) -> Option<bool> {
        self.owner = None;
        self.set_hidden(false)
    }

    fn set_hidden(&mut self, hidden: bool) -> Option<bool> {
        if self.hidden == hidden {
            None
        } else {
            self.hidden = hidden;
            Some(hidden)
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/windows_cursor_visibility.rs"]
mod tests;
