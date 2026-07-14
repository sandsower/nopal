# ADR 0011: Keep Session model authority in Pi

- Status: Accepted
- Date: 2026-07-14
- Decision owners: Nopal maintainers

## Context

The native Field needs to let a user change the model used by the current Plot and Session.
Pi already owns the active model, the available model registry, and whether an agent turn is active.
The durable Session stream owns semantic conversation and activity history, but model availability and current selection can change independently of that history.
The desktop also needs useful local ordering so recently selected models are easy to reach after restart.

## Decision

The Pi extension exposes a unified `nopal.session/v4` endpoint.
It preserves exact v2 and v3 durable history frames and adds ephemeral model request, state, and error frames on the same ordered connection.
Pi remains the sole authority for current model, available choices, and idle or active state.
The desktop never infers those facts from its preference file or from durable events.

A switch identifies one exact provider and model-ID pair reported by Pi.
The extension rejects switching while an agent turn or another switch is active.
It calls Pi once for an idempotent request and acknowledges success only after Pi reports the requested model as current.
The idempotency cache retains the 128 most recent completed requests and never evicts an in-flight request.
Prompt delivery waits behind an earlier switch acknowledgement so a following turn cannot race the selected model.
Direct Pi model changes publish the same authoritative state shape.
Reconnect sends a fresh snapshot after durable replay completion.
Model snapshots deduplicate Pi identities and expose explicit completeness metadata when the 2048-entry or 1 MiB transport bound omits a suffix.

The native Field presents a searchable picker.
Filtering matches model name, provider, or ID without case sensitivity.
The current model is shown from Pi state and switching remains disabled while Pi is active, state is unavailable, or a switch awaits acknowledgement.

The desktop persists at most 32 recently confirmed model identities in `model-recents.json` under the scoped private native state directory.
This file affects ordering only.
It is versioned, size-bounded, written through a private crash-safe replacement, and preserves malformed or unsupported existing content.

## Consequences

Durable Session history remains stable and replayable without encoding mutable model registry state.
All clients on the Session connection observe model changes in the same order as replay and live events.
A connection loss can leave the actual Pi switch result unknown, so the desktop clears the pending UI intent and waits for the next authoritative snapshot.
Recent ordering survives desktop restart without becoming a second source of truth.
Older v2 and v3 clients fail closed on a v4 endpoint instead of silently ignoring model-control frames.

## Rejected alternatives

### Store the active model in desktop preferences

This creates a competing authority that can disagree with Pi after direct changes, registry changes, or reconnect.

### Persist model changes as durable Session events

Model availability and current selection are mutable runtime state rather than semantic conversation history.
Persisting them would complicate replay and would still require reconciliation with Pi.

### Use a separate model-control socket

A second transport would introduce another lifecycle, ordering, and identity boundary for the same exact Session.
The unified v4 connection provides one ordered authority surface.
