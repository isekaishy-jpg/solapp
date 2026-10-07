use crate::backend::BackendOps;
use crate::host::Core;
use crate::post::{Batch, Post};
use crate::*;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;

struct PlainApp<M>(std::marker::PhantomData<M>);
impl<M: Send + 'static> SAApplication for PlainApp<M> {
    type Message = M;
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}
type Payload = [u8; 48];
type Fixture = Core<PlainApp<Payload>>;

fn limit<M>() -> usize {
    65536 / std::mem::size_of::<Post<M>>()
}
fn fixture(capacity: usize) -> (Fixture, SARecipientId) {
    let mut core = Core::with_config(&SAHostConfig {
        post_capacity: capacity,
        ..SAHostConfig::default()
    })
    .unwrap();
    let recipient = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
        .create_recipient()
        .unwrap();
    (core, recipient)
}
fn enqueue(core: &Fixture, recipient: SARecipientId, count: usize) -> Vec<SAPostReceipt> {
    (0..count)
        .map(|i| core.proxy().try_post(recipient, [i as u8; 48]).unwrap())
        .collect()
}
fn empty(core: &mut Fixture, batch: &mut Batch<Payload>) {
    for post in batch.by_ref() {
        post.receipt
            .settle(SAPostOutcome::Delivered { callbacks: 0 });
        core.dispose(post.message);
        core.transport.settled();
    }
}
fn complete(core: &mut Fixture, mut batch: Batch<Payload>) {
    empty(core, &mut batch);
    core.post_batches.complete(batch, true);
}
fn calls(counts: crate::allocation_probe::Counts) -> usize {
    counts.allocations + counts.reallocations
}
fn check_idle(core: &Fixture) {
    let state = core.post_batches.state_for_test();
    assert!(state.0.iter().sum::<usize>() * std::mem::size_of::<Post<Payload>>() <= 65536);
    assert_eq!(state.1, [0, 0]);
    assert_eq!(core.transport.accepted(), 0);
    assert_eq!(core.transport.len(), 0);
}

// Queue admission and receipt construction are deliberately outside the scopes;
// both checkout and completion (including replacements) contribute allocations.
fn operation(core: &mut Fixture, recipient: SARecipientId, sizes: [usize; 2]) -> usize {
    let receipts = enqueue(core, recipient, sizes.iter().sum());
    let ((mut outer, mut child), acquisition) = crate::allocation_probe::measure(|| {
        let outer = core
            .transport
            .detach(sizes[0], &mut core.post_batches)
            .unwrap();
        let child = core
            .transport
            .detach(sizes[1], &mut core.post_batches)
            .unwrap();
        (outer, child)
    });
    assert_eq!(core.transport.accepted(), receipts.len());
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.outcome() == SAPostOutcome::Pending)
    );
    let (_, completion) = crate::allocation_probe::measure(|| {
        empty(core, &mut child);
        core.post_batches.complete(child, true);
        empty(core, &mut outer);
        core.post_batches.complete(outer, true);
    });
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
    );
    check_idle(core);
    calls(acquisition) + calls(completion)
}

#[test]
fn production_three_operation_corpus_converges_including_asymmetric_regressions() {
    let k = limit::<Payload>();
    let tiny = (k * 2 / 100).max(2);
    let medium = k * 35 / 100;
    let large = k * 59 / 100;
    let operations = [
        [tiny, 0],
        [large, 0],
        [tiny, tiny],
        [tiny, medium],
        [medium, tiny],
        [medium, large],
        [large, medium],
        [1, tiny],
    ];
    let seeds = [[0, 0], [large, 0], [k * 44 / 100, k * 50 / 100]];
    let mut patterns = 0;
    for seed in seeds {
        for first in operations {
            for second in operations {
                for third in operations {
                    let mut core = fixture(k * 2).0;
                    let recipient =
                        SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
                            .create_recipient()
                            .unwrap();
                    for capacity in seed.into_iter().filter(|capacity| *capacity != 0) {
                        core.post_batches
                            .seed_for_test(VecDeque::with_capacity(capacity));
                    }
                    for period in 0..8 {
                        let total = [first, second, third]
                            .into_iter()
                            .map(|sizes| operation(&mut core, recipient, sizes))
                            .sum::<usize>();
                        if period >= 4 {
                            assert_eq!(
                                total,
                                0,
                                "seed={seed:?} sequence={:?} period={period}",
                                [first, second, third]
                            );
                        }
                    }
                    patterns += 1;
                }
            }
        }
    }
    println!(
        "production mixed corpus: patterns={patterns}, periods=8, final allocation-free periods=4, post_bytes={}",
        std::mem::size_of::<Post<Payload>>()
    );
}

