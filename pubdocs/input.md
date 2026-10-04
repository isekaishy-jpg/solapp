# Keyboard, mouse, text and cursors

[Guide index](README.md)

Subscribe to `SAEventFilter::Input` to receive owned normalized records through
the input gate described in the [events guide](events.md). Each record identifies
its native window generation and carries separate receipt and delivery stamps.
Deferred delivery preserves the original target, text and pointer-associated
coordinates. Windows occurrence time remains absent where the backend supplies
none.

## Keyboard and text

Physical scan codes support bindings independently of logical layout meaning.
Logical characters, named keys, dead keys, location, modifiers and repeat remain
separate observations. A logical character is not a committed-text notification.
Unknown native codes remain unidentified numeric values.

Call `begin_text(target, caret)` for the current text target. The caret rectangle
uses physical client pixels and is validated against the native rectangle range.
Update it with `update_text`; finish with `end_text`. Replacing or ending a session
invalidates its queued text. A key handler that ends a session cannot accidentally
deliver the following old commit to a replacement text target.

Ordinary text comes from the backend's actual committed text, including its
modifier effects. Synthetic focus keys never commit text. During IME composition,
only the composition commit path supplies text; preedit remains distinct. UTF-8
preedit selections must fall on character boundaries. The current profile targets
English/Latin entry while preserving Unicode and the Windows candidate UI fallback.
SA does not edit widgets, draw text or implement custom candidate/reading UI.

## Mouse observations and modes

Pointer positions are physical client pixels. Button and wheel events retain the
latest received position, or None before any position observation, and their
receipt-time scale. Wheel events preserve fractional values, both axes and their
line/pixel units. Consumers choose any game-specific accumulation or filtering.

Raw relative deltas and Windows normalized absolute-device coordinates use
different event variants. Absolute units are passed through unchanged, including
the virtual-desktop flag; they are not pixel coordinates or camera transforms.
Raw source identities are opaque observations, not permanent hardware identities.
Reconnect/reuse behavior follows the native backend. SA does not maintain camera
baselines or merge absolute sources into a fabricated relative stream.

`request_input_mode` selects relative delivery and pointer confinement separately.
Only one window receives raw movement at a time, and routing requires its
receipt-time platform focus. Normal window key/button/wheel events remain the
single source for those transitions; redundant raw copies are ignored.

Mode and raw-registration queries distinguish requested state from each last
successful native operation and its failure. A successful registration call does
not certify permanent registration ownership, hardware availability or receipt
of a particular packet. Partial failure does not invent a complete rollback.

Focus and capture loss reconcile only transitions still held in Platform state.
The adapter applies the complete received batch before callbacks, and Delivered
state then advances one record at a time. A physical release following capture
reconciliation is suppressed instead of becoming a duplicate game release.

## Prepared native cursors

Provide packed straight-alpha RGBA8 pixels, dimensions and an in-image hotspot
in `SAPreparedCursor`. `create_cursor` preserves that original input on rejection,
whether validation or native creation failed. A successful `SACursor` is a shared
native resource; `select_cursor` retains the selected clone for the window.
System cursor selections are also available.

Requested visibility is independent of selection and temporary input-mode
suppression. Focused relative mode suppresses native visibility; losing its
applicable focus, transferring the relative target or stopping restores the
stored visibility request. Cursor assignment alone does not enable relative
input or confinement. Native cursor visibility is not proof of a hardware plane
or of particular pixels being shown.

SA accepts already prepared images. Themes, animation, DPI variants, transforms,
software cursor drawing and game/UI hide reasons belong to their callers.
No controller, touch or pen API is part of this profile.
