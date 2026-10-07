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
        assert_eq!(cx.core.transport.accepted(), 0);
        assert_eq!(
            app.drops.lock().unwrap().len(),
            if mode == 0 { 3 } else { 2 }
        );
        assert!(cx.core.post_batches.retained_capacities()[0] >= 2);
        cx.request_stop();
        cx.core.poll_stop(
            &mut app,
            BackendOps::Test(&mut crate::backend::test::TestBackend::default()),
        );
        assert_eq!(cx.core.state, SAHostState::Closed);
        assert_eq!(cx.core.post_batches.retained_capacities(), [0, 0]);
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
    assert!(cx.core.post_batches.retained_capacities()[0] >= 2);
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
        core.post_batches.clear();
        for value in 0..count {
            core.proxy()
                .try_post(recipient, Message::new(value, &app.drops))
                .unwrap();
        }
        let (mut batch, allocations) = crate::allocation_probe::measure(|| {
            core.transport
                .detach(usize::MAX, &mut core.post_batches)
                .unwrap()
        });
        assert_eq!(allocations.allocations, usize::from(count > 1));
        assert_eq!(allocations.reallocations, 0);
        println!(
            "post-detach: count={count} allocations={} reallocations={}",
            allocations.allocations, allocations.reallocations
        );
        let mut consumed = 0;
        for post in batch.by_ref() {
            post.receipt
                .settle(SAPostOutcome::Delivered { callbacks: 0 });
            drop(post.message);
            core.transport.settled();
            consumed += 1;
        }
        core.post_batches.recycle(batch);
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

fn settle_batch(core: &mut Core<App>, mut batch: crate::post::Batch<Message>) {
    for post in batch.by_ref() {
        post.receipt
            .settle(SAPostOutcome::Delivered { callbacks: 0 });
        core.dispose(post.message);
        core.transport.settled();
    }
    core.post_batches.recycle(batch);
}

#[test]
fn warmed_detached_buffers_remove_backing_allocations_without_pooling_receipts() {
    println!(
        "post-buffer-layout: element_bytes={} cache_metadata_bytes={} batch_value_bytes={}",
        std::mem::size_of::<crate::post::Post<Message>>(),
        std::mem::size_of::<crate::post::BatchCache<Message>>(),
        std::mem::size_of::<crate::post::Batch<Message>>()
    );
    for count in [2, 32, 256] {
        let mut core = Core::<App>::new().unwrap();
        let mut app = App::new();
        let recipient = setup(&mut core, &mut app);
        for round in 0..3 {
            for value in 0..count {
                core.proxy()
                    .try_post(recipient, Message::new(value as u8, &app.drops))
                    .unwrap();
            }
            let (batch, allocations) = crate::allocation_probe::measure(|| {
                core.transport
                    .detach(count, &mut core.post_batches)
                    .unwrap()
            });
            assert_eq!(allocations.allocations, usize::from(round == 0));
            assert_eq!(allocations.reallocations, 0);
            let actual_capacity = match &batch {
                crate::post::Batch::Many(buffer) => buffer.capacity(),
                _ => panic!("multiple posts must own a detached buffer"),
            };
            assert_eq!(
                allocations.requested_bytes,
                if round == 0 {
                    actual_capacity * std::mem::size_of::<crate::post::Post<Message>>()
                } else {
                    0
                }
            );
            settle_batch(&mut core, batch);
            let retained_bytes = core
                .post_batches
                .retained_capacities()
                .iter()
                .sum::<usize>()
                * std::mem::size_of::<crate::post::Post<Message>>();
            assert!(retained_bytes <= 64 * 1024);
            println!(
                "post-buffer: count={count} round={round} backing_allocations={} requested_bytes={} retained_bytes={retained_bytes}",
                allocations.allocations, allocations.requested_bytes
            );
            assert_eq!(core.transport.accepted(), 0);
        }
    }
}

