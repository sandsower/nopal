# Execution contract: Rondo Core service API

Status: **active for approved single-manifest execution**.
Formerly cataloged as contract **C2**, this remains an inter-product contract because Rondo is a separate product from Nopal.
Owner: **Rondo Core** (BEAM).
Surface: `rondo.core/v1`.
Descriptor home: [`../conformance/execution/`](../conformance/execution/).
Schema seed: [`../conformance/execution/schemas/rondo-core-service-v1.schema.json`](../conformance/execution/schemas/rondo-core-service-v1.schema.json).

## Scope

This contract describes the service boundary Nopal manages without absorbing Rondo internals. Nopal coordinates over this contract; Rondo Core owns execution.

The implemented `rondo.core/v1` boundary covers:

- repository/Plot/run namespace model
- approved single-manifest submission
- run status and bounded event replay for active and archived runs
- opaque evidence pointers surfaced to the operator

Service lifecycle, cancellation, whole-bundle scheduling, dependency graphs, and supersede-chain enforcement are deferred operations.
Nopal's existing local lifecycle stub is not Rondo's durable execution record and is not part of this intake surface.

## Ownership boundary

Nopal may coordinate:

- evaluating or displaying the Nopal runtime placement decision
- submitting a run request and observing its status/events
- rendering status, event, and evidence pointers in the operator surface

Rondo Core remains owner of:

- execution supervision
- run ledger semantics and durable state
- workspaces and runtime process management
- agent adapters and model/provider execution
- artifacts, evidence production, and dashboard/archive internals

Standalone Rondo remains a first-class consumer of Rondo Core. The service boundary must not make Nopal the only supported coordinator.

## Runtime placement

This contract consumes the Nopal placement vocabulary exactly:

| Placement | Meaning at this boundary |
|---|---|
| `shared_user_runtime` | Rondo Core may use the caller's existing user runtime. |
| `dedicated_repo_runtime` | Rondo Core should isolate service/runtime state per repository namespace. |
| `dedicated_run_runtime` | Rondo Core should isolate runtime state for a single run. |
| `blocked` | Nopal must not start or submit work to Rondo Core for this request. |

Nopal decides placement and blocks submission before network contact when policy does not allow it.
Placement is not sent through the current Core HTTP request.
This contract does not redefine placement precedence.
Unknown placement vocabulary must degrade conservatively to `blocked` until the newer token is explicitly recognized by the consumer.

## HTTP transport and operations

The transport is loopback-only HTTP.
Rondo rejects non-loopback peers before request parsing.
Nopal calls this boundary through its reusable Core client and never invokes the `rondo` executable.

| Operation | HTTP boundary | Required request concepts | Required response concepts |
|---|---|---|---|
| `run.submit` | `POST /api/v1/execution-requests` | `manifest_path`, `manifest_sha256`, `repo_id`, optional `plot_id` | `surface`, `service_id`, `repo_id`, optional `plot_id`, `run_id`, `status`, `event_cursor`, `deduplicated` |
| `run.status` | `GET /api/v1/runs/{run_id}` | `repo_id` query and opaque `run_id` path value | `surface`, `repo_id`, optional `plot_id`, `run_id`, `status`, `last_event`, `evidence_pointers`, `event_cursor` |
| `run.events` | `GET /api/v1/runs/{run_id}/events` | `repo_id` and optional opaque `cursor` query values | `surface`, `repo_id`, optional `plot_id`, `run_id`, `events`, `next_event_cursor`, `has_more` |

A new submission returns HTTP 202.
An idempotent replay for the same repository id, exact manifest digest, and exact Plot id returns HTTP 200 with the existing run identity.
The same repository and manifest digest under another Plot creates an independent run.
Stable Core failures use structured codes without exposing manifest contents, local paths, or internal exception terms.

## Namespace model

- `repo_id`: stable opaque repository namespace chosen by the caller.
  Nopal uses an explicit configured value or derives `nopal.repo/v1:<sha256>` from the canonical checkout root without sending that path.
- `plot_id`: optional opaque caller namespace that Rondo validates, persists, and echoes without interpretation.
  Nopal requires an explicit established Plot for every managed submission and rejects missing or mismatched echoes.
  Standalone Rondo callers may omit this field and remain explicitly uncorrelated.
- `run_id`: opaque Rondo Core run id scoped under `repo_id`.
- `service_id`: opaque identifier for a live Rondo Core service/runtime instance.
- `event_cursor`: opaque cursor for streaming or replaying run events without relaunching completed work.

Nopal must not infer workspace paths, ledger locations, or artifact layout from these ids unless Rondo Core exposes them as evidence pointers.

## Event protocol seed

The initial conformance fixture names three event families:

- `rondo.service.status_changed`
- `rondo.run.status_changed`
- `rondo.run.evidence_recorded`

Every run-scoped event consumed by Nopal must retain Repository, Plot, run, and time context for deterministic display and replay.
Events may add fields, but existing required fields must remain compatible within `rondo.core/v1`.
Evidence pointers use only Rondo-owned `rondo-run://` URIs and never expose absolute workspace or ledger paths.

## Current evidence

Rondo now implements the loopback HTTP intake, durable pre-ack ledger, approved-export validation, idempotent admission, status, and paged events.
Nopal implements the typed Core client, policy-gated submit/observe commands, and Pi start/result tools.
The cross-repository runner exercises the registered Pi extension through the production Nopal binary and a real ephemeral Rondo HTTP server.

## Versioning

`rondo.core/v1` versions externally consumed service commands/endpoints and event payloads, not Rondo's internal GenServer module names.

Breaking changes include required field removal, enum tightening, namespace-layout changes visible to consumers, event-type semantic changes, or changes to the placement contract. Additive optional fields may remain in `/v1` only when existing fixtures and consumers keep passing.

## Conformance home

`conformance/execution/`

Current seed assets:

- schema: `schemas/rondo-core-service-v1.schema.json`
- fixture: `fixtures/minimal-service-contract.json`

Run the authoritative bridge proof from a Nopal checkout with:

```sh
scripts/verify-rondo-core-bridge <path-to-rondo-checkout>
```

The runner starts an isolated ephemeral Rondo HTTP service, submits one producer-valid approved export from an established Plot through the registered Pi tools and production Nopal binary, observes terminal status and events, replays the submission, and proves one Plot-correlated durable accepted ledger with only opaque evidence pointers.
