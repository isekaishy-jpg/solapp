use crate::backend::BackendOps;
use crate::host::Core;
use crate::*;
use std::time::Duration;

enum Action {
    AddDue,
    CancelOther,
    Advance(bool),
    BudgetChain,
    Limit,
}
struct App {
    action: Action,
    recipient: Option<SARecipientId>,
    other: Option<SATimerId>,
    trace: Vec<(u8, Duration)>,
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
fn handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    event: &SAEvent<'_, String, u8>,
) -> SAPropagation {
    let SAEvent::Timer {
        id, sample, event, ..
    } = event
    else {
        return SAPropagation::Continue;
    };
    app.trace.push((**event, sample.elapsed()));
    assert!(matches!(cx.cancel_timer(*id), Ok(SATimerCancel::Claimed)));
    match app.action {
        Action::AddDue if **event == 1 => {
            let now = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
            cx.schedule_timer(app.recipient.unwrap(), now, 3).unwrap();
        }
        Action::CancelOther if **event == 1 => assert!(matches!(
            cx.cancel_timer(app.other.unwrap()),
            Ok(SATimerCancel::Removed(2))
        )),
        Action::Advance(nested) if **event == 1 => {
            cx.core.clock.manual = Some(Duration::from_millis(200));
            let now = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
            cx.schedule_timer(app.recipient.unwrap(), now, 3).unwrap();
            if nested {
                cx.service_timers(app, 8).unwrap();
            }
        }
        Action::BudgetChain => {
            let now = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
            cx.schedule_timer(app.recipient.unwrap(), now, **event + 1)
                .unwrap();
        }
        _ => (),
    }
    SAPropagation::Continue
}
fn limit_handler(
    app: &mut App,
    cx: &mut SAContext<'_, App>,
    _: &SAEvent<'_, String, u8>,
) -> SAPropagation {
    assert_eq!(cx.service_timers(app, 8), Err(SAError::NestingLimit));
    SAPropagation::Continue
}

#[test]
fn claims_cancellation_callback_additions_and_nested_samples_follow_selected_stock_contract() {
    for action in [
        Action::AddDue,
        Action::CancelOther,
        Action::Advance(false),
        Action::Advance(true),
    ] {
        let mut core = Core::<App>::new().unwrap();
        core.clock.manual = Some(Duration::from_millis(99));
        let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
        let recipient = cx.create_recipient().unwrap();
        cx.subscribe(
            recipient,
            SAEventFilter::Timer,
            SAPriority::default(),
            handler,
        )
        .unwrap();
        let due = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
        let first = cx.schedule_timer(recipient, due, 1).unwrap();
        cx.core.clock.manual = Some(Duration::from_millis(100));
        let other = cx
            .schedule_timer(
                recipient,
                cx.clock().raw.checked_add(Duration::ZERO).unwrap(),
                2,
            )
            .unwrap();
        // A is earlier, but B is also due in this same pump. A can still cancel
        // B because the implementation claims one record immediately before use.
        if !matches!(action, Action::CancelOther) {
            assert!(matches!(
                cx.cancel_timer(other),
                Ok(SATimerCancel::Removed(2))
            ));
        }
        let mut app = App {
            action,
            recipient: Some(recipient),
            other: Some(other),
            trace: Vec::new(),
        };
        let report = cx.service_timers(&mut app, 8).unwrap();
        assert_eq!(report.sample.elapsed(), Duration::from_millis(100));
        assert!(matches!(cx.cancel_timer(first), Ok(SATimerCancel::Stale)));
        match app.action {
            Action::AddDue => assert_eq!(
                app.trace,
                [
                    (1, Duration::from_millis(100)),
                    (3, Duration::from_millis(100))
                ]
            ),
            Action::CancelOther => assert_eq!(app.trace.len(), 1),
            Action::Advance(false) => {
                assert_eq!(app.trace.len(), 1);
                cx.service_timers(&mut app, 8).unwrap();
                assert_eq!(app.trace[1], (3, Duration::from_millis(200)));
            }
            Action::Advance(true) => assert_eq!(
                app.trace,
                [
                    (1, Duration::from_millis(100)),
                    (3, Duration::from_millis(200))
                ]
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn budget_retains_due_chain_and_dispatch_nesting_preflight_does_not_claim_it() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    core.nesting_limit = 1;
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Local,
        SAPriority::default(),
        limit_handler,
    )
    .unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Timer,
        SAPriority::default(),
        handler,
    )
    .unwrap();
    let now = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
    cx.schedule_timer(recipient, now, 1).unwrap();
    let mut app = App {
        action: Action::Limit,
        recipient: Some(recipient),
        other: None,
        trace: Vec::new(),
    };
    cx.dispatch_local(&mut app, &0).unwrap();
    assert!(app.trace.is_empty());
    app.action = Action::BudgetChain;
    let report = cx.service_timers(&mut app, 4).unwrap();
    assert_eq!(report.claimed, 4);
    assert!(report.due_remaining);
    assert_eq!(app.trace.len(), 4);
    assert_eq!(cx.service_timers(&mut app, 1).unwrap().claimed, 1);
}

#[test]
fn marker_reservation_failure_preserves_due_owned_timer_before_claim() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Timer,
        SAPriority::default(),
        handler,
    )
    .unwrap();
    let deadline = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
    cx.schedule_timer(recipient, deadline, 7).unwrap();
    let mut app = App {
        action: Action::Limit,
        recipient: Some(recipient),
        other: None,
        trace: Vec::new(),
    };
    cx.core.dispatch.fail_next_marker_reservation = true;
    assert_eq!(
        cx.service_timers(&mut app, 1),
        Err(SAError::AllocationFailed)
    );
    assert!(app.trace.is_empty());
    assert!(cx.admission().ordinary_open);
    assert_eq!(cx.service_timers(&mut app, 1).unwrap().claimed, 1);
    assert_eq!(app.trace.len(), 1);
}

