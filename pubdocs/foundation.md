# Host foundation

Construct `SAHost<A>` on the process main thread and call `run(&mut app)` once.
The application may contain non-Send state and borrow external cleanup contexts.
Neither the host nor its temporary `SAContext` can move to another thread.

Implement `SAApplication::started` and `SAApplication::stopping`. Startup may
create windows with owned `SAWindowSpec` values. Rejection returns the complete
spec and a typed `SAError`; success returns a host-scoped `SAWindowTarget`.
This target identifies a particular native generation, not renderer readiness.
Queries reject foreign hosts and stale identities/generations.

A rejected creation can already have acquired a native window. The host retains
that acquisition's identity and destruction obligation until native destruction
is acknowledged, so its delayed notification cannot retire a later window.
Rejection still returns the original specification and typed error.

`request_stop` immediately closes ordinary admission and preserves the current
callback. An OS close request on a live host window requests whole-host stop.
For individual retirement, `request_close(target)` returns immediately and
closes that target's admission while other windows and the application continue.
Window creation is available during ordinary host operation. Replacement creates
a new target and closes the old one; `acquire_window(target)` retains the native
generation for external use. See the [display guide](display.md).

`stopping` also runs after startup returns an error or panics. Return `Pending`
while application work still needs the host or its windows; the host retries
at `SAHostConfig::stop_poll_interval` without requiring input or redraw. Return
`Settled` only when that access has ended. The host independently waits for native
input release, shell-helper final access and retained window leases, then releases
window owners and waits for actual native destruction before reporting `Closed`.
Rust owner drop alone does not count as native destruction.

A successful `SAExitReport` includes the host identity, first stop reason and
number of observed native window destructions. Startup errors are returned
after cleanup. A report relies on the application's truthful settlement of
external work; it does not independently certify GPU or provider completion.

Unexpected native destruction faults the host and invalidates the corresponding
target. A timeout does not authorize forced release. If the backend exits while
host-dependent obligations remain, or retirement itself panics, the process
fails closed rather than returning borrowed state still in use. Arbitrary
non-string startup panic payloads are retained until process exit so a panicking
payload destructor cannot bypass retirement.

For an executable example, see `examples/startup_smoke.rs`. It creates an
invisible native window, performs several retirement visits and checks normal
or failed-startup exit. It does not exercise rendering or input adaptation.
