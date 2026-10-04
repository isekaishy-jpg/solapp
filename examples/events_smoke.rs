//! Borrowed non-Send application, native post intake, and local timer smoke.

use solapp::{
    SAApplication, SAContext, SAError, SAEvent, SAEventFilter, SAHost, SAHostConfig, SAPostOutcome,
    SAPostReceipt, SAPriority, SAPropagation, SAStopProgress, SAWindowSpec,
};
use std::rc::Rc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct Demo<'data> {
    local: &'data str,
    owner_local: Rc<()>,
    worker: Option<JoinHandle<SAPostReceipt>>,
    posted: bool,
    timed: bool,
    trace: Vec<String>,
}

fn route<'data>(
    app: &mut Demo<'data>,
    cx: &mut SAContext<'_, Demo<'data>>,
    event: &SAEvent<'_, String, &'data str>,
) -> SAPropagation {
    match event {
        SAEvent::Local(value) => app.trace.push(format!("local: {value}")),
        SAEvent::Posted { message, .. } => {
            app.posted = true;
            app.trace.push(format!("posted: {message}"));
        }
        SAEvent::Timer { event, .. } => {
            app.timed = true;
            app.trace.push(format!("timer: {event}"));
        }
        SAEvent::Input(_) => (),
    }
    if app.posted && app.timed {
        cx.request_stop();
    }
    SAPropagation::Continue
}

impl<'data> SAApplication for Demo<'data> {
    type Message = String;
    type LocalEvent = &'data str;

    fn started(&mut self, cx: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        cx.create_window(SAWindowSpec {
            visible: false,
            ..SAWindowSpec::default()
        })
        .map_err(|rejected| rejected.into_parts().1)?;
        let recipient = cx.create_recipient()?;
        cx.subscribe(recipient, SAEventFilter::All, SAPriority::default(), route)?;
        let local = self.local;
        cx.dispatch_local(self, &local)?;
        let deadline = cx.clock().raw.checked_add(Duration::from_millis(20))?;
        cx.schedule_timer(recipient, deadline, local)
            .map_err(|rejected| rejected.into_parts().1)?;
        let proxy = cx.proxy();
        self.worker = Some(thread::spawn(move || {
            proxy
                .try_post(recipient, String::from("foreign-thread owned message"))
                .expect("post accepted before stop")
        }));
        Ok(())
    }

    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return SAStopProgress::Pending;
        }
        if let Some(worker) = self.worker.take() {
            let receipt = worker.join().expect("finished worker");
            assert_eq!(receipt.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
        }
        assert_eq!(Rc::strong_count(&self.owner_local), 1);
        SAStopProgress::Settled
    }
}

fn main() {
    let borrowed = String::from("borrowed owner-local payload");
    let mut app = Demo {
        local: &borrowed,
        owner_local: Rc::new(()),
        worker: None,
        posted: false,
        timed: false,
        trace: Vec::new(),
    };
    let mut host = SAHost::<Demo<'_>>::new(SAHostConfig::default()).expect("native event loop");
    let result = host.run(&mut app).expect("host retirement");
    assert!(app.posted && app.timed);
    assert_eq!(result.windows_retired, 1);
    println!("{:?}", app.trace);
    println!("events smoke completed: {result:?}");
}
