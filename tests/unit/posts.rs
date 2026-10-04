use crate::backend::BackendOps;
use crate::host::Core;
use crate::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::{self, ThreadId};

struct Message {
    value: u8,
    drops: Arc<Mutex<Vec<(u8, ThreadId)>>>,
    on_drop: Option<(SAProxy<Message>, SARecipientId, Arc<AtomicBool>)>,
}
impl Message {
    fn new(value: u8, drops: &Arc<Mutex<Vec<(u8, ThreadId)>>>) -> Self {
        Self {
            value,
            drops: Arc::clone(drops),
            on_drop: None,
        }
    }
}
impl Drop for Message {
    fn drop(&mut self) {
        self.drops
            .lock()
            .unwrap()
            .push((self.value, thread::current().id()));
        if let Some((proxy, recipient, observed)) = self.on_drop.take() {
            let rejection = proxy
                .try_post(recipient, Message::new(99, &self.drops))
                .unwrap_err();
            observed.store(
                rejection.reason() == &SAError::CapacityFull,
                Ordering::Relaxed,
            );
        }
    }
}
struct App {
    recipient: Option<SARecipientId>,
    trace: Vec<u8>,
    nested: bool,
    nested_done: bool,
    drops: Arc<Mutex<Vec<(u8, ThreadId)>>>,
    created: Vec<SAPostReceipt>,
    stop: bool,
    panic: bool,
}
impl App {
    fn new() -> Self {
        Self {
            recipient: None,
            trace: Vec::new(),
            nested: false,
            nested_done: false,
            drops: Arc::new(Mutex::new(Vec::new())),
            created: Vec::new(),
            stop: false,
            panic: false,
        }
    }
}
impl SAApplication for App {
    type Message = Message;
    type LocalEvent = u8;
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}
fn handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, Message, u8>,
) -> SAPropagation {
    let SAEvent::Posted { message, receipt } = event else {
        return SAPropagation::Continue;
    };
    assert_eq!(receipt.outcome(), SAPostOutcome::Pending);
    app.trace.push(message.value);
    if app.panic {
        panic!("posted handler failure");
    }
    if app.stop {
        cx.request_stop();
    }
    if app.nested && !app.nested_done {
        app.nested_done = true;
        app.created.push(
            cx.proxy()
                .try_post(app.recipient.unwrap(), Message::new(3, &app.drops))
                .unwrap(),
        );
        cx.drain_posts(app, 1).unwrap();
        app.created.push(
            cx.proxy()
                .try_post(app.recipient.unwrap(), Message::new(4, &app.drops))
                .unwrap(),
        );
    }
    SAPropagation::Continue
}
fn setup(core: &mut Core<App>, app: &mut App) -> SARecipientId {
    let mut cx = SAContext::new(core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    app.recipient = Some(recipient);
    cx.subscribe(
        recipient,
        SAEventFilter::Posted,
        SAPriority::default(),
        handler,
    )
    .unwrap();
    recipient
}

#[test]
fn actual_detached_frontier_survives_nested_drains_and_new_arrivals() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    app.nested = true;
    let recipient = setup(&mut core, &mut app);
    let proxy = core.proxy();
    let first = proxy
        .try_post(recipient, Message::new(1, &app.drops))
        .unwrap();
    let second = proxy
        .try_post(recipient, Message::new(2, &app.drops))
        .unwrap();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    assert_eq!(cx.drain_posts(&mut app, 2).unwrap(), 2);
    assert_eq!(app.trace, [1, 3, 2]);
    assert_eq!(app.created[1].outcome(), SAPostOutcome::Pending);
    assert!(matches!(
        first.outcome(),
        SAPostOutcome::Delivered { callbacks: 1 }
    ));
    assert!(matches!(
        second.outcome(),
        SAPostOutcome::Delivered { callbacks: 1 }
    ));
    cx.drain_posts(&mut app, 2).unwrap();
    assert_eq!(app.trace, [1, 3, 2, 4]);
}

