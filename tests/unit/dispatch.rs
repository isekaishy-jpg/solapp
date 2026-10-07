use crate::backend::BackendOps;
use crate::host::Core;
use crate::*;

#[test]
#[ignore = "release-build measurement; no timing thresholds"]
fn recipient_retirement_scaling_measurement() {
    use std::{hint::black_box, time::Instant};
    for (peak, live, depth) in [
        (128, 128, 0),
        (1024, 1024, 0),
        (4096, 4096, 0),
        (16384, 16384, 0),
        (4096, 4096, 8),
        (32768, 128, 0),
    ] {
        for trial in 0..7 {
            let mut core = Core::<App>::new().unwrap();
            let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
            let mut recipient = cx.create_recipient().unwrap();
            if peak > live {
                for _ in 0..peak {
                    cx.subscribe(
                        recipient,
                        SAEventFilter::Local,
                        SAPriority::default(),
                        first,
                    )
                    .unwrap();
                }
                cx.retire_recipient(recipient).unwrap();
                recipient = cx.create_recipient().unwrap();
            }
            for _ in 0..live {
                cx.subscribe(
                    recipient,
                    SAEventFilter::Local,
                    SAPriority::default(),
                    first,
                )
                .unwrap();
            }
            for _ in 0..depth {
                cx.core.dispatch.begin().unwrap();
            }
            let start = Instant::now();
            black_box(cx.retire_recipient(black_box(recipient)).unwrap());
            println!(
                "recipient-retirement peak={peak} live={live} depth={depth} trial={trial} elapsed_ns={}",
                start.elapsed().as_nanos()
            );
            for marker in (0..depth).rev() {
                cx.core.dispatch.end(marker);
            }
        }
    }
}

#[test]
fn recipient_compaction_matches_repeated_unsubscribe_for_every_nested_gap_and_removal_mask() {
    use crate::dispatch::{Dispatch, Recipient};
    fn registry(
        host: SAHostId,
        mask: usize,
    ) -> (Dispatch<App>, [SARecipientId; 2], Vec<SASubscriptionId>) {
        let mut dispatch = Dispatch::new();
        let recipients = std::array::from_fn(|_| {
            let key = dispatch.recipients.reserve().unwrap();
            dispatch.recipients.insert(
                key,
                Recipient {
                    alive: true,
                    active: 0,
                },
            );
            SARecipientId { host, key }
        });
        let ids = (0..6)
            .map(|index| {
                dispatch
                    .subscribe(
                        host,
                        recipients[usize::from(mask & (1 << index) == 0)],
                        SAEventFilter::Local,
                        SAPriority::new((6 - index) as f64).unwrap(),
                        first,
                    )
                    .unwrap()
            })
            .collect();
        (dispatch, recipients, ids)
    }
    let host = SAHostId::allocate().unwrap();
    for mask in 0..64 {
        for outer_gap in 0..=6 {
            for inner_gap in 0..=6 {
                let (mut actual, recipients, ids) = registry(host, mask);
                let (mut reference, _, _) = registry(host, mask);
                // Nested callbacks can retain the same subscription more than once.
                for _ in 0..2 {
                    actual.claim(ids[2].key);
                    reference.claim(ids[2].key);
                }
                for gap in [outer_gap, inner_gap, 6 - outer_gap] {
                    let a = actual.begin().unwrap();
                    let b = reference.begin().unwrap();
                    for _ in 0..gap {
                        assert_eq!(
                            actual.next(a, None, SAEventFilter::Local),
                            reference.next(b, None, SAEventFilter::Local)
                        );
                    }
                }
                assert_eq!(
                    actual.retire_recipient(host, recipients[0]),
                    reference.reference_retire_recipient(host, recipients[0])
                );
                actual.assert_same_registry(&reference);
                let expected: Vec<_> = [outer_gap, inner_gap, 6 - outer_gap]
                    .into_iter()
                    .map(|gap| gap - (0..gap).filter(|index| mask & (1 << index) != 0).count())
                    .collect();
                assert_eq!(actual.test_markers(), expected);
                // Insert before, at and after surviving gaps, including equal priorities.
                // Freed slot choice can differ: compare routed handlers and recipient incarnations.
                for priority in [20.0, 6.0, 5.5, 5.0, 4.0, 3.0, 2.0, 1.0, -1.0] {
                    actual
                        .subscribe(
                            host,
                            recipients[1],
                            SAEventFilter::Local,
                            SAPriority::new(priority).unwrap(),
                            second,
                        )
                        .unwrap();
                    reference
                        .subscribe(
                            host,
                            recipients[1],
                            SAEventFilter::Local,
                            SAPriority::new(priority).unwrap(),
                            second,
                        )
                        .unwrap();
                    assert_eq!(actual.test_markers(), reference.test_markers());
                    actual.assert_same_routing_order(&reference);
                }
                for marker in (0..3).rev() {
                    loop {
                        let a = actual.next(marker, None, SAEventFilter::Local);
                        let b = reference.next(marker, None, SAEventFilter::Local);
                        match (a, b) {
                            (Some(a), Some(b)) => {
                                let (ah, ar) = actual.claim(a);
                                let (bh, br) = reference.claim(b);
                                assert_eq!((ah as usize, ar), (bh as usize, br));
                                actual.release(a, ar);
                                reference.release(b, br);
                            }
                            (None, None) => break,
                            _ => panic!("different nested callback trace"),
                        }
                    }
                    actual.end(marker);
                    reference.end(marker);
                }
                let active_recipient = if mask & 4 != 0 {
                    recipients[0]
                } else {
                    recipients[1]
                };
                for remaining in (0..2).rev() {
                    actual.release(ids[2].key, active_recipient);
                    reference.release(ids[2].key, active_recipient);
                    if active_recipient == recipients[0] {
                        assert_eq!(
                            actual
                                .recipient(host, active_recipient)
                                .map(|record| record.active),
                            if remaining == 0 {
                                Err(SAError::StaleIdentity)
                            } else {
                                Ok(remaining)
                            }
                        );
                    }
                }
                assert_eq!(
                    actual.live_recipient(host, recipients[0]),
                    Err(SAError::StaleIdentity)
                );
                assert_eq!(
                    actual.unsubscribe(host, ids[2]),
                    reference.unsubscribe(host, ids[2])
                );
            }
        }
    }
}

