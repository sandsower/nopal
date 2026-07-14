# Execution contract conformance (Rondo Core)

Status: active home for the approved single-manifest `rondo.core/v1` intake; formerly cataloged as C2.

This directory holds the Nopal-facing Rondo Core boundary contract and stable fixtures.
The actual implementation remains in Rondo, while the cross-repository runner lives at `scripts/verify-rondo-core-bridge`.
The Nopal schema is canonical for this consumer boundary.
The runner compares Rondo's proposed event schema copy byte-for-byte, validates the service descriptor and both checked-in replay fixtures, and validates live producer pages before accepting the bridge.

## Assets

- `schemas/rondo-core-service-v1.schema.json` - JSON Schema for the active single-manifest service contract descriptor.
- `fixtures/minimal-service-contract.json` - minimal descriptor fixture covering submission, status, event replay, Nopal placement values, and ownership boundaries.
- `schemas/rondo-core-run-events-v1.schema.json` - JSON Schema for the `run.events` response payload and the three `rondo.core/v1` event families (`rondo.service.status_changed`, `rondo.run.status_changed`, `rondo.run.evidence_recorded`).
The service schema describes the contract descriptor; this one describes the event payloads a consumer actually receives.
- `fixtures/run-events-archived-replay.json` - a full `run.events` response from a completed run replayed from the zero cursor; one event of every family plus `next_event_cursor`.
- `fixtures/run-events-resume.json` - the same run resumed from `event_cursor: "rondo.core/v1:2"`, proving a mid cursor returns exactly the tail and archived replay is deterministic.
- `fixtures/run-status.json` - a `run.status` response for the same run.
The event-feed fixtures were produced by the reference implementation (`mix rondo.run_events` over `Rondo.Core.EventFeed`) with canonicalized `service_id`, `repo_id`, and `run_id` so they are stable conformance inputs.

## Event-feed conformance checks

Beyond the runner shape below, the event surface check should:

1. Call `run.events` from the zero cursor on a finished run and validate the full response against `rondo-core-run-events-v1.schema.json`.
2. Assert every one of the three event families is present, the feed is non-empty, and `next_event_cursor` matches `^rondo\.core/v1:\d{1,20}$`.
3. Resume from `rondo.core/v1:2`, assert the exact tail comes back, and assert polling from the returned tail cursor is empty without relaunching the run.
4. Confirm no `uri` leaks an absolute workspace or ledger path and every pointer uses `rondo-run://`.

## Expected runner shape

The authoritative runner is executable as:

```sh
scripts/verify-rondo-core-bridge <path-to-rondo-checkout>
```

A conforming runner:

1. builds the production Nopal binary;
2. starts an ephemeral loopback Rondo HTTP service with an isolated workspace root and an injected no-op execution adapter;
3. loads the production Nopal extension entrypoint and submits a producer-valid approved export through `nopal_afk_start`;
4. observes terminal status and events through `nopal_afk_result`;
5. replays the same submission through the production CLI and proves it deduplicates to the same run;
6. validates direct live replay, resume, and tail responses against the canonical schema;
7. proves the no-op adapter receives the expected trackerless issue, recipient, options, and run-owned frozen source contract;
8. proves exactly one accepted Rondo ledger and only opaque Rondo-owned evidence pointers.

This runner exercises the production Nopal CLI and Pi extension plus Rondo Core intake, supervision, freezing, ledger, status, and event surfaces.
It deliberately does not invoke Rondo's production agent adapter or perform repository work.

## Placement cases

Fixtures and runners should cover the Nopal placement vocabulary exactly:

- `shared_user_runtime`
- `dedicated_repo_runtime`
- `dedicated_run_runtime`
- `blocked`

`blocked` is a negative case: Nopal must not start the service or submit work for that request.
Unknown placement vocabulary must be treated conservatively as blocked until the consumer explicitly supports it.
The live runner uses `dedicated_run_runtime`; focused Nopal policy tests cover all blocking placement cases before network contact.

## Non-goals

- Do not encode Rondo GenServer module names as the Nopal contract.
- Do not require Nopal to know Rondo workspace, ledger, adapter, or artifact internals.
- Do not make standalone Rondo depend on Nopal.
- Do not add service lifecycle, cancellation, whole-bundle scheduling, dependency graphs, or supersede semantics to this single-manifest slice.
