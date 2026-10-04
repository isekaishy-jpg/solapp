//! The main-thread winit bridge; no public winit event types escape.

use ::winit::application::ApplicationHandler;
use ::winit::event::{DeviceEvent, DeviceId, WindowEvent};
use ::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use ::winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use ::winit::window::WindowId;
use std::time::{Duration, Instant};

use crate::backend::{BackendOps, NativeWindowKey};
use crate::error::{SAError, SANativeOperation};
use crate::host::{Core, SAApplication, SAHostConfig, SAHostState, SAStopReason};
use crate::{SAContext, SAContextPhase, SAInputState, SAInputStateLayer};

pub(crate) fn create_event_loop() -> Result<EventLoop<()>, SAError> {
    EventLoop::new().map_err(|error| SAError::Native {
        operation: SANativeOperation::CreateEventLoop,
        message: error.to_string(),
    })
}

pub(crate) fn run<A: SAApplication>(
    mut event_loop: EventLoop<()>,
    core: &mut Core<A>,
    app: &mut A,
    config: &SAHostConfig,
) -> Result<(), SAError> {
    // Retain the actual loop owner through helper join and wake closure.
    let _native_close = core.native_wake.close_guard();
    let deadline_wake = super::deadline::DeadlineWake::new(core.native_wake.clone())?;
    let mut bridge = Bridge {
        core,
        app,
        stop_poll_interval: config.stop_poll_interval,
        next_stop_poll: None,
        post_recheck_interval: config.post_recheck_interval,
        deadline_wake,
    };
    event_loop
        .run_app_on_demand(&mut bridge)
        .map_err(|error| SAError::Native {
            operation: SANativeOperation::RunEventLoop,
            message: error.to_string(),
        })
}

struct Bridge<'a, A: SAApplication> {
    core: &'a mut Core<A>,
    app: &'a mut A,
    stop_poll_interval: Duration,
    next_stop_poll: Option<Instant>,
    post_recheck_interval: Duration,
    deadline_wake: super::deadline::DeadlineWake,
}