#[test]
fn retirement_visits_occupied_order_once_and_each_removed_entry_checks_active_markers() {
    for (peak, live, depth) in [
        (128, 128, 0),
        (1024, 1024, 1),
        (4096, 4096, 8),
        (65536, 128, 8),
    ] {
        let mut core = Core::<App>::new().unwrap();
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        let old = cx.create_recipient().unwrap();
        for _ in 0..peak {
            cx.subscribe(old, SAEventFilter::Local, SAPriority::default(), first)
                .unwrap();
        }
        cx.retire_recipient(old).unwrap();
        let retired = cx.create_recipient().unwrap();
        let survivor = cx.create_recipient().unwrap();
        for index in 0..live {
            cx.subscribe(
                if index % 2 == 0 { retired } else { survivor },
                SAEventFilter::Local,
                SAPriority::default(),
                first,
            )
            .unwrap();
        }
        for _ in 0..depth {
            cx.core.dispatch.begin().unwrap();
        }
        let before = cx.core.dispatch.test_retirement_steps();
        let (_, allocation) = crate::allocation_probe::measure(|| {
            cx.retire_recipient(retired).unwrap();
        });
        let after = cx.core.dispatch.test_retirement_steps();
        assert_eq!(after.0 - before.0, live);
        assert_eq!(after.1 - before.1, (live / 2) * depth);
        assert_eq!(allocation.allocations + allocation.reallocations, 0);
        println!(
            "retirement-work historical_peak={peak} live={live} removed={} depth={depth} order_visits={} marker_checks={} allocations=0 reallocations=0",
            live / 2,
            after.0 - before.0,
            after.1 - before.1
        );
        for marker in (0..depth).rev() {
            cx.core.dispatch.end(marker);
        }
        cx.retire_recipient(survivor).unwrap();
    }
}

