# Solapp Windows extensions v7

Based on the reviewed winit 0.30.13 package, upstream revision
`e9809ef54b18499bb4f2cac945719ecc2a61061b`. The upstream Apache-2.0 LICENSE
and package files are preserved. Modified upstream files carry a modification
notice; this file identifies the local extension revision.

The Windows extension trait `winit::platform::windows::ActiveEventLoopExtWindows`
adds `try_create_custom_cursor(CustomCursorSource) -> std::io::Result<CustomCursor>`
and `try_listen_device_events(DeviceEvents) -> std::io::Result<()>`. Both use the
existing Windows backend owners. The ordinary cursor method keeps its logged
failure placeholder, and ordinary device-event requests keep ignoring results.
Default startup registration remains an ordinary request; callers needing a
confirmed result must explicitly use the fallible registration operation.

`WindowEvent::MouseCaptureLost` observes Windows `WM_CAPTURECHANGED`, including
synchronous sends, when capture transfers away from the window. Same-window
retention emits no event. The existing event runner buffers reentrant delivery.
Capture loss does not mean keyboard focus or physical button state changed.
Intentional last-button release can produce capture loss before the corresponding
button release; consumers must reconcile those records without duplicate releases.

V2 additionally corrects Windows raw mouse classification: relative motion is
identified by the absence of `MOUSE_MOVE_ABSOLUTE`, rather than by testing the
zero-valued `MOUSE_MOVE_RELATIVE` flag. Absolute packets emit
`DeviceEvent::MouseMotionAbsolute { position: (i32, i32), virtual_desktop: bool }`
and never relative mouse or axis motion. Values retain native Windows normalized
coordinates, nominally `0..=65535`, without clamping or conversion to desktop or
window pixels. Button and wheel fields keep their existing independent handling.
The backend adds no camera, warp or first-sample baseline policy.

V3 keeps every selected cursor in the existing window state, but immediate
application occurs only on the event-loop thread while that window has capture,
or verified uncaptured physical client control. The latter requires existing
IN_WINDOW state, no known foreground-thread capture, the actual pointer's window,
and client rectangle containment. Noncontrolling requests wait for the existing
legitimate WM_SETCURSOR path; no second native cursor owner is introduced.

V4 retains at most one currently applied custom cursor Arc on its owning
event-loop thread. The existing setter and WM_SETCURSOR sites share one native
application function; retention changes only after native replacement. Replacing
a noncontrolling window's stored selection cannot destroy the still-applied old
resource. Event-loop exit replaces a matching retained custom with the safe
shared default cursor before releasing it, and leaves unrelated native cursors
unchanged. Named/null applications and hidden cursor state keep these lifetimes.

V5 balances the backend's ShowCursor contribution on its owning GUI thread,
with one controlling HWND. Unrelated window visibility/grab requests retain
their window-local flags without restoring or stealing another window's hide.
Native client entry and WM_SETCURSOR establish client authority; immediate
requests additionally use V3's physical-client/foreground-capture guard, so
cached IN_WINDOW alone cannot certify current control. Own capture controls
visibility outside the client rectangle. Client/nonclient leave, capture
transfer/loss, WM_DESTROY and loop exit restore only the retiring owner's
contribution. A late old-window leave or close cannot restore a new owner's
hide. Existing ClipCursor error rollback and exclusive-mode behavior stay
unchanged; no additional native selector is introduced.

This patch keeps upstream custom-cursor selection/retention, raw registration
flags/target, native procedure processing, and the exceptional exclusive-display
failure panic. It does not implement another native subsystem or change upstream
cursor creation/cleanup internals. Native error diagnostics retain the upstream
cursor backend's existing error capture behavior.

Focused failure tests live in `tests/windows_backend_extensions.rs`, loaded
privately only for Windows library tests. Integration qualification belongs to
Solapp's stage 3; this revision alone does not claim complete input parity or
production host qualification.

V5's pure ownership regressions live in `tests/windows_cursor_visibility.rs`.
The public Solapp `cursor_visibility_smoke` example checks native counter
changes through own hidden windows with synthetic client/capture messages.
It restores the initial counter and does not move the physical pointer, change
OS focus, send global input, or certify visible pixels/hardware cursor planes.

V6 releases the destination window-state lock before `SetCapture` in all four
mouse-button acquisition paths. Synchronous CaptureLost callbacks may request
destination cursor state, release or transfer capture, or retire that window.
After reentry, live destinations refresh visibility from freshly locked current
flags and the existing actual capture/client checks. Native procedure recursion
retains WindowData until the outer callback returns; `userdata_removed` prevents
refresh or delivery of the original button event after native retirement.
Capture counts, same-owner retention, last-button release, cursor selection,
relative-mode suppression and ClipCursor rollback remain with their existing
owners. Ordinary callback delivery is unchanged.

Last-button release still decrements bookkeeping before any native operation and
drops the state lock before `ReleaseCapture`. It now verifies actual capture is
still owned by the releasing HWND, so an old destination's eventual button-up
cannot release a different owner selected by a synchronous acquisition callback.
CaptureLost remains a received reconciliation fact, not a permanent input gate:
an original real button press received afterward is still delivered, and its
later explicit release clears held state exactly once.

Solapp's `tests/capture_transfer.rs` runs own hidden-window posted-message cases
as isolated main-thread children with an independent parent watchdog. It covers
all button acquisition paths, same-owner/multiple-button retention, callback
visibility/release/transfer/retirement/stop, later input and service progress, and
balanced retirement. These messages do not measure physical-input frequency.

V7 commits each button acquisition's capture count after `SetCapture` returns,
after checking native retirement, and only while the destination still owns
capture. A synchronous callback can release and reacquire the destination with
another button; its nested count now survives alongside the original press.
No window-state lock spans `SetCapture`, and callbacks that leave capture released
or transferred elsewhere do not resurrect the destination's count. The native
regression covers both release orders after reentrant reacquisition, verifying
capture and held input survive the first release and balance after the last.
