//! Host-scoped checked identities. Native handles never serve as identifiers.

use crate::error::{SAError, SAIdentityKind};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_HOST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SlotKey {
    pub(crate) index: u32,
    pub(crate) generation: u64,
}

/// Recipient incarnation changes whenever its retired storage is reused.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SARecipientGeneration(u64);

/// Host-scoped recipient token; every operation checks its incarnation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SARecipientId {
    pub(crate) host: SAHostId,
    pub(crate) key: SlotKey,
}
impl SARecipientId {
    /// The issuing host.
    pub fn host(self) -> SAHostId {
        self.host
    }
    /// The exact recipient incarnation.
    pub fn generation(self) -> SARecipientGeneration {
        SARecipientGeneration(self.key.generation)
    }
}

/// Host-scoped subscription token with checked slot reuse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SASubscriptionId {
    pub(crate) host: SAHostId,
    pub(crate) key: SlotKey,
}

/// Host-scoped one-shot timer token with checked slot reuse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SATimerId {
    pub(crate) host: SAHostId,
    pub(crate) key: SlotKey,
}

/// Monotonic host-scoped identity of an owned post receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAPostId {
    pub(crate) host: SAHostId,
    pub(crate) sequence: u64,
}

pub(crate) struct Arena<T> {
    entries: Vec<Slot<T>>,
    kind: SAIdentityKind,
}
impl<T> Arena<T> {
    pub(crate) fn new(kind: SAIdentityKind) -> Self {
        Self {
            entries: Vec::new(),
            kind,
        }
    }
    pub(crate) fn reserve(&mut self) -> Result<SlotKey, SAError> {
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.value.is_none()
                && let Some(generation) = entry.incarnation
            {
                return Ok(SlotKey {
                    index: index as u32,
                    generation,
                });
            }
        }
        let index =
            u32::try_from(self.entries.len()).map_err(|_| SAError::IdentityExhausted(self.kind))?;
        self.entries
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        self.entries.push(Slot {
            incarnation: Some(1),
            value: None,
        });
        Ok(SlotKey {
            index,
            generation: 1,
        })
    }
    pub(crate) fn insert(&mut self, key: SlotKey, value: T) {
        self.entries[key.index as usize].value = Some(value);
    }
    pub(crate) fn get(&self, key: SlotKey) -> Option<&T> {
        self.entries
            .get(key.index as usize)
            .filter(|slot| slot.incarnation == Some(key.generation))?
            .value
            .as_ref()
    }
    pub(crate) fn get_mut(&mut self, key: SlotKey) -> Option<&mut T> {
        self.entries
            .get_mut(key.index as usize)
            .filter(|slot| slot.incarnation == Some(key.generation))?
            .value
            .as_mut()
    }
    pub(crate) fn remove(&mut self, key: SlotKey) -> Option<T> {
        let slot = self
            .entries
            .get_mut(key.index as usize)
            .filter(|slot| slot.incarnation == Some(key.generation))?;
        let value = slot.value.take()?;
        slot.incarnation = slot
            .incarnation
            .and_then(|generation| generation.checked_add(1));
        Some(value)
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (SlotKey, &T)> {
        self.entries.iter().enumerate().filter_map(|(index, slot)| {
            Some((
                SlotKey {
                    index: index as u32,
                    generation: slot.incarnation?,
                },
                slot.value.as_ref()?,
            ))
        })
    }
}

/// Process-local host identity. Values never wrap or silently repeat.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAHostId(u64);

impl SAHostId {
    pub(crate) fn allocate() -> Result<Self, SAError> {
        // Only uniqueness is shared; no host state is published by this counter.
        NEXT_HOST
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map(Self)
            .map_err(|_| SAError::IdentityExhausted(SAIdentityKind::Host))
    }
}

/// Stable window slot identity, including host and slot incarnation.
/// It is distinct from the native-window generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAWindowId {
    host: SAHostId,
    slot: u32,
    incarnation: u64,
}

impl SAWindowId {
    /// The host that issued this identity.
    pub fn host(self) -> SAHostId {
        self.host
    }
}

/// A native window generation; independent of slot reuse and renderer state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAWindowGeneration(u64);

impl SAWindowGeneration {
    pub(crate) const INITIAL: Self = Self(1);
}

/// A command target binds stable identity to its expected native generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SAWindowTarget {
    /// The stable, host-scoped window identity.
    pub id: SAWindowId,
    /// The exact native generation to which the command applies.
    pub generation: SAWindowGeneration,
}

struct Slot<T> {
    incarnation: Option<u64>,
    value: Option<T>,
}

pub(crate) struct WindowSlots<T> {
    host: SAHostId,
    slots: Vec<WindowSlot<T>>,
}

struct WindowSlot<T> {
    incarnation: Option<u64>,
    state: WindowSlotState<T>,
}

enum WindowSlotState<T> {
    Vacant,
    Reserved,
    Occupied(T),
}

