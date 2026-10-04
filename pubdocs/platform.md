# CPU topology and association opening

[Guide index](README.md)

## Topology and optional worker planning

`SACpuTopology::query` reports the active Windows physical cores, logical processor
group masks and native efficiency classes. It reports query or validation failure
instead of inventing a fallback count. This is a system snapshot, not a grant of
process affinity or a prediction of available CPU time.

With the optional `solworker` feature, pass explicit `SAWorkerRequest`
values to `SAWorkerPlan::independent_physical_caps`. The per-class ceiling is the
physical-core count minus the supplied reserved count, saturated at zero and
clamped to 2–64. Each nonzero class request is independently clamped to that
ceiling with a floor of two; zero remains disabled. Priority requests pass through
to SW. There are no implicit reserved-core or class-count defaults.

The aggregate SW budget is the sum of the resulting class counts. It may exceed
the physical-core count. Planning starts no threads, installs no affinity and
does not certify that SW startup will succeed. Applications can also construct
SW configurations directly and retain their own runtime and owner handles.

## Shell associations

Provide an owned `SAShellRequest` containing a URL or an absolute native file path.
SA passes the destination intact to the Windows `open` association, without
arguments or percent decoding. The application chooses allowed destinations and
schemes. Native file paths preserve non-Unicode Windows characters.

`open_shell` performs the call synchronously on the owner thread. An installed
association handler may block or enter native modal processing. Use
`request_shell` when the application needs the owner to remain responsive.

The asynchronous path starts a dedicated bounded helper lazily. It does not use
the SW CPU pool or borrow application state. Rejection returns the complete
request; success returns a transferable `SAShellReceipt` with a stable host-tagged
identity. Capacity includes executing requests until the native call and request
reclamation finish.

`receipt.cancel()` succeeds only before the helper claims the request. Success
prevents launch; the receipt becomes `Cancelled` after reclamation. Cancellation
cannot undo a claimed launch. A terminal `Complete` result reports native
association acceptance or failure, not external process startup or exit.

Stop closes new shell admission and lets accepted requests settle. Required
helper retirement remains independent of the closed ordinary post queue. The
host waits for final helper access and apartment cleanup before returning; a
timeout cannot authorize dropping active work. `shell_pending() == 0` alone is
not proof that the helper has completed its own teardown. Receipts remain usable
after host closure without retaining windows, request bytes or the application.

No desktop redirection integration is included without an identified consumer.