#[test]
fn capacity_remains_charged_through_owner_payload_destruction() {
    let mut core = Core::<App>::with_config(&SAHostConfig {
        post_capacity: 1,
        ..SAHostConfig::default()
    })
    .unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    let proxy = core.proxy();
    let observed = Arc::new(AtomicBool::new(false));
    let mut message = Message::new(1, &app.drops);
    message.on_drop = Some((proxy.clone(), recipient, Arc::clone(&observed)));
    let receipt = proxy.try_post(recipient, message).unwrap();
    let rejected = proxy
        .try_post(recipient, Message::new(2, &app.drops))
        .unwrap_err();
    assert_eq!(rejected.input().value, 2);
    assert_eq!(rejected.reason(), &SAError::CapacityFull);
    drop(rejected);
    SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
        .drain_posts(&mut app, 1)
        .unwrap();
    assert!(observed.load(Ordering::Relaxed));
    assert_eq!(receipt.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
    proxy
        .try_post(recipient, Message::new(3, &app.drops))
        .unwrap();
}

#[test]
fn obsolete_targets_stop_tails_and_panics_have_durable_terminal_outcomes() {
    for mode in 0..3 {
        let mut core = Core::<App>::new().unwrap();
        let mut app = App::new();
        let recipient = setup(&mut core, &mut app);
        let first = core
            .proxy()
            .try_post(recipient, Message::new(1, &app.drops))
            .unwrap();
        let second = core
            .proxy()
            .try_post(recipient, Message::new(2, &app.drops))
            .unwrap();
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        if mode == 0 {
            cx.retire_recipient(recipient).unwrap();
            let replacement = cx.create_recipient().unwrap();
            assert_ne!(replacement, recipient);
            assert_eq!(
                cx.proxy()
                    .try_post(recipient, Message::new(3, &app.drops))
                    .unwrap_err()
                    .reason(),
                &SAError::StaleIdentity
            );
        }
        app.stop = mode == 1;
        app.panic = mode == 2;
        let result = cx.drain_posts(&mut app, 8);
        match mode {
            0 => {
                assert!(result.is_ok());
                assert_eq!(
                    first.outcome(),
                    SAPostOutcome::Discarded(SAPostDiscardReason::RecipientRetired)
                );
                assert_eq!(second.outcome(), first.outcome());
            }
            1 => {
                assert!(result.is_ok());
                assert_eq!(first.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
                assert_eq!(
                    second.outcome(),
                    SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped)
                );
            }
            2 => {
                assert!(result.is_err());
                assert!(matches!(first.outcome(), SAPostOutcome::Faulted(_)));
                assert_eq!(
                    second.outcome(),
                    SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped)
                );
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn cross_thread_close_race_preserves_ownership_and_proxy_can_outlive_closed_empty_host() {
    for _ in 0..8 {
        let owner = thread::current().id();
        let mut core = Core::<App>::new().unwrap();
        let mut app = App::new();
        let recipient = setup(&mut core, &mut app);
        let proxy = core.proxy();
        let worker_proxy = proxy.clone();
        let drops = Arc::clone(&app.drops);
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let worker = thread::spawn(move || {
            worker_barrier.wait();
            (
                thread::current().id(),
                worker_proxy.try_post(recipient, Message::new(1, &drops)),
            )
        });
        barrier.wait();
        core.request_stop(SAStopReason::Application);
        let (worker_id, outcome) = worker.join().unwrap();
        match outcome {
            Ok(receipt) => {
                core.poll_stop(&mut app, BackendOps::Unavailable);
                assert_eq!(
                    receipt.outcome(),
                    SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped)
                );
                assert_eq!(app.drops.lock().unwrap()[0], (1, owner));
            }
            Err(rejected) => {
                assert_eq!(rejected.reason(), &SAError::AdmissionClosed);
                assert_eq!(rejected.input().value, 1);
                drop(rejected);
                assert_eq!(app.drops.lock().unwrap()[0], (1, owner));
            }
        }
        // Rejected input stays where the caller chooses to drop it; check the
        // explicit foreign-thread caller separately after the host root is gone.
        drop(core);
        let drops = Arc::clone(&app.drops);
        thread::spawn(move || {
            assert_eq!(
                proxy
                    .try_post(recipient, Message::new(2, &drops))
                    .unwrap_err()
                    .reason(),
                &SAError::AdmissionClosed
            );
        })
        .join()
        .unwrap();
        assert_ne!(app.drops.lock().unwrap().last().unwrap().1, owner);
        assert_ne!(worker_id, owner);
    }
}

#[test]
fn host_root_drop_settles_queue_on_owner_even_if_receipts_and_proxies_survive() {
    let owner = thread::current().id();
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    let proxy = core.proxy();
    let receipt = proxy
        .try_post(recipient, Message::new(7, &app.drops))
        .unwrap();
    drop(core);
    assert_eq!(
        receipt.outcome(),
        SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped)
    );
    assert_eq!(app.drops.lock().unwrap().as_slice(), &[(7, owner)]);
    thread::spawn(move || drop(proxy)).join().unwrap();
}