#[derive(Clone, Copy)]
enum Action {
    None,
    Insert,
    RemoveSelf,
    RemoveFuture,
    RemoveAll,
    Nested,
    Retire,
    Stop,
    Limit,
    RemoveSelfInsert,
    RemoveAllInsert,
    NestedMutation,
}
struct App {
    trace: Vec<u8>,
    action: Action,
    mutated: bool,
    recipient: Option<SARecipientId>,
    ids: Vec<SASubscriptionId>,
}
impl App {
    fn new(action: Action) -> Self {
        Self {
            trace: Vec::new(),
            action,
            mutated: false,
            recipient: None,
            ids: Vec::new(),
        }
    }
}
impl SAApplication for App {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}
fn trace(app: &mut App, event: &SAEvent<'_, String, u8>, value: u8) {
    let nested = matches!(event, SAEvent::Local(1));
    app.trace.push(value + if nested { 10 } else { 0 });
}
fn first(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, String, u8>,
) -> SAPropagation {
    trace(app, event, 1);
    if !app.mutated {
        app.mutated = true;
        match app.action {
            Action::None => (),
            Action::Insert => {
                for (priority, handler) in [
                    (20.0, high as SAHandler<App>),
                    (10.0, equal),
                    (5.0, middle),
                    (-5.0, low),
                ] {
                    cx.subscribe(
                        app.recipient.unwrap(),
                        SAEventFilter::Local,
                        SAPriority::new(priority).unwrap(),
                        handler,
                    )
                    .unwrap();
                }
            }
            Action::RemoveSelf => cx.unsubscribe(app.ids[0]).unwrap(),
            Action::RemoveSelfInsert => {
                cx.unsubscribe(app.ids[0]).unwrap();
                cx.subscribe(
                    app.recipient.unwrap(),
                    SAEventFilter::Local,
                    SAPriority::new(20.0).unwrap(),
                    high,
                )
                .unwrap();
                cx.subscribe(
                    app.recipient.unwrap(),
                    SAEventFilter::Local,
                    SAPriority::new(10.0).unwrap(),
                    equal,
                )
                .unwrap();
            }
            Action::RemoveFuture => cx.unsubscribe(app.ids[1]).unwrap(),
            Action::RemoveAll => {
                for id in &app.ids {
                    cx.unsubscribe(*id).unwrap();
                }
            }
            Action::RemoveAllInsert => {
                for id in &app.ids {
                    cx.unsubscribe(*id).unwrap();
                }
                cx.subscribe(
                    app.recipient.unwrap(),
                    SAEventFilter::Local,
                    SAPriority::new(20.0).unwrap(),
                    high,
                )
                .unwrap();
                cx.subscribe(
                    app.recipient.unwrap(),
                    SAEventFilter::Local,
                    SAPriority::new(10.0).unwrap(),
                    equal,
                )
                .unwrap();
            }
            Action::Nested => {
                cx.dispatch_local(app, &1).unwrap();
            }
            Action::NestedMutation => {
                cx.dispatch_local(app, &1).unwrap();
            }
            Action::Retire => {
                let recipient = app.recipient.unwrap();
                assert_eq!(cx.retire_recipient(recipient).unwrap().active_callbacks, 1);
                assert_eq!(
                    cx.recipient_state(recipient).unwrap(),
                    SARecipientState {
                        retired: true,
                        active_callbacks: 1
                    }
                );
                let new_recipient = cx.create_recipient().unwrap();
                assert_ne!(new_recipient, recipient);
                assert_eq!(
                    cx.subscribe(
                        recipient,
                        SAEventFilter::Local,
                        SAPriority::default(),
                        second
                    ),
                    Err(SAError::StaleIdentity)
                );
            }
            Action::Stop => return SAPropagation::Stop,
            Action::Limit => {
                assert_eq!(cx.dispatch_local(app, &1), Err(SAError::NestingLimit));
            }
        }
    }
    SAPropagation::Continue
}
fn second(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, String, u8>,
) -> SAPropagation {
    trace(app, event, 2);
    if matches!(app.action, Action::NestedMutation) && matches!(event, SAEvent::Local(1)) {
        cx.unsubscribe(app.ids[0]).unwrap();
        cx.subscribe(
            app.recipient.unwrap(),
            SAEventFilter::Local,
            SAPriority::new(20.0).unwrap(),
            high,
        )
        .unwrap();
    }
    SAPropagation::Continue
}
fn third(
    app: &mut App,
    _: &mut SAContext<'_, App>,
    event: &SAEvent<'_, String, u8>,
) -> SAPropagation {
    trace(app, event, 3);
    SAPropagation::Continue
}
fn high(app: &mut App, _: &mut SAContext<'_, App>, _: &SAEvent<'_, String, u8>) -> SAPropagation {
    app.trace.push(4);
    SAPropagation::Continue
}
fn equal(app: &mut App, _: &mut SAContext<'_, App>, _: &SAEvent<'_, String, u8>) -> SAPropagation {
    app.trace.push(5);
    SAPropagation::Continue
}
fn middle(app: &mut App, _: &mut SAContext<'_, App>, _: &SAEvent<'_, String, u8>) -> SAPropagation {
    app.trace.push(6);
    SAPropagation::Continue
}
fn low(app: &mut App, _: &mut SAContext<'_, App>, _: &SAEvent<'_, String, u8>) -> SAPropagation {
    app.trace.push(7);
    SAPropagation::Continue
}