#[test]
fn alternating_near_limit_batches_reuse_backing_without_displacing_idle_buffers() {
    use crate::post::{Batch, Post};
    use std::collections::VecDeque;
    for (lower, higher, spare) in [(500, 600, 0), (500, 600, 100), (300, 400, 200)] {
        let mut core = Core::<App>::with_config(&SAHostConfig {
            post_capacity: higher,
            ..SAHostConfig::default()
        })
        .unwrap();
        let mut app = App::new();
        let recipient = setup(&mut core, &mut app);
        let mut disposed = 0;
        for round in 0..5 {
            for count in [lower, higher] {
                let receipts: Vec<_> = (0..count)
                    .map(|value| {
                        core.proxy()
                            .try_post(recipient, Message::new(value as u8, &app.drops))
                            .unwrap()
                    })
                    .collect();
                if round == 0 && count == higher {
                    let retained = core.post_batches.retained_capacities();
                    core.transport
                        .fail_next_batch_reservation
                        .store(true, Ordering::Relaxed);
                    assert!(matches!(
                        core.transport.detach(count, &mut core.post_batches),
                        Err(SAError::AllocationFailed)
                    ));
                    assert_eq!(core.post_batches.retained_capacities(), retained);
                    assert_eq!(core.transport.len(), count);
                    assert_eq!(core.transport.accepted(), count);
                    assert_eq!(app.drops.lock().unwrap().len(), disposed);
                    assert!(
                        receipts
                            .iter()
                            .all(|r| r.outcome() == SAPostOutcome::Pending)
                    );
                }
                let (batch, allocations) = crate::allocation_probe::measure(|| {
                    core.transport
                        .detach(count, &mut core.post_batches)
                        .unwrap()
                });
                if round > 0 {
                    assert_eq!(allocations.allocations + allocations.reallocations, 0);
                    assert_eq!(
                        allocations.requested_bytes + allocations.reallocated_bytes,
                        0
                    );
                }
                settle_batch(&mut core, batch);
                disposed += count;
                if round == 0 && count == lower && spare != 0 {
                    core.post_batches
                        .recycle(Batch::Many(VecDeque::with_capacity(spare)));
                }
                let retained = core.post_batches.retained_capacities();
                assert!(retained.iter().any(|capacity| *capacity >= count));
                assert!(
                    retained.iter().sum::<usize>() * std::mem::size_of::<Post<Message>>()
                        <= 64 * 1024
                );
                if spare != 0 {
                    assert!(retained.contains(&spare));
                }
                assert_eq!(core.transport.accepted(), 0);
                assert_eq!(core.transport.len(), 0);
                assert_eq!(app.drops.lock().unwrap().len(), disposed);
                assert!(
                    receipts
                        .iter()
                        .all(|r| r.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
                );
            }
        }
        assert!(
            app.drops
                .lock()
                .unwrap()
                .iter()
                .all(|(_, owner)| *owner == thread::current().id())
        );
    }
}

#[test]
fn cache_retains_only_two_empty_buffers_with_combined_actual_capacity_byte_cap() {
    use crate::post::{Batch, BatchCache, Post};
    use std::collections::VecDeque;
    let mut cache = BatchCache::<Message>::new();
    for _ in 0..3 {
        cache.recycle(Batch::Many(VecDeque::with_capacity(2)));
    }
    assert_eq!(cache.retained_capacities(), [2, 2]);
    cache.clear();
    let capacity = (64 * 1024 / std::mem::size_of::<Post<Message>>()) * 3 / 4;
    cache.recycle(Batch::Many(VecDeque::with_capacity(capacity)));
    cache.recycle(Batch::Many(VecDeque::with_capacity(capacity)));
    assert_eq!(cache.retained_capacities(), [capacity, 0]);
    let actual_bytes =
        cache.retained_capacities().iter().sum::<usize>() * std::mem::size_of::<Post<Message>>();
    assert!(actual_bytes <= 64 * 1024);
    cache.clear();
    cache.recycle(Batch::Many(VecDeque::with_capacity(
        64 * 1024 / std::mem::size_of::<Post<Message>>() + 1,
    )));
    assert_eq!(cache.retained_capacities(), [0, 0]);
    // A two-post batch with a large inline message is still supported, but its
    // idle backing cannot fit the retention policy.
    let mut large = BatchCache::<[u8; 64 * 1024]>::new();
    large.recycle(Batch::Many(VecDeque::with_capacity(2)));
    assert_eq!(large.retained_capacities(), [0, 0]);
}

#[test]
fn failed_cached_growth_preserves_queue_receipts_and_original_cached_backing() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    for value in 0..2 {
        core.proxy()
            .try_post(recipient, Message::new(value, &app.drops))
            .unwrap();
    }
    let batch = core.transport.detach(2, &mut core.post_batches).unwrap();
    settle_batch(&mut core, batch);
    let capacities = core.post_batches.retained_capacities();
    let count = capacities.iter().max().unwrap() + 1;
    let receipts: Vec<_> = (2..2 + count)
        .map(|value| {
            core.proxy()
                .try_post(recipient, Message::new(value as u8, &app.drops))
                .unwrap()
        })
        .collect();
    core.transport
        .fail_next_batch_reservation
        .store(true, Ordering::Relaxed);
    assert!(matches!(
        core.transport.detach(count, &mut core.post_batches),
        Err(SAError::AllocationFailed)
    ));
    assert_eq!(core.post_batches.retained_capacities(), capacities);
    assert_eq!(core.transport.len(), count);
    assert_eq!(core.transport.accepted(), count);
    assert!(
        receipts
            .iter()
            .all(|r| r.outcome() == SAPostOutcome::Pending)
    );
    assert_eq!(app.drops.lock().unwrap().len(), 2);
    let batch = core
        .transport
        .detach(count, &mut core.post_batches)
        .unwrap();
    settle_batch(&mut core, batch);
    assert_eq!(core.transport.accepted(), 0);
    assert_eq!(app.drops.lock().unwrap().len(), 2 + count);
}