impl<A: SAApplication> Bridge<'_, A> {
    fn intake(
        &mut self,
        event_loop: &ActiveEventLoop,
        batch: Result<Vec<super::winit_input::NormalizedInput>, SAError>,
    ) {
        let result = batch.and_then(|batch| {
            SAContext::new(
                self.core,
                BackendOps::Winit(event_loop),
                SAContextPhase::Event,
            )
            .receive_input_batch(self.app, batch)
        });
        if let Err(error) = result {
            self.core.fail(error, SAStopReason::BackendFailed);
        }
    }
    fn advance(&mut self, event_loop: &ActiveEventLoop) {
        self.core
            .pump_display(self.app, BackendOps::Winit(event_loop));
        self.core
            .reap_closing_windows(BackendOps::Winit(event_loop));
        self.core
            .pump_ordinary(self.app, BackendOps::Winit(event_loop));
        self.core
            .pump_retirement(self.app, BackendOps::Winit(event_loop));
        if self.core.state == SAHostState::Retiring {
            self.core.reap_windows();
        }
        let now = Instant::now();
        if self.core.state == SAHostState::Stopping
            && self.next_stop_poll.is_none_or(|deadline| now >= deadline)
        {
            self.core.poll_stop(self.app, BackendOps::Winit(event_loop));
            self.next_stop_poll = Some(now + self.stop_poll_interval);
        }
        self.core.prepare_redraw();
        let service_deadline = |point| {
            self.core
                .services
                .deadline(point, self.core.clock.sample())
                .and_then(|deadline| self.core.clock.native_deadline(deadline))
        };
        match self.core.state {
            SAHostState::Closed => {
                self.deadline_wake.arm(None);
                event_loop.exit();
            }
            SAHostState::Stopping => {
                // A finite deadline sustains cleanup without redraw or input.
                let mut deadline = service_deadline(crate::SAServicePoint::Retirement)
                    .map_or(self.next_stop_poll.unwrap_or(now), |service| {
                        service.min(self.next_stop_poll.unwrap_or(now))
                    });
                if !self.core.application_settled && !self.core.display_events.is_empty() {
                    deadline = now;
                }
                self.deadline_wake.arm(Some(deadline));
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            SAHostState::Running => {
                let fallback = now + self.post_recheck_interval;
                let timer = self
                    .core
                    .timers
                    .next_deadline()
                    .and_then(|deadline| self.core.clock.native_deadline(deadline));
                let deadline = if self.core.ordinary_pending() {
                    now
                } else {
                    [
                        timer,
                        service_deadline(crate::SAServicePoint::Maintenance),
                        self.core
                            .pacing
                            .deadline()
                            .and_then(|deadline| self.core.clock.native_deadline(deadline)),
                    ]
                    .into_iter()
                    .flatten()
                    .fold(fallback, Instant::min)
                };
                self.deadline_wake.arm(Some(deadline));
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            SAHostState::Retiring => {
                let deadline = now + self.stop_poll_interval;
                self.deadline_wake.arm(Some(deadline));
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            _ => {
                self.deadline_wake.arm(None);
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }
}

impl<A: SAApplication> ApplicationHandler<()> for Bridge<'_, A> {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        self.advance(event_loop);
    }
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.core.app_entered {
            let result = BackendOps::Winit(event_loop).raw_input(crate::SARawInputPolicy::Never);
            self.core.raw_input.failure = result.as_ref().err().cloned();
            if result.is_ok() {
                self.core.raw_input.confirmed = Some(crate::SARawInputPolicy::Never);
            }
        }
        self.core.start(self.app, BackendOps::Winit(event_loop));
        self.advance(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let key = NativeWindowKey::Winit(id);
        let Some((_, target)) = self
            .core
            .native_windows
            .iter()
            .find(|(current, _)| *current == key)
            .copied()
        else {
            return;
        };
        let live = self
            .core
            .windows
            .get(target.id)
            .is_ok_and(|record| record.generation == target.generation && !record.closing);
        if !live && !matches!(event, WindowEvent::Destroyed) {
            self.advance(event_loop);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.core.request_stop(SAStopReason::WindowCloseRequested);
            }
            WindowEvent::Destroyed => {
                self.core.native_destroyed(key);
            }
            WindowEvent::RedrawRequested => {
                if live {
                    self.core
                        .pump_ordinary(self.app, BackendOps::Winit(event_loop));
                    self.core
                        .frame(self.app, BackendOps::Winit(event_loop), target);
                }
            }
            _ if self.core.ordinary_open() => {
                let receipt_target = self
                    .core
                    .windows
                    .get(target.id)
                    .ok()
                    .map(|record| (target, record.native.geometry()));
                if let Ok(record) = self.core.windows.get_mut(target.id)
                    && let Ok(observed) = record.native.display_observed()
                {
                    record.display.observe(observed);
                }
                if let Some((target, (size, scale))) = receipt_target {
                    let empty = SAInputState::default();
                    let state = self
                        .core
                        .input
                        .state(target, SAInputStateLayer::Platform)
                        .unwrap_or(&empty);
                    let batch = self.core.native_input.window_event(
                        target,
                        &event,
                        state,
                        size,
                        scale,
                        &mut self.core.text,
                    );
                    self.intake(event_loop, batch);
                }
            }
            _ => (),
        }
        self.advance(event_loop);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device: DeviceId, event: DeviceEvent) {
        if !self.core.ordinary_open() {
            self.advance(event_loop);
            return;
        }
        let selected = self.core.relative_target.filter(|target| {
            self.core.windows.get(target.id).is_ok_and(|record| {
                record.generation == target.generation
                    && record.input_mode.confirmed.relative_motion
            }) && self
                .core
                .input
                .state(*target, SAInputStateLayer::Platform)
                .is_some_and(|state| state.focused)
                && self.core.raw_input.confirmed == Some(crate::SARawInputPolicy::WhenFocused)
        });
        let batch = self.core.native_input.device_event(
            selected,
            &event,
            crate::SAInputDeviceId {
                host: self.core.id,
                native: device,
            },
        );
        self.intake(event_loop, batch);
        self.advance(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.advance(event_loop);
    }
}
