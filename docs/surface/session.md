# Nopal structured Session surface

Status: durable replay, structured activity, and live Pi model-control contract.

The structured Session surface carries native Composer instructions to one explicitly identified Pi Session and returns typed events for the native timeline.
It is an NDJSON transport contract and is independent from terminal output.
Terminal capture must never be promoted into semantic Session history.

## Endpoint capability

Core defaults new durable Unix-socket endpoint descriptors to kind `nopal.session/v4`.
The endpoint descriptor keeps the existing `kind`, `transport`, `address`, and `state` fields.
An explicit `nopal.session/v2` endpoint remains readable and emits only v2 durable events.
A v2 client must not silently accept v3 event variants.
A v3 client accepts exact persisted v2 envelopes followed by new v3 envelopes on one continuous stream.
The v4 endpoint preserves that exact durable v2 and v3 stream and adds ephemeral model request, state, and error frames.
V4 model-control frames are never written to durable Session history.
The consumer exposes separate v2-only and v3 server-frame types so exhaustive v2 consumers cannot accidentally begin accepting v3 activity.
The v3 frame type has explicit exact-v2 and typed-v3 event variants plus the shared replay-complete and feed-error controls.
The v4 frame type adds typed model state and model error variants to those durable frames.
`nopal.session/v1` describes the earlier live-only walking skeleton and does not promise replay.
A client must not bind durable replay behavior to a v1 endpoint.

## Prompt command

Composer commands retain kind `nopal.session.command/v1` because durable replay does not change prompt submission semantics.

```json
{
  "kind": "nopal.session.command/v1",
  "command_id": "command-01",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "command": {
    "type": "prompt",
    "text": "Explain the failing test"
  }
}
```

`command_id` is the stable identity and idempotency key of the submitted instruction.
`plot_id` and `session_id` are mandatory routing identities rather than display labels.
The only v1 command variant is `prompt`, whose `text` is passed to the selected Pi Session.
Prompt `text` must contain at least one non-whitespace character.
The durable journal must commit the typed `user_message` before the command is delivered to Pi or broadcast to feed clients.
Submitting the same `command_id` with the same prompt is an idempotent no-op.
Submitting the same `command_id` with different content is a fatal conflict.

## Subscribe

A client begins cold replay or cursor resume with kind `nopal.session.subscribe/v1`.

```json
{
  "kind": "nopal.session.subscribe/v1",
  "request_id": "request-01",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "after_cursor": null,
  "page_limit": 256
}
```

`request_id` identifies this subscription attempt.
`after_cursor` is required and nullable.
`null` requests the complete active-branch history.
A non-null cursor requests events strictly after that verified cursor.
On a v3 or v4 endpoint, the cursor may identify either a verified v2 envelope or a verified v3 envelope in the same stream.
An omitted `page_limit` defaults to 256.
The allowed range is 1 through 1024 inclusive.
Paging is an internal producer bound for one subscription and does not change the replay snapshot.

The producer snapshots the active-branch head, starts buffering later live events, emits all replay events through that snapshot, emits one matching `replay_complete`, and only then drains buffered live events in order.
No event may cross the replay-complete boundary out of order.

## Model control

A v4 client requests fresh Pi model state or an exact model switch with kind `nopal.session.model.request/v1`.

```json
{
  "kind": "nopal.session.model.request/v1",
  "request_id": "model-switch-01",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "request": {
    "type": "switch",
    "model": {
      "provider": "openai",
      "id": "gpt-example"
    }
  }
}
```

The two request variants are `refresh` and `switch`.
A switch target is the exact pair of Pi-reported provider and model ID.
Display names are not routing identities.
The producer accepts a switch only while Pi is idle and no other switch is pending.
An exact duplicate `request_id` and payload is idempotent.
Reusing a `request_id` for different content returns a conflict without calling Pi.
The producer retains the 128 most recent completed model-request identities and never evicts an in-flight request.
Requests older than that bounded window are new requests and clients must not retry them with a stale identity.
Prompt records received after a switch request wait until its model acknowledgement has been sent, so the following turn cannot overtake the switch.