#[test]
fn active_batches_exclusively_take_cached_buffers_and_deeper_nesting_allocates() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    // Fill both cache slots with empty backing without ever sharing a batch.
    for _ in 0..2 {
        core.post_batches.recycle(crate::post::Batch::Many(
            std::collections::VecDeque::with_capacity(2),
        ));
    }
    let mut batches = Vec::new();
    for depth in 0..4 {
        for value in 0..2 {
            core.proxy()
                .try_post(recipient, Message::new(depth * 2 + value, &app.drops))
                .unwrap();
        }
        let (batch, allocations) = crate::allocation_probe::measure(|| {
            core.transport.detach(2, &mut core.post_batches).unwrap()
        });
        assert_eq!(allocations.allocations, usize::from(depth >= 2));
        batches.push(batch);
        assert_eq!(core.transport.accepted(), usize::from(depth + 1) * 2);
        assert_eq!(core.transport.len(), 0);
    }
    assert_eq!(core.post_batches.retained_capacities(), [0, 0]);
    for batch in batches.into_iter().rev() {
        settle_batch(&mut core, batch);
    }
    assert!(
        core.post_batches
            .retained_capacities()
            .iter()
            .all(|capacity| *capacity >= 2)
    );
    assert_eq!(core.transport.accepted(), 0);
    assert_eq!(app.drops.lock().unwrap().len(), 8);
}

fn deep_handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, Message, u8>,
) -> SAPropagation {
    if let SAEvent::Posted { message, .. } = event {
        app.trace.push(message.value);
        if message.value % 2 == 1 && message.value < 9 {
            for value in [message.value + 2, message.value + 3] {
                app.created.push(
                    cx.proxy()
                        .try_post(app.recipient.unwrap(), Message::new(value, &app.drops))
                        .unwrap(),
                );
            }
            assert_eq!(cx.drain_posts(app, 2), Ok(2));
        }
    }
    SAPropagation::Continue
}

#[test]
fn nested_callbacks_beyond_cache_depth_preserve_outer_frontiers_and_disposal() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    app.recipient = Some(recipient);
    cx.subscribe(
        recipient,
        SAEventFilter::Posted,
        SAPriority::default(),
        deep_handler,
    )
    .unwrap();
    for _ in 0..2 {
        cx.core.post_batches.recycle(crate::post::Batch::Many(
            std::collections::VecDeque::with_capacity(2),
        ));
    }
    for value in [1, 2] {
        app.created.push(
            cx.proxy()
                .try_post(recipient, Message::new(value, &app.drops))
                .unwrap(),
        );
    }
    assert_eq!(cx.drain_posts(&mut app, 2), Ok(2));
    assert_eq!(app.trace, [1, 3, 5, 7, 9, 10, 8, 6, 4, 2]);
    assert_eq!(cx.core.transport.accepted(), 0);
    assert_eq!(cx.core.transport.len(), 0);
    assert!(
        cx.core
            .post_batches
            .retained_capacities()
            .iter()
            .all(|capacity| *capacity >= 2)
    );
    assert!(
        app.created
            .iter()
            .all(|receipt| { receipt.outcome() == SAPostOutcome::Delivered { callbacks: 1 } })
    );
    let mut drops = app.drops.lock().unwrap().clone();
    assert!(
        drops
            .iter()
            .all(|(_, owner)| *owner == thread::current().id())
    );
    drops.sort_by_key(|(value, _)| *value);
    assert_eq!(
        drops.iter().map(|(value, _)| *value).collect::<Vec<_>>(),
        (1..=10).collect::<Vec<_>>()
    );
}