#[test]
fn growth_ramps_preserve_amortization_and_plateaus_reuse_complete_cycles() {
    for (start, end, maximum) in [(2, 32, 4), (100, 200, 2), (400, 600, 2)] {
        assert!(end <= limit::<Payload>());
        let (mut core, recipient) = fixture(end);
        let reservations = (start..=end)
            .map(|count| operation(&mut core, recipient, [count, 0]))
            .sum::<usize>();
        assert!(
            reservations <= maximum,
            "ramp={start}..={end} reservations={reservations}"
        );
        for _ in 0..1000 {
            assert_eq!(operation(&mut core, recipient, [end, 0]), 0);
        }
        println!(
            "production ramp: {start}..={end} reservations={reservations}, plateau=1000 allocation-free"
        );
    }
}

#[test]
fn incompatible_peak_transitions_to_new_fitting_phase() {
    let k = limit::<Payload>();
    let (mut core, recipient) = fixture(2 * k);
    operation(&mut core, recipient, [k * 9 / 10, k * 8 / 10]);
    let medium = k * 35 / 100;
    let large = k * 59 / 100;
    for period in 0..8 {
        let total = operation(&mut core, recipient, [medium, large])
            + operation(&mut core, recipient, [large, medium]);
        if period >= 4 {
            assert_eq!(total, 0);
        }
    }
    let (_, active, observed_large, observed_small) = core.post_batches.state_for_test();
    assert_eq!(active, [0, 0]);
    assert_eq!((observed_large, observed_small), (large, medium));
}

#[test]
fn minimum_capacity_pairs_converge_from_cold_and_full_sized_seed() {
    let k = limit::<Payload>();
    let sequences = [
        [[2, k - 2], [2, k - 2], [2, k - 2]],
        [[k - 2, 2], [k - 2, 2], [k - 2, 2]],
        [[2, k - 2], [k - 2, 2], [2, 2]],
        [[1, 2], [2, k - 2], [2, 2]],
    ];
    for seed in [0, k] {
        for sequence in sequences {
            let (mut core, recipient) = fixture(k);
            if seed != 0 {
                core.post_batches
                    .seed_for_test(VecDeque::with_capacity(seed));
            }
            for period in 0..8 {
                let total = sequence
                    .into_iter()
                    .map(|sizes| operation(&mut core, recipient, sizes))
                    .sum::<usize>();
                if period >= 4 {
                    assert_eq!(
                        total, 0,
                        "seed={seed} sequence={sequence:?} period={period}"
                    );
                }
            }
        }
    }
}

