//! Main-process native startup/cleanup witness, with no interactive window.

use solapp::{
    SAApplication, SAContext, SAError, SAHost, SAHostConfig, SAStopProgress, SAWindowSpec,
    SAWindowState, SAWindowTarget,
};

struct Smoke {
    window: Option<SAWindowTarget>,
    polls: usize,
    fail: bool,
}

impl SAApplication for Smoke {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        self.window = Some(
            cx.create_window(SAWindowSpec {
                visible: false,
                ..SAWindowSpec::default()
            })
            .map_err(|rejected| rejected.into_parts().1)?,
        );
        println!("native window acquired");
        if self.fail {
            return Err(SAError::application("injected startup failure"));
        }
        cx.request_stop();
        Ok(())
    }

    fn stopping(&mut self, cx: &mut SAContext<'_, Self>) -> SAStopProgress {
        let Some(window) = self.window else {
            return SAStopProgress::Settled;
        };
        assert_eq!(cx.window_state(window), Ok(SAWindowState::Retiring));
        self.polls += 1;
        if self.polls < 3 {
            SAStopProgress::Pending
        } else {
            println!("application cleanup settled");
            SAStopProgress::Settled
        }
    }
}

fn main() {
    let fail = std::env::args().any(|argument| argument == "--fail-startup");
    let mut app = Smoke {
        window: None,
        polls: 0,
        fail,
    };
    let mut host = SAHost::<Smoke>::new(SAHostConfig::default()).expect("native event loop");
    let result = host.run(&mut app);
    assert_eq!(app.polls, 3);
    if fail {
        assert_eq!(
            result,
            Err(SAError::application("injected startup failure"))
        );
    } else {
        println!("{result:?}");
        assert_eq!(result.unwrap().windows_retired, 1);
    }
    assert_eq!(host.state(), solapp::SAHostState::Closed);
    assert_eq!(host.run(&mut app), Err(SAError::AlreadyRun));
    println!("native smoke completed");
}
