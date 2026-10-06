//! Deterministic private backend with owner-thread acquisition/retirement trace.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::thread::{self, ThreadId};

use crate::backend::NativeWindowKey;
use crate::error::{SAError, SANativeOperation};
use crate::window::SAWindowSpec;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Trace {
    Created(String),
    DestroyRequested(String),
    Destroyed(String),
    CursorVisibility(String, bool),
}

#[derive(Default)]
pub(crate) struct TestBackend {
    pub(crate) trace: Rc<RefCell<Vec<Trace>>>,
    pub(crate) fail_title: Option<String>,
    pub(crate) fail_raw: bool,
    pub(crate) fail_confine: bool,
    pub(crate) fail_next_observation: bool,
    pending: Rc<RefCell<Vec<(u64, String)>>>,
    next_id: u64,
}

impl TestBackend {
    pub(crate) fn raw_input(&mut self, _: crate::SARawInputPolicy) -> Result<(), SAError> {
        if self.fail_raw {
            Err(SAError::Native {
                operation: SANativeOperation::RegisterRawInput,
                message: String::from("injected registration failure"),
            })
        } else {
            Ok(())
        }
    }
    pub(crate) fn create_window(&mut self, spec: &SAWindowSpec) -> Result<TestWindow, SAError> {
        if self.fail_title.as_ref() == Some(&spec.title) {
            return Err(SAError::Native {
                operation: SANativeOperation::CreateWindow,
                message: String::from("injected native acquisition failure"),
            });
        }
        self.trace
            .borrow_mut()
            .push(Trace::Created(spec.title.clone()));
        self.next_id += 1;
        Ok(TestWindow {
            id: self.next_id,
            title: spec.title.clone(),
            trace: Rc::clone(&self.trace),
            pending: Rc::clone(&self.pending),
            owner: thread::current().id(),
            already_destroyed: false,
            display: Cell::new(crate::SADisplayObserved {
                backend_mode: crate::SADisplayMode::Windowed,
                geometry: crate::SADisplayGeometry {
                    position: Some(crate::SAScreenPosition { x: 0, y: 0 }),
                    size: spec.size,
                    scale: crate::SAScaleFactor::new(1.0).unwrap(),
                    minimized: Some(false),
                    maximized: false,
                },
            }),
            fail_display: Cell::new(false),
            fail_observation: Cell::new(std::mem::take(&mut self.fail_next_observation)),
            fail_confine: self.fail_confine,
            native_alive: Cell::new(true),
        })
    }

    pub(crate) fn complete_destruction(&mut self) -> Option<NativeWindowKey> {
        self.pending.borrow_mut().pop().map(|(id, title)| {
            self.trace.borrow_mut().push(Trace::Destroyed(title));
            NativeWindowKey::Test(id)
        })
    }

    pub(crate) fn complete_destruction_of(
        &mut self,
        key: NativeWindowKey,
    ) -> Option<NativeWindowKey> {
        let NativeWindowKey::Test(id) = key else {
            return None;
        };
        let mut pending = self.pending.borrow_mut();
        let index = pending.iter().position(|(current, _)| *current == id)?;
        let (_, title) = pending.remove(index);
        self.trace.borrow_mut().push(Trace::Destroyed(title));
        Some(key)
    }
}

pub(crate) struct TestWindow {
    pub(crate) native_alive: Cell<bool>,
    pub(crate) display: Cell<crate::SADisplayObserved>,
    pub(crate) fail_display: Cell<bool>,
    pub(crate) fail_observation: Cell<bool>,
    pub(crate) fail_confine: bool,
    pub(crate) id: u64,
    pub(crate) already_destroyed: bool,
    title: String,
    trace: Rc<RefCell<Vec<Trace>>>,
    owner: ThreadId,
    pending: Rc<RefCell<Vec<(u64, String)>>>,
}

impl TestWindow {
    pub(crate) fn cursor_visible(&self, visible: bool) {
        assert!(
            self.native_alive.get(),
            "native cursor operation on destroyed target"
        );
        self.trace
            .borrow_mut()
            .push(Trace::CursorVisibility(self.title.clone(), visible));
    }
    pub(crate) fn confine(&self, _: bool) -> Result<(), SAError> {
        if self.fail_confine {
            Err(SAError::Native {
                operation: SANativeOperation::ConfinePointer,
                message: String::from("injected confinement failure"),
            })
        } else {
            Ok(())
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        assert_eq!(thread::current().id(), self.owner);
        if self.already_destroyed {
            return;
        }
        self.trace
            .borrow_mut()
            .push(Trace::DestroyRequested(self.title.clone()));
        self.pending
            .borrow_mut()
            .push((self.id, self.title.clone()));
    }
}