#[test]
fn oversized_detached_burst_settles_normally_and_does_not_retain_backing() {
    let count = 64 * 1024 / std::mem::size_of::<crate::post::Post<Message>>() + 1;
    let mut core = Core::<App>::with_config(&SAHostConfig {
        post_capacity: count,
        ..SAHostConfig::default()
    })
    .unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    for value in 0..count {
        core.proxy()
            .try_post(recipient, Message::new(value as u8, &app.drops))
            .unwrap();
    }
    let batch = core
        .transport
        .detach(count, &mut core.post_batches)
        .unwrap();
    settle_batch(&mut core, batch);
    assert_eq!(core.post_batches.retained_capacities(), [0, 0]);
    assert_eq!(core.transport.accepted(), 0);
    assert_eq!(app.drops.lock().unwrap().len(), count);
}

#[test]
fn oversized_frontier_preserves_idle_small_backing_for_the_next_warmed_drain() {
    let count = 64 * 1024 / std::mem::size_of::<crate::post::Post<Message>>() + 1;
    let mut core = Core::<App>::with_config(&SAHostConfig {
        post_capacity: count,
        ..SAHostConfig::default()
    })
    .unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    for value in 0..2 {
        core.proxy()
            .try_post(recipient, Message::new(value, &app.drops))
            .unwrap();
    }
    let batch = core.transport.detach(2, &mut core.post_batches).unwrap();
    settle_batch(&mut core, batch);
    let small_capacities = core.post_batches.retained_capacities();
    let receipts: Vec<_> = (0..count)
        .map(|value| {
            core.proxy()
                .try_post(recipient, Message::new(value as u8, &app.drops))
                .unwrap()
        })
        .collect();
    core.transport
        .fail_next_batch_reservation
        .store(true, Ordering::Relaxed);
    assert!(matches!(
        core.transport.detach(count, &mut core.post_batches),
        Err(SAError::AllocationFailed)
    ));
    assert_eq!(core.post_batches.retained_capacities(), small_capacities);
    assert_eq!(core.transport.len(), count);
    assert_eq!(core.transport.accepted(), count);
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.outcome() == SAPostOutcome::Pending)
    );
    let (batch, allocations) = crate::allocation_probe::measure(|| {
        core.transport
            .detach(count, &mut core.post_batches)
            .unwrap()
    });
    assert_eq!(allocations.allocations, 1);
    assert_eq!(allocations.reallocations, 0);
    assert!(allocations.requested_bytes > 64 * 1024);
    assert_eq!(core.post_batches.retained_capacities(), small_capacities);
    settle_batch(&mut core, batch);
    assert_eq!(core.post_batches.retained_capacities(), small_capacities);
    assert!(
        receipts
            .iter()
            .all(|receipt| { receipt.outcome() == SAPostOutcome::Delivered { callbacks: 0 } })
    );
    for value in 0..2 {
        core.proxy()
            .try_post(recipient, Message::new(value, &app.drops))
            .unwrap();
    }
    let (batch, allocations) = crate::allocation_probe::measure(|| {
        core.transport.detach(2, &mut core.post_batches).unwrap()
    });
    assert_eq!(allocations.allocations, 0);
    assert_eq!(allocations.reallocations, 0);
    assert_eq!(allocations.requested_bytes, 0);
    settle_batch(&mut core, batch);
    assert_eq!(core.post_batches.retained_capacities(), small_capacities);
    assert_eq!(core.transport.accepted(), 0);
    let drops = app.drops.lock().unwrap();
    assert_eq!(drops.len(), count + 4);
    assert!(
        drops
            .iter()
            .all(|(_, owner)| *owner == thread::current().id())
    );
}