After replay completion, and whenever Pi model authority changes, the producer emits kind `nopal.session.model.state/v1`.

```json
{
  "kind": "nopal.session.model.state/v1",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "request_id": "model-switch-01",
  "state_epoch": "8abdf460-6221-42ca-a30f-b9f580780c0e",
  "revision": 4,
  "agent_state": "idle",
  "current": {
    "provider": "openai",
    "id": "gpt-example",
    "name": "GPT Example"
  },
  "available": [],
  "available_complete": true,
  "available_total": 0
}
```

Pi is the sole authority for `current`, `available`, and `agent_state`.
The `available` list contains a deterministic prefix of at most 2048 unique provider and model-ID pairs and always fits the shared 1 MiB frame limit.
`available_total` reports the number of unique valid models Pi provided before transport bounds were applied.
`available_complete` is true exactly when `available_total` equals the emitted list length.
`state_epoch` identifies one bridge authority lifetime and `revision` increases within it.
`request_id` is nullable for unsolicited state and matches the request only when the response acknowledges that request.
A desktop client confirms a switch only when a matching acknowledgement names the exact requested current model.
Reconnect replaces prior model state from the new authoritative snapshot rather than replaying model frames from history.

Rejected requests use kind `nopal.session.model.error/v1` with the exact request, Plot, and Session identities.
Stable error codes are `busy`, `unknown_model`, `conflict`, `unavailable`, and `internal`.
Busy and transient availability failures may be retried after Pi or transport state changes.
An error never changes the last confirmed current model.

## Durable event

Every replayed or live semantic event on a v2 endpoint has kind `nopal.session.event/v2`.
A v3 or v4 endpoint replays persisted v2 envelopes exactly and uses kind `nopal.session.event/v3` for new events.

```json
{
  "kind": "nopal.session.event/v2",
  "event_id": "event-02",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "stream_id": "stream-01",
  "sequence": 2,
  "previous_cursor": "cursor-01",
  "cursor": "cursor-02",
  "command_id": "command-01",
  "event": {
    "type": "assistant_message",
    "text": "The assertion is inverted."
  }
}
```

`event_id` is the stable identity of the semantic event.
`stream_id` is the stable identity of the durable journal for the exact Plot and Session.
`sequence` starts at 1 and increases by exactly one along the active branch.
`previous_cursor` is required and nullable.
It is `null` exactly when `sequence` is 1, and otherwise names the immediately preceding event cursor.
`cursor` is a stable opaque identity that binds the Plot, Session, stream, prior cursor, sequence, event ID, optional command ID, and canonical event payload.
The cursor material intentionally does not include the event-envelope kind.
Envelope version validation remains strict and separate from cursor validation.
This version-neutral cursor material allows a new v3 event to extend an exact persisted v2 head without recalculating or replacing the v2 cursor.
Consumers must compare cursors exactly and must not infer meaning from their representation.
`command_id` is optional because lifecycle events can exist outside a prompt round trip.

The durable event variants are:

- `session_ready`, with no required variant fields.
- `user_message`, with required `text`.
- `assistant_message`, with required `text`.
- `session_error`, with required `message`.

The earlier `nopal.session.event/v1` shape remains readable only as a legacy journal input for deterministic migration.
The host assigns migrated events stable stream, sequence, predecessor, and cursor facts without changing their existing event IDs.
Legacy v1 input migrates deterministically to its existing v2 representation before any v3 append.
No migration rewrites a persisted v2 envelope as v3.

## Structured activity event

New durable events on a v3 or v4 endpoint use `nopal.session.event/v3` and retain the same Plot, Session, stream, sequence, predecessor, cursor, event, and optional command identities.
The v3 payload enum includes the four existing Session event variants plus six typed activity variants.
The four existing variants keep their v2 payload semantics inside v3, including multiline message text up to the shared frame bound.
Activity-specific display bounds do not narrow `user_message`, `assistant_message`, or `session_error`.

- `command_started` requires `activity_id`, `tool_call_id`, a safe bounded `command` display, and `started_at`.
  It may include a safe `working_directory` label.
- `command_finished` requires the same activity identities, `duration_ms`, a tagged `exit`, an `outcome`, and optional bounded `output`.
- `command_failed` requires the same activity identities and a safe `message`.
  It may include `duration_ms`.
