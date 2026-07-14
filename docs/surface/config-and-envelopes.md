# Nopal config and envelope surface

Status: **active**.
Formerly cataloged as contract **C1**, this is now Nopal's own versioned product surface because Nopal has no external consumer for it.
Only the closed safety lattices below stay frozen ABI, and vocabularies are open.
Owner: **Nopal** (`crates/nopal-core`, `crates/nopal-cli`).
Surface: `nopal.*/v1` envelopes and `.nopal/*.jsonc` source modules.

## Scope

This surface covers deterministic decisions that must not live in agent prompt prose:

- binary version and capability introspection (`nopal.info/v1`)
- project readiness and validation (`nopal.status/v1`, `nopal.validation/v1`)
- preflight/gate declarations and selection (`nopal.gates/v1`, `nopal.gates.select/v1`)
- action-policy verdicts and runtime placement (`nopal.policy/v1`, `nopal.policy.*.v1`)
- normalized process artifact export and drift checks (`nopal.process_artifact/v1`, `nopal.process_artifact.export/v1`, `nopal.process_artifact.check/v1`)
- conservative Beislið workflow import drafts (`nopal.beislid_import/v1`) for migration into `.nopal/*.jsonc`
- durable run-ledger interoperability (`nopal.run_ledger.*/v1`, `run-ledger-v1` on disk)
- workflow lifecycle/checkpoint declarations (`nopal.workflow/v1`), including the effective handoff/babysit config with defaults applied (`nopal.workflow.show/v1`)
- external integration provider declarations (`nopal.integrations/v1`)
- non-authoritative host guidance hints (`nopal.guidance/v1`)
- checkpoint pointer file reads, interoperable with Beislið's `.beislid/checkpoints/latest.json` (`nopal.run_ledger.pointer/v1`)

Nopal selects, decides, normalizes, and explains.
It does **not** execute gates, call agents, or contact the network.
`nopal ledger` is the one warm surface here: it writes local durable state and probes local git only.

## Schema / normative sources

- `.nopal/nopal.jsonc`: project manifest shape.
- `.nopal/gates.jsonc`: local example of `nopal.gates/v1`.
- `.nopal/policy.jsonc`: local example of `nopal.policy/v1`.
- `examples/nopal/.nopal/workflow.jsonc`: example `nopal.workflow/v1` lifecycle/checkpoint surface.
- `examples/nopal/.nopal/integrations.jsonc`: example `nopal.integrations/v1` provider surface.
- `examples/nopal/.nopal/guidance.jsonc`: example non-authoritative `nopal.guidance/v1` hints.
- `crates/nopal-core/src/status.rs`: project readiness (`nopal.status/v1`) and validation (`nopal.validation/v1`) report shapes.
- `crates/nopal-core/src/validate.rs`: module presence/state checks and diagnostic ordering behind both reports.
- `crates/nopal-core/src/gates.rs`: gate/preflight schema and selector semantics.
- `crates/nopal-core/src/policy.rs`: action policy schema and decision/placement semantics.
- `crates/nopal-core/src/workflow.rs`: workflow event/action validation.
- `crates/nopal-core/src/integrations.rs`: integration provider validation.
- `crates/nopal-core/src/guidance.rs`: guidance authority-boundary validation.
- `crates/nopal-core/src/process_artifact.rs`: normalized process artifact export, source hashes, redaction, and drift diagnostics.
- `crates/nopal-core/src/beislid_import.rs`: Beislið workflow fenced-block import, unsupported-field diagnostics, and draft module validation.
- `crates/nopal-core/src/run_ledger.rs`: pure `run-ledger-v1` value semantics.
- `crates/nopal-core/src/run_ledger_store.rs`: local durable store semantics.

## Versioning

Additive vocabulary tokens are data, not ABI: they may land on the same `/v1` at any time as long as existing consumers keep passing, and do not require a new version.
Only the closed lattices below are fixed safety vocabulary and require either a new version or a documented fix-forward migration window to change:

- policy decisions (`allow` < `ask` < `deny`)
- policy placements (`shared_user_runtime` < `dedicated_repo_runtime` < `dedicated_run_runtime` < `blocked`)
- run-ledger statuses
- the protected floors for destructive and secret-bearing handling

Stable diagnostic codes, selector semantics, defaulting rules, and the run-ledger on-disk layout stay part of this surface's compatibility promise.
Unknown safety vocabulary must degrade conservatively instead of silently widening access.

## Conformance home

[`conformance/surface/`](../../conformance/surface/)

Fixtures should cover:

- valid and invalid `.nopal/` trees
- selector edge cases and stage mismatch reporting
- policy most-restrictive decision and placement cases
- run-ledger Python/Rust interop fixtures
- process artifact export/check fixtures including stale hashes and redaction
- Beislið workflow migration fixtures proving import, validation, no-secret export, and drift rejection over Nopal/Rondo/Memento-shaped configs
- valid/invalid workflow, integrations, and guidance module fixtures
- conservative degradation when unknown vocabulary appears in a newer config
- explicit coverage for the closed lattices that remain ABI-sensitive
