# Nopal process and proof-artifact surface

Status: **active with a known consumer gap**.
Beislið is a sibling surface over the same Nopal Core engine, so this is Nopal's surface even though the normative schemas still live in the Beislið repository.
Surface: `beislid-process-artifact-v1`, `approved-slice-plan-export-v0`, `execution-envelope-v0`, `proof-requirement-v1`, and `command_proofs`.

## Scope

This surface carries process/proof semantics between the beislid skill layer and executors such as Rondo:

- approved slice-plan exports
- execution envelopes and per-slice manifests
- process artifacts containing gates, action policy, guides, and proof contracts
- `proof-requirement-v1` attestations
- `command_proofs` executable proof commands for deterministic task-success checks

Beislid owns authoring and validation for this surface.
Consumers must not re-parse beislid prose or silently project incompatible shapes.
For Nopal repositories that opt into the config/envelope surface, `nopal.beislid_import/v1` and `nopal.process_artifact/v1` provide a deterministic adapter/export path; this must remain optional so standalone Beislið skill workflows stay portable without a Nopal Core install.

## Schema / normative sources

- `schemas/` in the Beislið repository
- `scripts/validate_export.py` in the beislid repository
- `docs/configuration.md` in the beislid repository
- `docs/testing.md` in the beislid repository

## Known gap

Rondo's `ExecutionRequest` intake currently rejects Beislið `approved-slice-plan-export-v0` slice manifests (`schema: "approved-slice-v1"`) because its local projection expects `boundaries` and `proof_requirements` as string lists while the producer schema emits structured objects.
All nine exported, validated, approved slices failed to load in the initial integration attempt.

This is conformance debt on the Rondo side: Rondo should consume the schema-defined shape or an explicitly versioned adapter fixture, not a hand-maintained sibling shape.
When a nopal repo provides a process artifact, Rondo may use it for cold config/drift checks, but execution envelopes and approved-slice manifests remain the source of task execution truth.

## Versioning

This surface's version includes the JSON Schema, validator behavior, required fields, enum values, and the meaning of proof verdicts.
Changes to `proof_requirements` or `command_proofs` semantics require either additive compatibility or a new version.

## Conformance home

[`conformance/surface/`](../../conformance/surface/)

Fixtures should cover:

- one minimal valid process artifact
- one valid approved-slice export with structured `boundaries` and `proof_requirements`
- one valid envelope using `command_proofs`
- negative fixtures for stale hashes, unknown enum values, and consumer-shape drift
- migration fixtures showing Beislið workflow configs can be imported into Nopal drafts and exported as process artifacts without making Beislið depend on Nopal
