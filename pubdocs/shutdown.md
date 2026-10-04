# Shutdown and replacement

`request_stop` latches the first stop reason and closes ordinary admission.
Accepted ownership still has to settle. Application `stopping` callbacks return
`Pending` while domain cleanup needs the host, then `Settled` once their external
obligations no longer need callbacks, windows or borrowed application state.
Solapp latches Settled and stops calling application service/retirement callbacks.
It independently waits for native input release, shell-helper final access,
window leases and actual native destruction.

`shutdown_snapshot()` on the context or host reports separate observations:
application settlement, active callbacks, accepted posts, queued/claimed timers,
deferred input, service registrations, window roots/leases, destruction
acknowledgments, native input cleanup and shell requests/helper retirement.
`fault()` on the context exposes the retained first typed failure.

These are point-in-time observations. A foreign lease or helper can progress
immediately afterward. Zero shell requests does not establish apartment/thread
retirement; dropping a window root does not establish native Destroyed; an
application's Settled report does not erase retained leases. SA does not infer
provider, cache, worker or GPU completion from its counters.

An application can display these reasons after its own timeout. Elapsed time
never authorizes destruction. Keep necessary continuations running while
`Pending`; do not block the owner waiting for work that requires owner service.
The direct SW/SC example shows route quiescence and independent cache cleanup.
Real renderer completion remains the renderer adapter's obligation.

For live UI/resource replacement, retire old recipient/text generations and let
their owners reject stale publication. Keep the host and application-owned SW
runtime. Display changes keep the window target; a new native window gets a new
target and the old one follows per-target retirement. SA supplies these mechanisms
without choosing Stock/modern UI or Stock/HD assets.

A recoverable startup failure still enters application cleanup for partial
acquisitions. A retirement panic or backend loss with outstanding final access
cannot safely return borrowed state: the explicit fatal path terminates the
process. Do not rely on destructors running after that path. The retirement-panic
contract is exercised in an isolated subprocess; it is separate from normal
pending shutdown and typed recoverable failures.