struct LargeApp;
impl SAApplication for LargeApp {
    type Message = [u8; 64 * 1024];
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}

#[test]
fn large_inline_messages_use_uncached_fallible_backing_and_keep_receipts() {
    let mut core = Core::<LargeApp>::with_config(&SAHostConfig {
        post_capacity: 2,
        ..SAHostConfig::default()
    })
    .unwrap();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    let receipts = [
        cx.proxy().try_post(recipient, [1; 64 * 1024]).unwrap(),
        cx.proxy().try_post(recipient, [2; 64 * 1024]).unwrap(),
    ];
    assert_eq!(cx.drain_posts(&mut LargeApp, 2), Ok(2));
    assert_eq!(cx.core.transport.accepted(), 0);
    assert_eq!(cx.core.post_batches.retained_capacities(), [0, 0]);
    assert!(
        receipts
            .iter()
            .all(|r| r.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
    );
}

fn nested_failure_handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, Message, u8>,
) -> SAPropagation {
    if let SAEvent::Posted { message, .. } = event {
        app.trace.push(message.value);
        if message.value == 1 {
            for value in [3, 4] {
                app.created.push(
                    cx.proxy()
                        .try_post(app.recipient.unwrap(), Message::new(value, &app.drops))
                        .unwrap(),
                );
            }
            let result = cx.drain_posts(app, 2);
            assert_eq!(result.is_err(), app.panic);
            assert!(cx.core.post_batches.retained_capacities()[0] >= 2);
        } else if message.value == 3 {
            assert_eq!(cx.core.post_batches.retained_capacities(), [0, 0]);
            assert!(!app.panic, "nested posted handler failure");
            cx.request_stop();
        }
    }
    SAPropagation::Continue
}

#[test]
fn nested_stop_and_fault_exhaust_both_owned_tails_before_recycling() {
    for panic in [false, true] {
        let mut core = Core::<App>::new().unwrap();
        let mut app = App::new();
        app.panic = panic;
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        let recipient = cx.create_recipient().unwrap();
        app.recipient = Some(recipient);
        cx.subscribe(
            recipient,
            SAEventFilter::Posted,
            SAPriority::default(),
            nested_failure_handler,
        )
        .unwrap();
        for _ in 0..2 {
            cx.core.post_batches.recycle(crate::post::Batch::Many(
                std::collections::VecDeque::with_capacity(2),
            ));
        }
        let outer = [1, 2].map(|value| {
            cx.proxy()
                .try_post(recipient, Message::new(value, &app.drops))
                .unwrap()
        });
        assert_eq!(cx.drain_posts(&mut app, 2), Ok(2));
        assert_eq!(app.trace, [1, 3]);
        assert_eq!(
            outer[0].outcome(),
            SAPostOutcome::Delivered { callbacks: 1 }
        );
        let discarded = SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped);
        assert_eq!(outer[1].outcome(), discarded);
        assert_eq!(app.created[1].outcome(), discarded);
        if panic {
            assert!(matches!(
                app.created[0].outcome(),
                SAPostOutcome::Faulted(_)
            ));
        } else {
            assert_eq!(
                app.created[0].outcome(),
                SAPostOutcome::Delivered { callbacks: 1 }
            );
        }
        assert_eq!(cx.core.transport.accepted(), 0);
        assert_eq!(cx.core.post_batches.retained_capacities(), [2, 2]);
        assert_eq!(
            app.drops.lock().unwrap().as_slice(),
            &[3, 4, 1, 2].map(|value| (value, thread::current().id()))
        );
        cx.core.poll_stop(
            &mut app,
            BackendOps::Test(&mut crate::backend::test::TestBackend::default()),
        );
        assert_eq!(cx.core.state, SAHostState::Closed);
        assert_eq!(cx.core.post_batches.retained_capacities(), [0, 0]);
    }
}