#[test]
fn native_selected_mutation_frontiers_and_nested_traversal() {
    for (action, expected) in [
        (Action::None, vec![1, 2, 3]),
        (Action::Insert, vec![1, 6, 2, 3, 7]),
        (Action::RemoveSelf, vec![1, 2, 3]),
        (Action::RemoveFuture, vec![1, 3]),
        (Action::RemoveAll, vec![1]),
        (Action::Nested, vec![1, 11, 12, 13, 2, 3]),
        (Action::Retire, vec![1]),
        (Action::Stop, vec![1]),
        (Action::Limit, vec![1, 2, 3]),
        (Action::RemoveSelfInsert, vec![1, 4, 5, 2, 3]),
        (Action::RemoveAllInsert, vec![1, 4, 5]),
        (Action::NestedMutation, vec![1, 11, 12, 13, 4, 2, 3]),
    ] {
        let mut core = Core::<App>::new().unwrap();
        if matches!(action, Action::Limit) {
            core.nesting_limit = 1;
        }
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        let mut app = App::new(action);
        let recipient = cx.create_recipient().unwrap();
        app.recipient = Some(recipient);
        for (priority, handler) in [
            (10.0, first as SAHandler<App>),
            (0.0, second),
            (-1.0, third),
        ] {
            app.ids.push(
                cx.subscribe(
                    recipient,
                    SAEventFilter::Local,
                    SAPriority::new(priority).unwrap(),
                    handler,
                )
                .unwrap(),
            );
        }
        let report = cx.dispatch_local(&mut app, &0).unwrap();
        assert_eq!(app.trace, expected);
        assert_eq!(
            report.propagation,
            if matches!(action, Action::Stop) {
                SAPropagation::Stop
            } else {
                SAPropagation::Continue
            }
        );
        if matches!(action, Action::Insert) {
            app.trace.clear();
            cx.dispatch_local(&mut app, &0).unwrap();
            assert_eq!(app.trace, [4, 5, 1, 6, 2, 3, 7]);
        }
        if matches!(action, Action::Retire) {
            assert_eq!(cx.recipient_state(recipient), Err(SAError::StaleIdentity));
        }
    }
}

#[test]
fn finite_equal_priorities_include_signed_zero_and_stale_subscriptions_cannot_remove_reuse() {
    for (left, right) in [(10.0, 10.0), (-0.0, 0.0), (0.0, -0.0)] {
        let mut core = Core::<App>::new().unwrap();
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        let recipient = cx.create_recipient().unwrap();
        let old = cx
            .subscribe(
                recipient,
                SAEventFilter::Local,
                SAPriority::new(left).unwrap(),
                first,
            )
            .unwrap();
        cx.subscribe(
            recipient,
            SAEventFilter::Local,
            SAPriority::new(right).unwrap(),
            second,
        )
        .unwrap();
        let mut app = App::new(Action::None);
        cx.dispatch_local(&mut app, &0).unwrap();
        assert_eq!(app.trace, [2, 1]);
        cx.unsubscribe(old).unwrap();
        let new = cx
            .subscribe(
                recipient,
                SAEventFilter::Local,
                SAPriority::default(),
                third,
            )
            .unwrap();
        assert_ne!(old, new);
        assert_eq!(cx.unsubscribe(old), Err(SAError::StaleIdentity));
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(SAPriority::new(value).is_err());
    }
}

