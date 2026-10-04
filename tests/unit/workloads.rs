//! Explicit release-mode mechanism measurements; excluded from ordinary tests.

use crate::backend::{BackendOps, test::TestBackend};
use crate::host::Core;
use crate::*;
use std::time::Instant;

struct App {
    latencies: Vec<u64>,
    previous_sequence: u64,
    wheel_sum: f64,
}
impl SAApplication for App {
    type Message = ();
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        Ok(())
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        SAStopProgress::Settled
    }
}
fn route(app: &mut App, _: &mut SAContext<'_, App>, event: &SAEvent<'_, (), ()>) -> SAPropagation {
    if let SAEvent::Input(record) = event {
        let ordinal = app.latencies.len();
        if ordinal.is_multiple_of(2) {
            assert_eq!(
                record.event,
                SAInputEvent::PointerMoved(SAPhysicalPosition {
                    x: ordinal as f64,
                    y: 51.0,
                })
            );
        } else {
            assert_eq!(
                record.event,
                SAInputEvent::Wheel {
                    delta: SAScrollDelta::Lines {
                        x: 0.0,
                        y: if ordinal % 4 == 1 { 0.25 } else { -0.25 }
                    },
                    position: Some(SAPhysicalPosition {
                        x: (ordinal - 1) as f64,
                        y: 51.0
                    }),
                    scale: 1.5,
                }
            );
        }
        assert!(record.stamp.sequence > app.previous_sequence);
        app.previous_sequence = record.stamp.sequence;
        app.latencies.push(
            (record.stamp.delivery.unwrap().elapsed() - record.stamp.receipt.elapsed()).as_nanos()
                as u64,
        );
        if let SAInputEvent::Wheel {
            delta: SAScrollDelta::Lines { y, .. },
            ..
        } = record.event
        {
            app.wheel_sum += y;
        }
    }
    SAPropagation::Continue
}

#[test]
#[ignore = "explicit release-mode measurement; no timing thresholds"]
fn input_workload_measurement() {
    const COUNT: usize = 20_000;
    for deferred in [false, true] {
        let mut core = Core::<App>::new().unwrap();
        let mut backend = TestBackend::default();
        let mut app = App {
            latencies: Vec::with_capacity(COUNT),
            previous_sequence: 0,
            wheel_sum: 0.0,
        };
        let target = {
            let mut cx = SAContext::new(
                &mut core,
                BackendOps::Test(&mut backend),
                SAContextPhase::Startup,
            );
            let target = cx.create_window(SAWindowSpec::default()).unwrap();
            let recipient = cx.create_recipient().unwrap();
            cx.subscribe(
                recipient,
                SAEventFilter::Input,
                SAPriority::default(),
                route,
            )
            .unwrap();
            target
        };
        core.start(&mut app, BackendOps::Test(&mut backend));
        let start = Instant::now();
        let mut peak_queue = 0;
        let mut drains = 0;
        {
            let mut cx = SAContext::new(
                &mut core,
                BackendOps::Test(&mut backend),
                SAContextPhase::Event,
            );
            let mut receive = |app: &mut App, cx: &mut SAContext<'_, App>| {
                for index in 0..COUNT {
                    let event = if index % 2 == 0 {
                        SAInputEvent::PointerMoved(SAPhysicalPosition {
                            x: index as f64,
                            y: 51.0,
                        })
                    } else {
                        SAInputEvent::Wheel {
                            delta: SAScrollDelta::Lines {
                                x: 0.0,
                                y: if index % 4 == 1 { 0.25 } else { -0.25 },
                            },
                            position: Some(SAPhysicalPosition {
                                x: (index - 1) as f64,
                                y: 51.0,
                            }),
                            scale: 1.5,
                        }
                    };
                    cx.receive_input(app, target, event, SAInputOrigin::NativeWindow, None)
                        .unwrap();
                    peak_queue = peak_queue.max(cx.core.input.queue.len());
                }
            };
            if deferred {
                cx.with_input_deferred(&mut app, |app, cx| receive(app, cx));
                assert!(app.latencies.is_empty());
                assert_eq!(peak_queue, COUNT);
                while !cx.core.input.queue.is_empty() {
                    cx.drain_input(&mut app, 128).unwrap();
                    drains += 1;
                }
            } else {
                receive(&mut app, &mut cx);
            }
        }
        let elapsed_ns = start.elapsed().as_nanos();
        assert_eq!(app.latencies.len(), COUNT);
        assert_eq!(app.wheel_sum, 0.0);
        app.latencies.sort_unstable();
        println!(
            "INPUT_MEASUREMENT {{\"deferred\":{deferred},\"records\":{COUNT},\"elapsed_ns\":{elapsed_ns},\"peak_queued\":{peak_queue},\"explicit_drains\":{drains},\"receipt_to_delivery_p50_ns\":{},\"receipt_to_delivery_p95_ns\":{},\"receipt_to_delivery_max_ns\":{}}}",
            app.latencies[(COUNT - 1) / 2],
            app.latencies[(COUNT - 1) * 95 / 100],
            app.latencies[COUNT - 1]
        );
        core.request_stop(SAStopReason::Application);
        core.poll_stop(&mut app, BackendOps::Test(&mut backend));
        while let Some(key) = backend.complete_destruction() {
            core.native_destroyed(key);
        }
        assert_eq!(core.state, SAHostState::Closed);
    }
}
