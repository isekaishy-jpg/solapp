use super::*;
use crate::SAWindowSpec;
use crate::backend::test::{TestBackend, Trace};

struct FailedStartup;
impl SAApplication for FailedStartup {
    type Message = ();
    type LocalEvent = ();
    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        cx.create_window(SAWindowSpec::default())
            .map(|_| ())
            .map_err(|rejected| rejected.into_parts().1)
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}

#[test]
fn failed_initialization_counts_only_actual_retirement_once() {
    let mut core = Core::<FailedStartup>::new().unwrap();
    let mut backend = TestBackend::default();
    backend.fail_next_observation = true;
    core.start(&mut FailedStartup, BackendOps::Test(&mut backend));
    core.poll_stop(&mut FailedStartup, BackendOps::Test(&mut backend));
    assert_eq!(core.state, SAHostState::Retiring);
    assert_eq!(core.retired_windows, 0);
    let key = backend.complete_destruction().unwrap();
    assert_eq!(core.retired_windows, 0);
    core.native_destroyed(key);
    assert_eq!(core.state, SAHostState::Closed);
    assert_eq!(core.retired_windows, 1);
    core.native_destroyed(key);
    assert_eq!(core.retired_windows, 1);
    assert!(core.native_windows.is_empty());
    assert!(backend.complete_destruction().is_none());
    assert_eq!(
        backend.trace.borrow().as_slice(),
        &[
            Trace::Created(String::from("Solapp")),
            Trace::DestroyRequested(String::from("Solapp")),
            Trace::Destroyed(String::from("Solapp")),
        ]
    );
}