#[test]
fn checkout_failure_rolls_back_idle_demand_and_active_tickets_before_queue_mutation() {
    for seeded in [false, true] {
        let (mut core, recipient) = fixture(64);
        if seeded {
            core.post_batches.seed_for_test(VecDeque::with_capacity(4));
        }
        core.post_batches.set_demand_for_test(6, 2);
        let receipts = enqueue(&core, recipient, 12);
        let state = core.post_batches.state_for_test();
        core.transport
            .fail_next_batch_reservation
            .store(true, Ordering::Relaxed);
        assert!(matches!(
            core.transport.detach(8, &mut core.post_batches),
            Err(SAError::AllocationFailed)
        ));
        assert_eq!(core.post_batches.state_for_test(), state);
        assert_eq!((core.transport.len(), core.transport.accepted()), (12, 12));
        assert!(
            receipts
                .iter()
                .all(|r| r.outcome() == SAPostOutcome::Pending)
        );
        let outer = core.transport.detach(8, &mut core.post_batches).unwrap();
        let state = core.post_batches.state_for_test();
        core.transport
            .fail_next_batch_reservation
            .store(true, Ordering::Relaxed);
        assert!(matches!(
            core.transport.detach(4, &mut core.post_batches),
            Err(SAError::AllocationFailed)
        ));
        assert_eq!(core.post_batches.state_for_test(), state);
        assert_eq!((core.transport.len(), core.transport.accepted()), (4, 12));
        assert!(
            receipts
                .iter()
                .all(|r| r.outcome() == SAPostOutcome::Pending)
        );
        let child = core.transport.detach(4, &mut core.post_batches).unwrap();
        complete(&mut core, child);
        complete(&mut core, outer);
        check_idle(&core);
    }
}