#[test]
fn arbitrary_cancellation_preserves_heap_and_reused_tokens_and_foreign_deadlines_reject_ownership()
{
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Timer,
        SAPriority::default(),
        handler,
    )
    .unwrap();
    let mut tokens = Vec::new();
    for value in [7, 2, 8, 1, 6, 3, 5, 4] {
        tokens.push((
            value,
            cx.schedule_timer(
                recipient,
                cx.clock()
                    .raw
                    .checked_add(Duration::from_millis(u64::from(value)))
                    .unwrap(),
                value,
            )
            .unwrap(),
        ));
    }
    for (value, id) in &tokens {
        if value % 2 == 0 {
            assert!(
                matches!(cx.cancel_timer(*id), Ok(SATimerCancel::Removed(removed)) if removed == *value)
            );
        }
    }
    let old = tokens[1].1;
    let replacement = cx
        .schedule_timer(
            recipient,
            cx.clock()
                .raw
                .checked_add(Duration::from_millis(9))
                .unwrap(),
            9,
        )
        .unwrap();
    assert_ne!(old, replacement);
    assert!(matches!(cx.cancel_timer(old), Ok(SATimerCancel::Stale)));
    let foreign = Core::<App>::new()
        .unwrap()
        .clock
        .sample()
        .checked_add(Duration::ZERO)
        .unwrap();
    let (payload, error) = cx
        .schedule_timer(recipient, foreign, 42)
        .unwrap_err()
        .into_parts();
    assert_eq!(payload, 42);
    assert_eq!(error, SAError::ForeignHost);
    cx.core.clock.manual = Some(Duration::from_millis(20));
    let mut app = App {
        action: Action::Limit,
        recipient: Some(recipient),
        other: None,
        trace: Vec::new(),
    };
    cx.service_timers(&mut app, 16).unwrap();
    assert_eq!(
        app.trace
            .iter()
            .map(|(value, _)| *value)
            .collect::<Vec<_>>(),
        [1, 3, 5, 7, 9]
    );
}

#[test]
fn failed_heap_admission_preserves_original_payload_and_reusable_candidate_without_heap_changes() {
    let mut core = Core::<App>::new().unwrap();
    core.clock.manual = Some(Duration::ZERO);
    let mut cx = SAContext::new(&mut core, BackendOps::Unavailable, SAContextPhase::Event);
    let recipient = cx.create_recipient().unwrap();
    cx.subscribe(
        recipient,
        SAEventFilter::Timer,
        SAPriority::default(),
        handler,
    )
    .unwrap();
    let due = cx.clock().raw.checked_add(Duration::ZERO).unwrap();
    for _ in 0..3 {
        cx.core.timers.fail_next_heap_reservation = true;
        let (returned, error) = cx
            .schedule_timer(recipient, due, 42)
            .unwrap_err()
            .into_parts();
        assert_eq!((returned, error), (42, SAError::AllocationFailed));
        assert_eq!(cx.core.timers.counts(), (0, 0));
    }
    let old = cx.schedule_timer(recipient, due, 1).unwrap();
    assert_eq!(old.key.index, 0);
    assert_eq!(old.key.generation, 1);
    assert!(matches!(
        cx.cancel_timer(old),
        Ok(SATimerCancel::Removed(1))
    ));
    cx.core.timers.fail_next_heap_reservation = true;
    assert_eq!(
        cx.schedule_timer(recipient, due, 99).unwrap_err().input(),
        &99
    );
    let replacement = cx.schedule_timer(recipient, due, 2).unwrap();
    assert_eq!(replacement.key.index, old.key.index);
    assert_eq!(replacement.key.generation, old.key.generation + 1);
    assert!(matches!(cx.cancel_timer(old), Ok(SATimerCancel::Stale)));
    let mut app = App {
        action: Action::Limit,
        recipient: Some(recipient),
        other: None,
        trace: Vec::new(),
    };
    let report = cx.service_timers(&mut app, 8).unwrap();
    assert_eq!(report.claimed, 1);
    assert_eq!(app.trace, [(2, Duration::ZERO)]);
    assert_eq!(cx.core.timers.counts(), (0, 0));
}
