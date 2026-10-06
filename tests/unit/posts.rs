use crate::backend::BackendOps;
use crate::host::Core;
use crate::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::{self, ThreadId};

struct Message {
    value: u8,
    panic_drop: bool,
    drops: Arc<Mutex<Vec<(u8, ThreadId)>>>,
    on_drop: Option<(SAProxy<Message>, SARecipientId, Arc<AtomicBool>)>,
}
impl Message {
    fn new(value: u8, drops: &Arc<Mutex<Vec<(u8, ThreadId)>>>) -> Self {
        Self {
            value,
            panic_drop: false,
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
        assert!(!self.panic_drop, "posted destructor failure");
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

#[test]
fn empty_singleton_and_many_detach_preserve_frontier_and_failed_reservation_ownership() {
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
    core.transport
        .fail_next_batch_reservation
        .store(true, Ordering::Relaxed);
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    assert_eq!(cx.drain_posts(&mut app, 0), Ok(0));
    assert_eq!(cx.core.transport.accepted(), 2);
    assert_eq!(cx.drain_posts(&mut app, 2), Err(SAError::AllocationFailed));
    assert!(app.trace.is_empty());
    assert!(app.drops.lock().unwrap().is_empty());
    assert_eq!(first.outcome(), SAPostOutcome::Pending);
    assert_eq!(second.outcome(), SAPostOutcome::Pending);
    assert_eq!(cx.core.transport.len(), 2);
    assert_eq!(cx.drain_posts(&mut app, 1), Ok(1));
    assert_eq!(app.trace, [1]);
    assert_eq!(second.outcome(), SAPostOutcome::Pending);
    assert_eq!(cx.drain_posts(&mut app, 8), Ok(1));
    assert_eq!(cx.drain_posts(&mut app, 8), Ok(0));
    assert_eq!(cx.core.transport.accepted(), 0);
    assert_eq!(app.trace, [1, 2]);
}

#[test]
fn singleton_frontier_is_detached_before_nested_arrivals_and_destructor_failure_settles_tail() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    app.nested = true;
    let recipient = setup(&mut core, &mut app);
    core.proxy()
        .try_post(recipient, Message::new(1, &app.drops))
        .unwrap();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    assert_eq!(cx.drain_posts(&mut app, 8), Ok(1));
    assert_eq!(app.trace, [1, 3]);
    assert_eq!(cx.core.transport.accepted(), 1);
    assert_eq!(cx.drain_posts(&mut app, 8), Ok(1));
    assert_eq!(app.trace, [1, 3, 4]);
    assert_eq!(cx.core.transport.accepted(), 0);
    let mut failing = Message::new(5, &app.drops);
    failing.panic_drop = true;
    let first = cx.proxy().try_post(recipient, failing).unwrap();
    let second = cx
        .proxy()
        .try_post(recipient, Message::new(6, &app.drops))
        .unwrap();
    assert_eq!(cx.drain_posts(&mut app, 8), Ok(2));
    assert_eq!(first.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
    assert_eq!(
        second.outcome(),
        SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped)
    );
    assert_eq!(cx.core.transport.accepted(), 0);
    let dropped: Vec<_> = app
        .drops
        .lock()
        .unwrap()
        .iter()
        .map(|(value, _)| *value)
        .collect();
    assert_eq!(dropped, [3, 1, 4, 5, 6]);
}

fn capacity_handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, Message, u8>,
) -> SAPropagation {
    if let SAEvent::Posted { message, .. } = event {
        app.trace.push(message.value);
        assert_eq!(cx.core.transport.accepted(), 1);
        assert_eq!(
            cx.core.transport.len(),
            0,
            "active singleton is physically detached"
        );
        let rejected = cx
            .proxy()
            .try_post(app.recipient.unwrap(), Message::new(99, &app.drops))
            .unwrap_err();
        assert_eq!(rejected.reason(), &SAError::CapacityFull);
        assert_eq!(rejected.input().value, 99);
    }
    SAPropagation::Continue
}
#[test]
fn singleton_callback_still_holds_accepted_capacity() {
    let mut core = Core::<App>::with_config(&SAHostConfig {
        post_capacity: 1,
        ..SAHostConfig::default()
    })
    .unwrap();
    let mut app = App::new();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    app.recipient = Some(recipient);
    cx.subscribe(
        recipient,
        SAEventFilter::Posted,
        SAPriority::default(),
        capacity_handler,
    )
    .unwrap();
    let receipt = cx
        .proxy()
        .try_post(recipient, Message::new(1, &app.drops))
        .unwrap();
    assert_eq!(cx.drain_posts(&mut app, 1), Ok(1));
    assert_eq!(receipt.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
    assert_eq!(cx.core.transport.accepted(), 0);
    assert_eq!(app.trace, [1]);
}

#[test]
fn detached_backing_allocates_zero_for_empty_and_singleton_and_once_for_many() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    for count in [0, 1, 2, 32] {
        for value in 0..count {
            core.proxy()
                .try_post(recipient, Message::new(value, &app.drops))
                .unwrap();
        }
        let (batch, allocations) =
            crate::allocation_probe::measure(|| core.transport.detach(usize::MAX).unwrap());
        assert_eq!(allocations.allocations, usize::from(count > 1));
        assert_eq!(allocations.reallocations, 0);
        println!(
            "post-detach: count={count} allocations={} reallocations={}",
            allocations.allocations, allocations.reallocations
        );
        let mut consumed = 0;
        for post in batch {
            post.receipt
                .settle(SAPostOutcome::Delivered { callbacks: 0 });
            drop(post.message);
            core.transport.settled();
            consumed += 1;
        }
        assert_eq!(consumed, count);
        assert_eq!(core.transport.accepted(), 0);
    }
}

#[test]
fn singleton_retirement_stop_and_callback_failure_settle_and_dispose_once() {
    for mode in 0..3 {
        let mut core = Core::<App>::new().unwrap();
        let mut app = App::new();
        let recipient = setup(&mut core, &mut app);
        let receipt = core
            .proxy()
            .try_post(recipient, Message::new(1, &app.drops))
            .unwrap();
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        if mode == 0 {
            cx.retire_recipient(recipient).unwrap();
        }
        app.stop = mode == 1;
        app.panic = mode == 2;
        let result = cx.drain_posts(&mut app, 1);
        match mode {
            0 => {
                assert_eq!(result, Ok(1));
                assert_eq!(
                    receipt.outcome(),
                    SAPostOutcome::Discarded(SAPostDiscardReason::RecipientRetired)
                );
            }
            1 => {
                assert_eq!(result, Ok(1));
                assert_eq!(receipt.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
            }
            _ => {
                assert!(result.is_err());
                assert!(matches!(receipt.outcome(), SAPostOutcome::Faulted(_)));
            }
        }
        assert_eq!(cx.core.transport.accepted(), 0);
        assert_eq!(
            app.drops.lock().unwrap().as_slice(),
            &[(1, thread::current().id())]
        );
    }
}

fn producer_handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, Message, u8>,
) -> SAPropagation {
    if let SAEvent::Posted { message, .. } = event {
        app.trace.push(message.value);
        if !app.nested_done {
            app.nested_done = true;
            let proxy = cx.proxy();
            let recipient = app.recipient.unwrap();
            let drops = Arc::clone(&app.drops);
            let created = thread::spawn(move || {
                [3, 4].map(|value| {
                    proxy
                        .try_post(recipient, Message::new(value, &drops))
                        .unwrap()
                })
            })
            .join()
            .unwrap();
            app.created.extend(created);
        }
    }
    SAPropagation::Continue
}
#[test]
fn producer_arrivals_during_singleton_or_many_callbacks_wait_beyond_detached_frontier() {
    for count in [1, 2] {
        let mut core = Core::<App>::new().unwrap();
        let mut app = App::new();
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        let recipient = cx.create_recipient().unwrap();
        app.recipient = Some(recipient);
        cx.subscribe(
            recipient,
            SAEventFilter::Posted,
            SAPriority::default(),
            producer_handler,
        )
        .unwrap();
        for value in 1..=count {
            cx.proxy()
                .try_post(recipient, Message::new(value, &app.drops))
                .unwrap();
        }
        assert_eq!(cx.drain_posts(&mut app, 8), Ok(usize::from(count)));
        assert_eq!(app.trace, if count == 1 { vec![1] } else { vec![1, 2] });
        assert!(
            app.created
                .iter()
                .all(|receipt| receipt.outcome() == SAPostOutcome::Pending)
        );
        assert_eq!(cx.core.transport.accepted(), 2);
        assert_eq!(cx.drain_posts(&mut app, 8), Ok(2));
        assert_eq!(&app.trace[usize::from(count)..], [3, 4]);
        assert_eq!(cx.core.transport.accepted(), 0);
    }
}
