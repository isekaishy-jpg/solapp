# Service progress, frames and direct composition

[Guide index](README.md)

Register a service with an explicit point mask, record budget and finite fallback
interval. `SAApplication::service` receives its exact identity and a fresh raw
sample. Match the identity in application state and call the owning library
directly. SA schedules opportunities; SW owns job execution and SC owns resource
publication and cleanup.

## Progress and continuations

The callback reports `Quiescent`, `Continue`, `WaitUntil` or `AwaitWake`.
`Quiescent` still receives the registration's maintenance recheck. `Continue`
retains actionable work for another bounded visit. `WaitUntil` explicitly says
no work is needed before its raw deadline. `AwaitWake` names that service's wake
source and a finite deadline; the registration fallback can require an earlier
recheck.

Obtain an owned `SAWake` for a worker/provider notification route. A notification
only signals; the owner callback rechecks authoritative state and follows that
library's arm/recheck protocol. Signals coalesce. SA consumes the pending signal
before entering the callback, so a notification racing the callback's final check
remains pending. A wake is a hint, never a completed task or publication result.

`service_pending` supports explicit bounded service without recursively pumping
winit or starting another frame. An active service cannot invoke itself again;
other eligible services can run. Budgets count callbacks or application-selected
records and do not preempt arbitrary user code.

A native message loop can receive deadline wakes while the application is
unborrowed. A loop entered inside an application callback cannot reenter that
callback through winit; notifications remain buffered until the callback returns.
Return a continuation whenever a wait needs fresh native input or owner progress.
Do not block an active SW owner waiting for work that requires that same owner.

## Solworker and Solcache

The application owns its SW runtime, owner handles, notification routes, phases
and cancellation/close policy. SA service points do not assign SW phase numbers.
Set the appropriate phase before pumping, restore the previous phase across an
explicit nested service, and respect SW's active-owner restrictions.

SC cleanup needs an independent maintenance service even when no worker completes
and no window redraws. Its cleanup context stays on its creating thread and may
borrow application-scoped backing. A completion notice can cross a transferable
post queue; borrowed backing itself remains with its owner.

The `service_composition` example demonstrates direct intermediate SW publication,
real worker completion and independently delayed SC cleanup after route closure.
Retain route context until `close` and actual quiescence; a closed notification
route does not mean all resource cleanup or rendering has finished.

## Pacing and clocks

Frame scheduling is disabled until explicitly configured. `RendererManaged`
leaves interval limiting to the renderer and services requested frames. The
Stock and Forever policies expose their selected rate rules separately. Supply
renderer-active state explicitly; SA does not substitute OS keyboard focus.

Finite cap requests do not bypass the current interval. A delayed frame produces
one fresh frame rather than a loop that catches up every missed interval. The
application invokes rendering and owns any additional presentation policy.

Raw monotonic time controls deadlines, service and pacing. Application time is a
separate continuous clock; explicit rescaling or pausing does not move raw timers.
Frame timestamps and deltas use that application clock. No game update step or
simulation policy is implied.

## Retirement

Retirement-eligible services continue while the application reports pending
domain work. Once it reports `Settled`, application service callbacks stop while
SA completes its own remaining native/helper/lease obligations. Internal progress
does not depend on the closed ordinary post queue.

All native wake sends share a close gate. Event-loop teardown closes the gate and
waits for admitted native sends before destroying its wake window, including
unwinding. Surviving signal/receipt handles retain no borrowed application state.