#[test]
fn callback_panic_releases_active_retention_and_faults_ordinary_admission() {
    fn panics(
        _: &mut App,
        cx: &mut SAContext<'_, App>,
        _: &SAEvent<'_, String, u8>,
    ) -> SAPropagation {
        assert_eq!(cx.core.dispatch.depth, 1);
        panic!("handler failure");
    }
    let mut core = Core::<App>::new().unwrap();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Local,
        SAPriority::default(),
        panics,
    )
    .unwrap();
    let result = cx.dispatch_local(&mut App::new(Action::None), &0);
    assert!(matches!(result, Err(SAError::ApplicationPanicked { .. })));
    assert_eq!(cx.recipient_state(recipient).unwrap().active_callbacks, 0);
    assert_eq!(cx.core.dispatch.depth, 0);
    assert!(!cx.admission().ordinary_open);
}

#[test]
fn removed_current_record_reanchors_to_remaining_visited_ordinary_records() {
    fn mutate(
        app: &mut App,
        cx: &mut SAContext<'_, App>,
        _: &SAEvent<'_, String, u8>,
    ) -> SAPropagation {
        app.trace.push(1);
        cx.unsubscribe(app.ids[0]).unwrap();
        cx.subscribe(
            app.recipient.unwrap(),
            SAEventFilter::Local,
            SAPriority::new(40.0).unwrap(),
            high,
        )
        .unwrap();
        cx.subscribe(
            app.recipient.unwrap(),
            SAEventFilter::Local,
            SAPriority::new(20.0).unwrap(),
            middle,
        )
        .unwrap();
        SAPropagation::Continue
    }
    let mut core = Core::<App>::new().unwrap();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Local,
        SAPriority::new(30.0).unwrap(),
        third,
    )
    .unwrap();
    let current = cx
        .subscribe(
            recipient,
            SAEventFilter::Local,
            SAPriority::new(10.0).unwrap(),
            mutate,
        )
        .unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Local,
        SAPriority::default(),
        second,
    )
    .unwrap();
    let mut app = App::new(Action::None);
    app.recipient = Some(recipient);
    app.ids.push(current);
    cx.dispatch_local(&mut app, &0).unwrap();
    assert_eq!(app.trace, [3, 1, 6, 2]);
    // New high40 is before the surviving visited30 anchor; middle20 is after
    // that anchor's marker and is admitted into this same outer traversal.
}

#[test]
fn failed_subscription_order_admission_leaves_candidate_reusable_and_existing_order_live() {
    let mut core = Core::<App>::new().unwrap();
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    for _ in 0..3 {
        cx.core.dispatch.fail_next_order_reservation = true;
        assert_eq!(
            cx.subscribe(
                recipient,
                SAEventFilter::Local,
                SAPriority::default(),
                first
            ),
            Err(SAError::AllocationFailed)
        );
    }
    let old = cx
        .subscribe(
            recipient,
            SAEventFilter::Local,
            SAPriority::default(),
            first,
        )
        .unwrap();
    assert_eq!(old.key.index, 0);
    assert_eq!(old.key.generation, 1);
    cx.subscribe(
        recipient,
        SAEventFilter::Local,
        SAPriority::new(10.0).unwrap(),
        second,
    )
    .unwrap();
    cx.unsubscribe(old).unwrap();
    cx.core.dispatch.fail_next_order_reservation = true;
    assert_eq!(
        cx.subscribe(
            recipient,
            SAEventFilter::Local,
            SAPriority::new(500.0).unwrap(),
            first
        ),
        Err(SAError::AllocationFailed)
    );
    let replacement = cx
        .subscribe(
            recipient,
            SAEventFilter::Local,
            SAPriority::default(),
            third,
        )
        .unwrap();
    assert_eq!(replacement.key.index, old.key.index);
    assert_eq!(replacement.key.generation, old.key.generation + 1);
    assert_eq!(cx.unsubscribe(old), Err(SAError::StaleIdentity));
    let mut app = App::new(Action::None);
    cx.dispatch_local(&mut app, &0).unwrap();
    assert_eq!(app.trace, [2, 3]);
}