#[test]
fn inline_parent_and_deeper_or_oversized_batches_do_not_consume_or_evict_tickets() {
    let k = limit::<Payload>();
    let (mut core, recipient) = fixture(k + 16);
    let receipts = enqueue(&core, recipient, 1);
    let inline = core.transport.detach(1, &mut core.post_batches).unwrap();
    assert_eq!(core.post_batches.state_for_test(), ([0, 0], [0, 0], 0, 0));
    let mut active = [None, None];
    for slot in &mut active {
        enqueue(&core, recipient, 2);
        *slot = Some(core.transport.detach(2, &mut core.post_batches).unwrap());
    }
    let participating = core.post_batches.state_for_test();
    assert_eq!(participating.1, [2, 2]);
    for count in [3, k + 1] {
        let fallback_receipts = enqueue(&core, recipient, count);
        let batch = core
            .transport
            .detach(count, &mut core.post_batches)
            .unwrap();
        assert_eq!(core.post_batches.state_for_test(), participating);
        complete(&mut core, batch);
        assert_eq!(core.post_batches.state_for_test(), participating);
        assert!(
            fallback_receipts
                .iter()
                .all(|r| r.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
        );
    }
    for batch in active.into_iter().rev().flatten() {
        complete(&mut core, batch);
    }
    let retained = core.post_batches.state_for_test();
    // An oversized checkout with free records also bypasses selection and demand.
    let oversized = enqueue(&core, recipient, k + 1);
    let batch = core
        .transport
        .detach(k + 1, &mut core.post_batches)
        .unwrap();
    complete(&mut core, batch);
    assert_eq!(core.post_batches.state_for_test(), retained);
    assert!(
        oversized
            .iter()
            .all(|r| r.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
    );
    complete(&mut core, inline);
    assert_eq!(
        receipts[0].outcome(),
        SAPostOutcome::Delivered { callbacks: 0 }
    );
    check_idle(&core);
}

#[test]
fn each_rebalance_failure_and_actual_capacity_overshoot_preserves_existing_idle_and_receipts() {
    let k = limit::<Payload>();
    for (fail, overshoot, expected_attempts) in [
        (Some(1), None, 1),
        (Some(2), None, 2),
        (None, Some(1), 2),
        (None, Some(2), 2),
        (None, None, 2),
    ] {
        let (mut core, recipient) = fixture(k);
        core.post_batches
            .seed_for_test(VecDeque::with_capacity(k * 80 / 100));
        core.post_batches
            .seed_for_test(VecDeque::with_capacity(k * 19 / 100));
        let receipts = enqueue(&core, recipient, k * 59 / 100 + k * 35 / 100);
        let outer = core
            .transport
            .detach(k * 59 / 100, &mut core.post_batches)
            .unwrap();
        let child = core
            .transport
            .detach(k * 35 / 100, &mut core.post_batches)
            .unwrap();
        complete(&mut core, child);
        let idle = core.post_batches.retained_capacities();
        core.post_batches
            .configure_rebalance_for_test(fail, overshoot);
        let (_, measured) = crate::allocation_probe::measure(|| complete(&mut core, outer));
        assert_eq!(
            core.post_batches.rebalance_reservations_for_test(),
            expected_attempts
        );
        assert_eq!(
            calls(measured),
            expected_attempts - usize::from(fail.is_some())
        );
        if fail.is_some() || overshoot.is_some() {
            assert_eq!(core.post_batches.retained_capacities(), idle);
        } else {
            let capacities = core.post_batches.retained_capacities();
            assert!(capacities.iter().all(|capacity| *capacity != 0));
            assert!(capacities.contains(&(k * 35 / 100)));
        }
        assert!(
            receipts
                .iter()
                .all(|r| r.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
        );
        check_idle(&core);
    }
}

#[test]
fn individually_overlimit_actual_return_is_dropped_without_rebalance() {
    let k = limit::<Payload>();
    let (mut core, recipient) = fixture(2);
    enqueue(&core, recipient, 2);
    let mut batch = core.transport.detach(2, &mut core.post_batches).unwrap();
    empty(&mut core, &mut batch);
    let Batch::Many(buffer) = &mut batch else {
        panic!("many frontier required")
    };
    buffer.reserve_empty_for_test(k + 1);
    core.post_batches.seed_for_test(VecDeque::with_capacity(2));
    let before = core.post_batches.retained_capacities();
    let (_, measured) =
        crate::allocation_probe::measure(|| core.post_batches.complete(batch, true));
    assert_eq!(calls(measured), 0);
    assert_eq!(core.post_batches.retained_capacities(), before);
    check_idle(&core);
}

fn cold_layout<M: Send + Clone + 'static>(message: M, expected_capacity: usize) {
    let mut core = Core::<PlainApp<M>>::with_config(&SAHostConfig {
        post_capacity: 2,
        ..SAHostConfig::default()
    })
    .unwrap();
    let recipient = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
        .create_recipient()
        .unwrap();
    let receipts = [
        core.proxy().try_post(recipient, message.clone()).unwrap(),
        core.proxy().try_post(recipient, message).unwrap(),
    ];
    let (mut batch, measured) = crate::allocation_probe::measure(|| {
        core.transport.detach(2, &mut core.post_batches).unwrap()
    });
    let Batch::Many(buffer) = &batch else {
        panic!("many frontier required")
    };
    assert_eq!(buffer.capacity(), expected_capacity);
    assert_eq!(calls(measured), 1);
    assert_eq!(
        measured.requested_bytes,
        expected_capacity * std::mem::size_of::<Post<M>>()
    );
    for post in batch.by_ref() {
        post.receipt
            .settle(SAPostOutcome::Delivered { callbacks: 0 });
        core.dispose(post.message);
        core.transport.settled();
    }
    core.post_batches.complete(batch, true);
    assert_eq!(core.post_batches.state_for_test().1, [0, 0]);
    assert!(
        core.post_batches
            .retained_capacities()
            .iter()
            .sum::<usize>()
            <= limit::<M>()
    );
    assert!(
        receipts
            .iter()
            .all(|r| r.outcome() == SAPostOutcome::Delivered { callbacks: 0 })
    );
}
#[test]
fn cold_sizing_uses_real_element_layout_including_zero_and_large_inline_payloads() {
    cold_layout((), 4);
    cold_layout([0_u8; 48], 4);
    cold_layout([0_u8; 2048], 2);
    cold_layout([0_u8; 65536], 2);
}

#[test]
fn private_post_layout_matches_optimized_measurement_surrogates() {
    assert_eq!(std::mem::size_of::<Post<[u8; 32]>>(), 64);
    assert_eq!(std::mem::size_of::<Post<[u8; 256]>>(), 288);
    assert_eq!(std::mem::align_of::<Post<[u8; 32]>>(), 8);
    assert_eq!(std::mem::align_of::<Post<[u8; 256]>>(), 8);
    println!(
        "private instrumented layouts: Post32={} Post256={} Batch32={} Batch256={} BatchCache32={} BatchCache256={}; cache includes cfg(test) failure instrumentation",
        std::mem::size_of::<Post<[u8; 32]>>(),
        std::mem::size_of::<Post<[u8; 256]>>(),
        std::mem::size_of::<Batch<[u8; 32]>>(),
        std::mem::size_of::<Batch<[u8; 256]>>(),
        std::mem::size_of::<crate::post::BatchCache<[u8; 32]>>(),
        std::mem::size_of::<crate::post::BatchCache<[u8; 256]>>()
    );
}

struct OwnedPayload {
    value: u8,
    drops: std::sync::Arc<std::sync::Mutex<Vec<(u8, std::thread::ThreadId)>>>,
    panic_drop: bool,
    reentry: Option<DisposalReentry>,
}
struct DisposalReentry {
    proxy: SAProxy<OwnedPayload>,
    recipient: SARecipientId,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<SAPostReceipt, SAError>>>>,
}
impl Drop for OwnedPayload {
    fn drop(&mut self) {
        self.drops
            .lock()
            .unwrap()
            .push((self.value, std::thread::current().id()));
        assert!(!self.panic_drop, "qualification destructor fault");
        if let Some(reentry) = self.reentry.take() {
            let result = reentry
                .proxy
                .try_post(
                    reentry.recipient,
                    OwnedPayload {
                        value: 99,
                        drops: self.drops.clone(),
                        panic_drop: false,
                        reentry: None,
                    },
                )
                .map_err(|rejected| rejected.reason().clone());
            *reentry.result.lock().unwrap() = Some(result);
        }
    }
}
struct ClosingApp {
    recipient: Option<SARecipientId>,
    trace: Vec<u8>,
    drops: std::sync::Arc<std::sync::Mutex<Vec<(u8, std::thread::ThreadId)>>>,
    created: Vec<SAPostReceipt>,
    native: Option<crate::backend::test::TestBackend>,
    mode: u8,
}
impl SAApplication for ClosingApp {
    type Message = OwnedPayload;
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}
impl ClosingApp {
    fn payload(&self, value: u8) -> OwnedPayload {
        OwnedPayload {
            value,
            drops: self.drops.clone(),
            panic_drop: self.mode == 4 && value == 3,
            reentry: None,
        }
    }
}
fn close_inside_callback(app: &mut ClosingApp, cx: &mut SAContext<'_, ClosingApp>) {
    let mut native = app.native.take().unwrap();
    cx.request_stop();
    let outstanding_before = cx.core.post_batches.state_for_test().1;
    if app.mode == 2 {
        // Destruction observed while Stopping removes the ledger entry before
        // finish/reap reach closure; this also exercises the backend-fault path.
        let key = cx.core.native_windows[0].0;
        cx.core.native_destroyed(key);
    }
    cx.core.poll_stop(app, BackendOps::Test(&mut native));
    if app.mode == 1 {
        assert_eq!(cx.core.state, SAHostState::Retiring);
        cx.core
            .native_destroyed(native.complete_destruction().unwrap());
    }
    assert_eq!(cx.core.state, SAHostState::Closed);
    assert_eq!(cx.core.post_batches.retained_capacities(), [0, 0]);
    assert_eq!(cx.core.post_batches.state_for_test().1, outstanding_before);
    assert!(outstanding_before.iter().any(|requested| *requested != 0));
    // Ticket accounting is deliberately not a shutdown readiness requirement.
    app.native = Some(native);
}
fn closing_handler(
    app: &mut ClosingApp,
    cx: &mut SAContext<'_, ClosingApp>,
    event: &SAEvent<'_, OwnedPayload, ()>,
) -> SAPropagation {
    let SAEvent::Posted { message, receipt } = event else {
        return SAPropagation::Continue;
    };
    assert_eq!(receipt.outcome(), SAPostOutcome::Pending);
    app.trace.push(message.value);
    if message.value == 1 {
        assert_eq!(
            cx.core
                .post_batches
                .state_for_test()
                .1
                .iter()
                .filter(|n| **n != 0)
                .count(),
            1
        );
        for value in [3, 4, 5] {
            app.created.push(
                cx.proxy()
                    .try_post(app.recipient.unwrap(), app.payload(value))
                    .unwrap(),
            );
        }
        let result = cx.drain_posts(app, 2);
        assert_eq!(result.is_err(), app.mode == 3);
        if app.mode >= 3 {
            close_inside_callback(app, cx);
        }
        assert_eq!(cx.core.state, SAHostState::Closed);
        assert_eq!(
            cx.core
                .post_batches
                .state_for_test()
                .1
                .iter()
                .filter(|n| **n != 0)
                .count(),
            1
        );
        assert_eq!(cx.core.transport.accepted(), 2);
    } else if message.value == 3 {
        assert_eq!(cx.core.post_batches.state_for_test().1, [2, 2]);
        assert_eq!(
            cx.core.transport.len(),
            1,
            "later queue arrival excluded from child frontier"
        );
        assert_eq!(cx.core.transport.accepted(), 5);
        assert!(app.mode != 3, "qualification callback fault");
        if app.mode < 3 {
            close_inside_callback(app, cx);
        }
    }
    SAPropagation::Continue
}

fn reentry_handler(
    app: &mut ClosingApp,
    cx: &mut SAContext<'_, ClosingApp>,
    event: &SAEvent<'_, OwnedPayload, ()>,
) -> SAPropagation {
    if let SAEvent::Posted { message, receipt } = event {
        assert_eq!(receipt.outcome(), SAPostOutcome::Pending);
        app.trace.push(message.value);
        if message.value == 1 {
            assert_eq!(cx.core.post_batches.state_for_test().1, [2, 0]);
            assert_eq!(cx.core.transport.accepted(), 2);
            assert_eq!(cx.core.transport.len(), 0);
        }
    }
    SAPropagation::Continue
}

#[test]
fn many_frontier_holds_ticket_and_accepted_capacity_across_destructor_post_reentry() {
    for capacity in [2, 3] {
        let mut core = Core::<ClosingApp>::with_config(&SAHostConfig {
            post_capacity: capacity,
            ..SAHostConfig::default()
        })
        .unwrap();
        let mut app = ClosingApp {
            recipient: None,
            trace: Vec::with_capacity(4),
            drops: std::sync::Arc::new(std::sync::Mutex::new(Vec::with_capacity(4))),
            created: Vec::new(),
            native: None,
            mode: 0,
        };
        let recipient = {
            let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
            let recipient = cx.create_recipient().unwrap();
            cx.subscribe(
                recipient,
                SAEventFilter::Posted,
                SAPriority::default(),
                reentry_handler,
            )
            .unwrap();
            recipient
        };
        let observation = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut first = app.payload(1);
        first.reentry = Some(DisposalReentry {
            proxy: core.proxy(),
            recipient,
            result: observation.clone(),
        });
        let receipts = [
            core.proxy().try_post(recipient, first).unwrap(),
            core.proxy().try_post(recipient, app.payload(2)).unwrap(),
        ];
        assert_eq!(
            SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
                .drain_posts(&mut app, 2),
            Ok(2)
        );
        assert_eq!(app.trace, [1, 2]);
        assert_eq!(core.post_batches.state_for_test().1, [0, 0]);
        assert!(
            receipts
                .iter()
                .all(|receipt| receipt.outcome() == SAPostOutcome::Delivered { callbacks: 1 })
        );
        let outcome = observation.lock().unwrap().take().unwrap();
        let expected = if capacity == 2 {
            assert_eq!(outcome.unwrap_err(), SAError::CapacityFull);
            assert_eq!((core.transport.accepted(), core.transport.len()), (0, 0));
            [1, 99, 2]
        } else {
            let receipt = outcome.unwrap();
            assert_eq!(receipt.outcome(), SAPostOutcome::Pending);
            assert_eq!((core.transport.accepted(), core.transport.len()), (1, 1));
            assert_eq!(
                SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
                    .drain_posts(&mut app, 2),
                Ok(1)
            );
            assert_eq!(app.trace, [1, 2, 99]);
            assert_eq!(receipt.outcome(), SAPostOutcome::Delivered { callbacks: 1 });
            [1, 2, 99]
        };
        assert_eq!((core.transport.accepted(), core.transport.len()), (0, 0));
        assert_eq!(core.post_batches.state_for_test().1, [0, 0]);
        assert_eq!(
            app.drops.lock().unwrap().as_slice(),
            &expected.map(|value| (value, std::thread::current().id()))
        );
    }
}

#[test]
fn nested_context_closure_preserves_outstanding_tickets_and_owner_disposal_across_faults() {
    for mode in 0..5 {
        let mut core = Core::<ClosingApp>::new().unwrap();
        let mut native = crate::backend::test::TestBackend::default();
        let mut app = ClosingApp {
            recipient: None,
            trace: Vec::with_capacity(8),
            drops: std::sync::Arc::new(std::sync::Mutex::new(Vec::with_capacity(8))),
            created: Vec::with_capacity(3),
            native: None,
            mode,
        };
        let recipient = {
            let mut cx = SAContext::new(
                &mut core,
                BackendOps::Test(&mut native),
                SAContextPhase::Startup,
            );
            if mode == 1 || mode == 2 {
                cx.create_window(SAWindowSpec::default()).unwrap();
            }
            let recipient = cx.create_recipient().unwrap();
            cx.subscribe(
                recipient,
                SAEventFilter::Posted,
                SAPriority::default(),
                closing_handler,
            )
            .unwrap();
            recipient
        };
        app.recipient = Some(recipient);
        app.native = Some(native);
        // Two seeded buffers ensure callback/closure work need not allocate batch
        // backing. Configure failures to prove closure never attempts replacement.
        for _ in 0..2 {
            core.post_batches.seed_for_test(VecDeque::with_capacity(2));
        }
        core.post_batches
            .configure_rebalance_for_test(Some(1), Some(1));
        let outer = [1, 2].map(|value| {
            core.proxy()
                .try_post(recipient, app.payload(value))
                .unwrap()
        });
        assert_eq!(
            SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event)
                .drain_posts(&mut app, 2),
            Ok(2)
        );
        assert_eq!(core.state, SAHostState::Closed);
        assert_eq!(core.post_batches.state_for_test().1, [0, 0]);
        assert_eq!(core.post_batches.retained_capacities(), [0, 0]);
        assert_eq!(core.post_batches.rebalance_reservations_for_test(), 0);
        assert_eq!((core.transport.len(), core.transport.accepted()), (0, 0));
        assert_eq!(app.trace, [1, 3]);
        assert_eq!(
            outer[0].outcome(),
            SAPostOutcome::Delivered { callbacks: 1 }
        );
        let discarded = SAPostOutcome::Discarded(SAPostDiscardReason::HostStopped);
        assert_eq!(outer[1].outcome(), discarded);
        assert_eq!(app.created[1].outcome(), discarded);
        assert_eq!(app.created[2].outcome(), discarded);
        if mode == 3 {
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
        let owner = std::thread::current().id();
        let expected = if mode < 3 {
            [5, 3, 4, 1, 2]
        } else {
            [3, 4, 5, 1, 2]
        };
        assert_eq!(
            app.drops.lock().unwrap().as_slice(),
            &expected.map(|value| (value, owner))
        );
        // A new drain after closure is rejected without allocating or reviving
        // backing; outstanding drains completed through the earlier return path.
        let (result, measured) = crate::allocation_probe::measure(|| {
            SAContext::new(
                &mut core,
                BackendOps::Unavailable,
                SAContextPhase::Retirement,
            )
            .drain_posts(&mut app, 8)
        });
        assert_eq!(result, Err(SAError::AdmissionClosed));
        assert_eq!(calls(measured), 0);
        assert_eq!(core.post_batches.retained_capacities(), [0, 0]);
    }
}