impl<T> WindowSlot<T> {
    fn value(&self) -> Option<&T> {
        match &self.state {
            WindowSlotState::Occupied(value) => Some(value),
            _ => None,
        }
    }

    fn value_mut(&mut self) -> Option<&mut T> {
        match &mut self.state {
            WindowSlotState::Occupied(value) => Some(value),
            _ => None,
        }
    }

    fn retire(&mut self) -> WindowSlotState<T> {
        self.incarnation = self.incarnation.and_then(|value| value.checked_add(1));
        std::mem::replace(&mut self.state, WindowSlotState::Vacant)
    }
}

impl<T> WindowSlots<T> {
    pub(crate) fn new(host: SAHostId) -> Self {
        Self {
            host,
            slots: Vec::new(),
        }
    }

    // Reserve before native construction: acquired resources always have room
    // for their retirement record. A failed initialization stays reserved until
    // its native destruction is acknowledged.
    pub(crate) fn reserve(&mut self) -> Result<SAWindowId, SAError> {
        for (slot, entry) in self.slots.iter_mut().enumerate() {
            if matches!(entry.state, WindowSlotState::Vacant)
                && let Some(incarnation) = entry.incarnation
            {
                entry.state = WindowSlotState::Reserved;
                return Ok(SAWindowId {
                    host: self.host,
                    slot: slot as u32,
                    incarnation,
                });
            }
        }
        let slot = u32::try_from(self.slots.len())
            .map_err(|_| SAError::IdentityExhausted(SAIdentityKind::Window))?;
        self.slots
            .try_reserve(1)
            .map_err(|_| SAError::AllocationFailed)?;
        self.slots.push(WindowSlot {
            incarnation: Some(1),
            state: WindowSlotState::Reserved,
        });
        Ok(SAWindowId {
            host: self.host,
            slot,
            incarnation: 1,
        })
    }

    pub(crate) fn insert(&mut self, id: SAWindowId, value: T) {
        assert_eq!(id.host, self.host);
        let entry = &mut self.slots[id.slot as usize];
        assert_eq!(entry.incarnation, Some(id.incarnation));
        assert!(matches!(entry.state, WindowSlotState::Reserved));
        entry.state = WindowSlotState::Occupied(value);
    }

    // Releases only this exact reservation, never an occupied or reused slot.
    pub(crate) fn cancel_reservation(&mut self, id: SAWindowId) -> bool {
        if id.host != self.host {
            return false;
        }
        let Some(entry) = self.slots.get_mut(id.slot as usize) else {
            return false;
        };
        if entry.incarnation != Some(id.incarnation)
            || !matches!(entry.state, WindowSlotState::Reserved)
        {
            return false;
        }
        entry.retire();
        true
    }

    pub(crate) fn get(&self, id: SAWindowId) -> Result<&T, SAError> {
        if id.host != self.host {
            return Err(SAError::ForeignHost);
        }
        let entry = self
            .slots
            .get(id.slot as usize)
            .ok_or(SAError::StaleIdentity)?;
        if entry.incarnation != Some(id.incarnation) {
            return Err(SAError::StaleIdentity);
        }
        entry.value().ok_or(SAError::StaleIdentity)
    }

    pub(crate) fn remove_where(&mut self, mut predicate: impl FnMut(&T) -> bool) -> Option<T> {
        for slot in &mut self.slots {
            if slot.value().is_some_and(&mut predicate)
                && let WindowSlotState::Occupied(value) = slot.retire()
            {
                return Some(value);
            }
        }
        None
    }

    pub(crate) fn get_mut(&mut self, id: SAWindowId) -> Result<&mut T, SAError> {
        self.get(id)?;
        Ok(self.slots[id.slot as usize].value_mut().unwrap())
    }

    pub(crate) fn remove(&mut self, id: SAWindowId) -> Result<T, SAError> {
        self.get(id)?;
        let entry = &mut self.slots[id.slot as usize];
        let WindowSlotState::Occupied(value) = entry.retire() else {
            unreachable!("checked occupied window slot")
        };
        Ok(value)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (SAWindowId, &T)> {
        self.slots.iter().enumerate().filter_map(|(slot, entry)| {
            Some((
                SAWindowId {
                    host: self.host,
                    slot: slot as u32,
                    incarnation: entry.incarnation?,
                },
                entry.value()?,
            ))
        })
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = (SAWindowId, &mut T)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(slot, entry)| {
                Some((
                    SAWindowId {
                        host: self.host,
                        slot: slot as u32,
                        incarnation: entry.incarnation?,
                    },
                    entry.value_mut()?,
                ))
            })
    }

    #[cfg(test)]
    pub(crate) fn retire_all(&mut self) {
        while self.remove_where(|_| true).is_some() {}
    }
}

#[cfg(test)]
#[path = "../tests/unit/identity.rs"]
mod tests;
