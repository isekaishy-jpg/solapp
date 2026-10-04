# Native window access and display changes

Acquire `SAWindowAccess` through `SAContext::acquire_window(target)` while the
target is open. It retains the actual native window generation. The lease can
be cloned and transferred; handle extraction remains on the creating thread.
`native_ref()` returns a borrowed `SANativeWindowRef` that is neither `Send`
nor `Sync`. This view supplies HWND, optional HINSTANCE, and the
raw-window-handle window/display traits. Copying a raw integer does not retain
anything.

A renderer adapter keeps the lease until its final native-window use has
ended. For Vulkane, owner-thread extraction can supply `Surface::from_win32`;
the adapter must retain the SA lease and enforce its own swapchain/surface
lifetime and actual GPU/presentation completion. SA does not supply a graphics
device, surface, swapchain, rendering thread, or completion certificate.

`request_close(target)` closes admission for that target and begins retirement.
It returns without blocking. Retirement waits for retained access to be released, releases the owner root on the
owner thread, and observes actual native destruction. It has no timeout that
authorizes destroying a window still in use. A lease's `is_native_alive()` is
an observation, not permission to call native APIs from another thread.
Unexpected external destruction invalidates extraction and is a fault; holding
a lease cannot prevent unrelated code from illegally destroying its HWND.

Use `monitors(target)` for current monitor and driver-mode choices. Submit an
explicit `SADisplayRequest::Windowed`, `Borderless`, or `Exclusive` through
`request_display`. Returned `SADisplayReceipt` means admission only. It identifies
both the transition and the presentation revision. SA revalidates the selected
monitor/mode before application; application code handles unavailable choices.

Progress arrives through `SAApplication::display_transition` and can be queried
through `display_transition`. The ordinary successful progression is
`Requested`, `Applying`, `AwaitingPresentation`, then `Ready`. The renderer or
application reports `SAPresentationStatus` against the exact receipt through
`report_presentation`. Stale acknowledgments are rejected. Only requests still
pending native application can be superseded; an active transition must settle
before a later one applies. A reported rendering failure remains a failure.

`display_observed` keeps backend mode and native geometry/DPI separate. Winit's
stored fullscreen state does not certify the hardware scanout mode. Physical
client dimensions may be zero while minimized. A display request also does not
prove renderer readiness or user acceptance.

Windowed requests accept an explicit placement or restore the saved placement.
Implicit restoration from a previously maximized window preserves the backend's
native normal-placement information. If a monitor disappeared, placement is
adjusted against current monitor snapshots. The backend can subsequently adjust
geometry.

Keep settings, timeout/revert policy, configuration persistence, UI replacement,
and Stock/HD resource rebuilding belong to the application and its domains.
Commit settings only after application confirmation. Ordinary display changes
preserve the native target; replacing a window uses create-new then close-old,
with distinct identities. Both operate within the same host and application-owned
SW runtime. The accepted exceptional exclusive-mode failure in winit can panic;
successful driver enumeration is not a guarantee that later application succeeds.

Qualification covers simulated transition/failure/reversal contracts, real
hidden-window lease retirement, and compilation of the actual Vulkane adapter
boundary. It does not establish real GPU final use, interactive mode switching,
monitor unplug behavior, or a particular driver's exclusive-mode reliability.