- `tool_started` requires the activity identities, a bounded `tool_name`, a presentation-safe `summary`, and `started_at`.
- `tool_finished` requires the same identities, `duration_ms`, an `outcome`, and a presentation-safe `summary`.
- `tool_failed` requires the same identities, a safe `message`, and outcome `failed`.
  It may include `duration_ms`.

`activity_id` is the stable identity of one command or tool lifecycle within the durable Session stream.
`tool_call_id` preserves Pi's stable tool-call identity when the activity originates from a Pi tool hook.
The optional envelope `command_id` preserves causality to one Composer instruction when it exists.
Correlation uses only exact identities and never command text, tool name, timestamps, display labels, or Terminal content.

A command exit is tagged as `code`, `signal`, or `unavailable`.
Command outcome is `succeeded`, `failed`, `cancelled`, or `unknown`.
Tool terminal outcome is `succeeded`, `cancelled`, `unknown`, or the dedicated failed event.
Unavailable exit, duration, output, or correlation facts remain explicit and are never inferred from Terminal bytes.

Command output declares channel `stdout`, `stderr`, or `combined`.
Output and tool summaries declare `truncated`, `original_bytes`, and `omitted_bytes`.
Those counts must agree with the retained UTF-8 bytes.
A non-truncated value has zero omitted bytes.
A truncated value has at least one omitted byte.

The producer redacts and bounds presentation data before cursor calculation and persistence.
Activity identities retain the shared 4096-byte identity bound.
Tool names are at most 256 UTF-8 bytes.
Command display strings and safe summaries are at most 8192 UTF-8 bytes.
Failure messages are at most 4096 UTF-8 bytes.
Command output previews are at most 32768 UTF-8 bytes.
Unknown tools persist an explicit unavailable summary rather than raw arguments or raw result JSON.
Tool activity rejects the reserved fields `input`, `arguments`, `result`, `raw_input`, and `raw_result` even when they would otherwise be additive fields.

## Replay completion

One successful replay ends with kind `nopal.session.replay_complete/v1`.

```json
{
  "kind": "nopal.session.replay_complete/v1",
  "request_id": "request-01",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "stream_id": "stream-01",
  "cursor": "cursor-03",
  "sequence": 3,
  "event_count": 3
}
```

The exact Plot, Session, stream, `cursor`, and `sequence` identify the replay snapshot head.
`event_count` is the number of replay events emitted for this request, not the lifetime stream length.
An empty journal reports `cursor: null`, `sequence: 0`, and `event_count: 0`.
A resume that is already at the head reports that non-null head cursor and sequence with `event_count: 0`.
The client may publish staged replay events and enable Composer only after the completion identity and cursor chain validate.

## Feed error

Operational feed failures use kind `nopal.session.feed_error/v1` and are never persisted as semantic `session_error` events.

```json
{
  "kind": "nopal.session.feed_error/v1",
  "request_id": "request-01",
  "plot_id": "plot-01",
  "session_id": "session-01",
  "code": "history_gap",
  "retryable": false,
  "message": "The requested cursor is not on the active Session branch."
}
```

`request_id`, `plot_id`, and `session_id` are required nullable fields.
Plot and Session identity must either both be present or both be null when the failure happened before context binding.
`message` must be non-empty and no larger than 4096 bytes.
Stable v1 codes are:

- `history_gap` for an unknown resume cursor.
- `history_corrupt` for malformed or internally inconsistent durable history.
- `foreign_session` for history carrying another Plot or Session identity.
- `branch_diverged` for a cursor on an abandoned active-branch suffix.
- `history_too_large` when the configured durable-history bound is exceeded.
- `cursor_conflict` when one cursor or event identity names conflicting content.
- `command_conflict` when one command identity is submitted with different prompt content.
- `replay_buffer_overflow` when bounded live buffering cannot preserve the replay boundary.
- `protocol_violation` for malformed or out-of-order feed frames.
- `unavailable` for a temporary endpoint or host failure.
- `internal` for another producer failure that cannot be represented more precisely.

