# Solapp

Solapp is the Windows x64 application host for the Sol libraries. The current
implementation provides owner-thread startup, native window creation, checked
identities, local dispatch, owned posts, raw timers, scoped input delivery, native
keyboard/mouse/text, independent input modes and prepared cursors.
Source-specific services and explicit frame pacing are supported. Stop closes
admission and retains application/native retirement obligations.

Build on Windows x64 with Rust 1.98.0, the MSVC toolchain and Windows SDK.
Keep `solworker` and `solcache` as sibling directories of `solapp`; optional
planning and development examples/tests use those path dependencies. Run
`cargo test --all-features` from the Solapp directory.

The design uses winit for native windows/input, with narrow Windows helpers.
Solworker owns execution, Solcache owns resource mechanisms, and the renderer
owns graphics resources. Applications compose these directly and retain policy
for UI, assets, settings and live representation changes.

The [foundation guide](foundation.md) describes the implemented API and its
limits. Run the `startup_smoke` example for invisible-window startup and
retirement; `--fail-startup` exercises application initialization failure after
native window acquisition. Each invocation is a separate process because a
second winit event loop in one process is not promised.

The [events guide](events.md) covers mutation-aware dispatch, post ownership,
input state layers and timer semantics. Run `events_smoke` for cross-thread
posting and local timers through the native event loop.

The [input guide](input.md) covers keyboard and mouse observations, text sessions,
input modes and prepared cursors. The [service guide](services.md) covers
continuations, clocks, pacing and direct SW/SC composition; run
`service_composition` for a native integration example. The [display guide](display.md)
covers retained renderer access, live display changes and per-window retirement.
The [platform guide](platform.md) covers topology, optional `solworker` planning
and synchronous/bounded asynchronous shell requests.
The [shutdown guide](shutdown.md) covers pending-obligation snapshots, replacement,
normal retirement and the explicit fatal path.
