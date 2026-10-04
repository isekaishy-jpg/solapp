use crate::backend::BackendOps;
use crate::host::Core;
use crate::*;

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