`retryable: true` permits transport-level reconnect with the same verified cursor.
A contract, identity, corruption, gap, or conflict failure is terminal until external state changes and must preserve the last verified timeline prefix visibly.
`replay_buffer_overflow` is retryable because reconnecting from that verified prefix creates a fresh bounded replay without weakening continuity checks.

## Continuity, duplicates, and active branches

Consumers validate every frame against the selected Plot and Session before routing or rendering it.
A structurally valid event for another context is foreign data and must not appear in the selected timeline.
The next new event must have the same stream, sequence exactly one greater than the verified head, and `previous_cursor` exactly equal to the verified head cursor.
Those continuity rules apply across the exact v2-to-v3 version boundary, including on a v4 endpoint.
An exact duplicate with the same cursor, event identity, command identity, and payload is a no-op.
Reusing a cursor or event identity for different content is a fatal conflict.
An unknown cursor, skipped sequence, or predecessor mismatch is a visible gap and must never trigger implicit replay from zero.
Malformed or internally inconsistent persisted sequence, predecessor, or cursor facts are `history_corrupt`, not branch divergence.

Replay and resume operate only on Pi's active Session branch.
A cursor in the common active prefix remains resumable after branch navigation.
A cursor on an abandoned suffix fails visibly as `branch_diverged` or `history_gap`.
`branch_diverged` is reserved for a known cursor on an abandoned suffix, while an unknown cursor is `history_gap`.
The producer must not merge abandoned branch entries into the active history.

## Bounds and compatibility

All frames are one UTF-8 JSON object per LF-terminated line.
Consumers reject a line larger than 1 MiB before JSON parsing.
Command, request, event, Plot, Session, stream, and cursor identities must not be empty, whitespace-only, contain control characters, or exceed 4096 UTF-8 bytes.
Unknown additive fields are preserved at envelope and variant levels.
Removing or changing required fields, changing variant meaning, or changing a frame kind requires a new version.

A v3 or v4 endpoint may hydrate an exact persisted v2 prefix and a persisted v3 suffix repeatedly.
Every restart must preserve event IDs, envelope kinds, stream identity, sequence, predecessor, cursor, command identity, payload, and additive fields.
Restart must not duplicate v3 activity or convert a v2 cursor into a v3 cursor.

One client replay buffers at most 128 later live events or 8 MiB, whichever comes first.
Overflow closes the feed visibly rather than dropping events.
The Unix writer separately admits at most 128 data frames or 8 MiB per client.
A typed fatal control frame bypasses those data limits, cancels data that has not started writing, and shares one two-second flush deadline from the first writer failure through EOF.
One active durable history is bounded at 100,000 events or 256 MiB.
Exceeding the bound fails as `history_too_large` rather than truncating semantic history.
The host retains at most 100,000 recently observed cursors per Plot and Session for abandoned-branch diagnostics, refreshing active-branch cursors before evicting the oldest retained cursor.
An evicted abandoned cursor is intentionally indistinguishable from another unknown cursor and fails as `history_gap` rather than `branch_diverged`.

The native model picker keeps at most 32 recently confirmed provider and model-ID pairs in a separate versioned private preference.
That preference controls UI ordering only and never supplies current model or availability facts.
Malformed, oversized, unreadable, or future-version preference files are preserved rather than overwritten.

## Persistence boundary

Pi's current Session API does not flush custom entries before the first assistant entry exists.
Completed first turns are durable, but a host crash during the first in-flight turn cannot provide strict exactly-once recovery with the current API.
After any assistant entry exists, an accepted durable user event is persisted immediately.
If restart finds a persisted user event without a terminal assistant or error event, the host records an interruption error and does not redeliver the prompt.

## Terminal boundary

Terminal is a same-Session fallback surface labeled `Live Terminal - not part of Session history`.
Raw VT bytes, captured text, prompts, and plausible JSON printed in the terminal are presentation data only.
They never create durable events, fill replay gaps, or repair corrupt history.

## Conformance

Checked-in fixtures live in [`conformance/surface/session/`](../../conformance/surface/session/).
Run their consumer checks with:

```sh
cargo test -p nopal-feed-client session::tests
```