#[test]
fn final_native_destruction_ack_clears_retained_post_backing_while_host_survives() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let mut native = crate::backend::test::TestBackend::default();
    let mut cx = SAContext::new(
        &mut core,
        BackendOps::Test(&mut native),
        SAContextPhase::Startup,
    );
    cx.create_window(SAWindowSpec::default()).unwrap();
    for _ in 0..2 {
        cx.core.post_batches.recycle(crate::post::Batch::Many(
            std::collections::VecDeque::with_capacity(2),
        ));
    }
    cx.request_stop();
    core.poll_stop(&mut app, BackendOps::Test(&mut native));
    assert_eq!(core.state, SAHostState::Retiring);
    assert_eq!(core.post_batches.retained_capacities(), [2, 2]);
    core.native_destroyed(native.complete_destruction().unwrap());
    assert_eq!(core.state, SAHostState::Closed);
    assert_eq!(core.post_batches.retained_capacities(), [0, 0]);
}

#[test]
fn cached_backing_needs_no_reservation_and_empty_singleton_leave_cache_idle() {
    let mut core = Core::<App>::new().unwrap();
    let mut app = App::new();
    let recipient = setup(&mut core, &mut app);
    core.post_batches.recycle(crate::post::Batch::Many(
        std::collections::VecDeque::with_capacity(2),
    ));
    let capacities = core.post_batches.retained_capacities();
    core.transport
        .fail_next_batch_reservation
        .store(true, Ordering::Relaxed);
    for count in [0, 1, 2] {
        for value in 0..count {
            core.proxy()
                .try_post(recipient, Message::new(value as u8, &app.drops))
                .unwrap();
        }
        let (batch, allocations) = crate::allocation_probe::measure(|| {
            core.transport
                .detach(count, &mut core.post_batches)
                .unwrap()
        });
        assert_eq!(allocations.allocations, 0);
        assert_eq!(allocations.reallocations, 0);
        assert_eq!(allocations.requested_bytes, 0);
        assert_eq!(
            core.post_batches.retained_capacities(),
            if count < 2 { capacities } else { [0, 0] }
        );
        settle_batch(&mut core, batch);
        assert_eq!(core.post_batches.retained_capacities(), capacities);
        assert!(
            core.transport
                .fail_next_batch_reservation
                .load(Ordering::Relaxed)
        );
    }
    assert_eq!(core.transport.accepted(), 0);
}

