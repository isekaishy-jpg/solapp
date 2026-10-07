# Events, posts and timers

[Guide index](README.md)

Solapp routes events on the host owner thread. Application state stays in the
application, including borrowed or non-Send state. A subscription stores a
function pointer, so explicit nested dispatch can reborrow the application and
context without recursively borrowing a closure. `Message` is the transferable
post type and must be `Send + 'static`; `LocalEvent` has neither requirement.

## Recipients and subscription mutation

Create a recipient with `cx.create_recipient()` and subscribe using a filter,
finite `SAPriority` and `SAHandler`. Priorities run from highest to lowest; a
new registration precedes an existing equal-priority registration. Positive and
negative zero compare equally. Non-finite priorities are rejected.

Each dispatch has its own position marker. Insertions after that marker can run
in the same dispatch. Inserting before it waits for another dispatch. Removing
a subscription immediately removes it from the ordering; an active invocation
remains retained until return. Consequently, self-removal followed by insertion
can admit a new higher-priority subscriber into the current dispatch when no
remaining earlier subscriber puts that insertion before the marker. Nested
dispatch starts at the head with an independent marker.

`retire_recipient` closes future admission and calls for that generation.
`recipient_state` reports active callbacks while they are retained; after full
reclamation the old token is stale. Retirement does not authorize destroying
application state still in use by an active callback. Removing an old token
cannot remove a new occupant of a reused slot.

Retirement traverses the occupied subscription order once. It adjusts each
active dispatch marker for each removed entry, so its work is O(S + R * D) for
S ordered subscriptions, R removals and D active dispatches. It does not scan
historical vacant subscription slots or allocate temporary storage.

`dispatch_local` borrows its event for the call. A handler may stop propagation
or explicitly dispatch another event. The configured nesting limit produces a
typed error. Handlers execute without a host mutex held; budget limits do not
preempt application code.

## Transferable posts

`host.proxy()` and `cx.proxy()` return an owned `SAProxy<Message>`. A producer
calls `try_post(recipient, message)`. Rejection returns the original message on
that producer's thread. Successful admission transfers ownership to the host.
Capacity includes queued, detached and active messages through their owner-thread
destruction. Receipt allocation occurs before admission.

An accepted post has a durable receipt: Pending, Delivered, Discarded or Faulted.
Delivered means recipient routing returned, including zero matching callbacks;
it does not mean a domain published its result. A later payload-destructor panic
faults the host rather than rewriting a terminal routing receipt. Receipts hold
no message or application reference.

An explicit post drain detaches a bounded batch. Posts arriving during routing
wait for another drain; a nested drain cannot steal the outer batch's tail.
Recipient retirement discards obsolete posts. Stop discards an unrouted tail,
and the owner still reclaims every accepted message. Receipt and proxy observers
may outlive the closed, empty host.

Empty and singleton drains need no detached batch backing. Larger drains can
reuse owner-local empty buffers; each active or nested drain owns its buffer
exclusively. The current private cache retains at most two empty buffers and
64 KiB of combined element backing, measured from actual capacity and message
size. Larger batches and deeper nesting remain supported through fallible
allocation. These limits cover idle backing, excluding active batches, receipts,
payload-owned storage and allocator overhead. Buffers return only after complete
settlement and disposal, and cached storage is released when the host closes.
An oversized drain preserves any smaller idle buffers for later reuse.
The cache uses observed overlapping batch sizes to budget growth and can replace
empty backing after settlement to make room for a reusable pair. This adjustment
is best effort: failure to allocate replacement backing cannot change completed
receipts or the drain result. Temporary replacement backing is additional to the
idle retention limit. More than two participating batches use uncached backing;
all outstanding batches keep their independent ownership through completion.

The native wake is a hint backed by finite intake rechecks. Close and enqueue
share one admission lock. A failed wake cannot change an accepted post into a
rejection or return its ownership to the producer.

## Input delivery

Normalized input records own text and preserve receipt-time position, scale,
source information and monotonically ordered sequence. The growing deferred
queue does not overwrite older input or automatically coalesce motion. Platform
state advances at receipt; Delivered state advances immediately before routing.
Queries require an explicit state layer and current window generation.

Use `with_input_deferred` during application work that must defer delivery. Its
scope restores the previous gate on return or unwinding. An explicit bounded
`drain_input` temporarily permits delivery, including inside that scope. It can
only consume input already available to Solapp; it does not recursively pump
native messages or start another frame. Leftovers preserve receipt order.

Native keyboard, mouse and text delivery use these mechanisms. The
[input guide](input.md) describes translation, text sessions and native cursors.

## Raw deadlines and timers

`cx.clock().raw` samples this host's monotonic clock. `checked_add(duration)`
constructs a checked raw deadline. Foreign-host deadlines are rejected. Raw
deadlines do not use application-time scaling.

`schedule_timer` accepts an owned local event for a recipient. A timer pump
samples raw time once, claims each due timer before routing, and checks the heap
again after callbacks. A callback-added timer already due at that sample can run
in the same pump. A nested explicit pump takes a new sample. Equal-deadline FIFO
ordering is not promised. The callback budget leaves later due work for another
pump.

`cancel_timer` returns the owned payload of an unclaimed timer. An already
claimed timer reports Claimed; cancellation cannot revoke its callback. A stale
token cannot cancel reused storage. Nesting-limit rejection leaves unclaimed
due timers and queued input intact. Stop reclaims pending timer payloads on the
owner, including local payloads borrowing an externally scoped context.

Pending and claimed timer counts take constant work, independent of historical
timer population. Counts do not replace the separate active-pump and payload
disposal obligations during shutdown.

Run `events_smoke` for a real native loop accepting a cross-thread post and a
timer before orderly stop. Deterministic tests cover mutation, close races,
retained input and failure handling. Native input acquisition and real rendering
are separate validation obligations.