#[test]
#[ignore = "optimized detachment timing; run only while other compilers/timings are idle"]
#[expect(
    clippy::assertions_on_constants,
    reason = "This ignored timing fixture must reject debug-mode measurements."
)]
fn detached_post_buffer_timing_matrix() {
    assert!(!cfg!(debug_assertions), "run this fixture with --release");
    use std::time::Instant;
    const WARMUP: usize = 128;
    const ITERATIONS: usize = 10_000;
    const SAMPLES: usize = 9;
    let oversize = 64 * 1024 / std::mem::size_of::<crate::post::Post<Message>>() + 1;
    let workloads: [(&str, &[usize], bool); 6] = [
        ("two", &[2], false),
        ("thirty_two", &[32], false),
        ("two_fifty_six", &[256], false),
        ("nested_two", &[32, 32], true),
        ("nested_four", &[32, 32, 32, 32], true),
        ("burst", &[2, 32, oversize, 2], false),
    ];
    println!(
        "post-timing-parameters: samples={SAMPLES} warmup={WARMUP} iterations={ITERATIONS} post_bytes={} cache_limit_bytes=65536 cache_limit_buffers=2; detach-only excludes emptying/recycling/deallocation; total includes detach+empty+recycle/deallocate (including cold cache clearing); both exclude receipt allocation, payload dispatch/disposal, queue restoration and native wake; burst cold means cold at cycle start",
        std::mem::size_of::<crate::post::Post<Message>>()
    );
    for (workload, counts, nested) in workloads {
        let capacity = if nested {
            counts.iter().sum()
        } else {
            *counts.iter().max().unwrap()
        };
        let mut timings = [Vec::new(), Vec::new(), Vec::new()];
        let mut total_timings = [Vec::new(), Vec::new(), Vec::new()];
        for sample in 0..SAMPLES {
            // Rotate order so every variant occupies each ordinal three times.
            for offset in 0..3 {
                let variant = (sample + offset) % 3;
                let name = ["baseline_uncached", "cache_cold", "cache_warm"][variant];
                let mut core = Core::<App>::with_config(&SAHostConfig {
                    post_capacity: capacity,
                    ..SAHostConfig::default()
                })
                .unwrap();
                let mut app = App::new();
                let recipient = setup(&mut core, &mut app);
                for value in 0..capacity {
                    core.proxy()
                        .try_post(recipient, Message::new(value as u8, &app.drops))
                        .unwrap();
                }
                let mut elapsed = std::time::Duration::ZERO;
                let mut total_elapsed = std::time::Duration::ZERO;
                let mut spare =
                    std::collections::VecDeque::with_capacity(*counts.iter().max().unwrap());
                for iteration in 0..WARMUP + ITERATIONS {
                    let mut active: [Option<crate::post::Batch<Message>>; 4] =
                        [None, None, None, None];
                    for (index, count) in counts.iter().enumerate() {
                        let start = Instant::now();
                        let batch = if variant == 0 {
                            core.transport.detach_uncached_for_measurement(*count)
                        } else {
                            core.transport.detach(*count, &mut core.post_batches)
                        }
                        .unwrap();
                        let duration = start.elapsed();
                        if iteration >= WARMUP {
                            elapsed += duration;
                            total_elapsed += duration;
                        }
                        if nested {
                            active[index] = Some(batch);
                        } else {
                            let start = Instant::now();
                            let mut batch = batch;
                            for post in batch.by_ref() {
                                spare.push_back(post);
                            }
                            if variant != 0 {
                                core.post_batches.recycle(batch);
                            } else {
                                drop(batch);
                            }
                            if variant == 1 && index + 1 == counts.len() {
                                core.post_batches.clear();
                            }
                            if iteration >= WARMUP {
                                total_elapsed += start.elapsed();
                            }
                            spare = match core
                                .transport
                                .restore_for_measurement(crate::post::Batch::Many(spare))
                            {
                                crate::post::Batch::Many(empty) => empty,
                                _ => unreachable!(),
                            };
                        }
                    }
                    for (index, mut batch) in active.into_iter().rev().flatten().enumerate() {
                        let start = Instant::now();
                        for post in batch.by_ref() {
                            spare.push_back(post);
                        }
                        if variant != 0 {
                            core.post_batches.recycle(batch);
                        } else {
                            drop(batch);
                        }
                        if variant == 1 && index + 1 == counts.len() {
                            core.post_batches.clear();
                        }
                        if iteration >= WARMUP {
                            total_elapsed += start.elapsed();
                        }
                        spare = match core
                            .transport
                            .restore_for_measurement(crate::post::Batch::Many(spare))
                        {
                            crate::post::Batch::Many(empty) => empty,
                            _ => unreachable!(),
                        };
                    }
                    assert_eq!(core.transport.accepted(), capacity);
                    assert_eq!(core.transport.len(), capacity);
                }
                let ns = elapsed.as_nanos() as f64 / (ITERATIONS * counts.len()) as f64;
                let total_ns = total_elapsed.as_nanos() as f64 / (ITERATIONS * counts.len()) as f64;
                timings[variant].push(ns);
                total_timings[variant].push(total_ns);
                let retained_bytes = core
                    .post_batches
                    .retained_capacities()
                    .iter()
                    .sum::<usize>()
                    * std::mem::size_of::<crate::post::Post<Message>>();
                println!(
                    "post-timing-sample: workload={workload} variant={name} sample={sample} ns_per_detach={ns:.2} total_ns_per_batch={total_ns:.2} retained_bytes={retained_bytes} counts={counts:?} nested={nested}"
                );
            }
        }
        for (variant, mut samples) in timings.into_iter().enumerate() {
            samples.sort_by(f64::total_cmp);
            let name = ["baseline_uncached", "cache_cold", "cache_warm"][variant];
            println!(
                "post-timing-median: workload={workload} variant={name} ns_per_detach={:.2}",
                samples[SAMPLES / 2]
            );
        }
        for (variant, mut samples) in total_timings.into_iter().enumerate() {
            samples.sort_by(f64::total_cmp);
            let name = ["baseline_uncached", "cache_cold", "cache_warm"][variant];
            println!(
                "post-total-timing-median: workload={workload} variant={name} ns_per_batch={:.2}",
                samples[SAMPLES / 2]
            );
        }
    }
}
