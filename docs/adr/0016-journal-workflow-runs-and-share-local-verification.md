# ADR 0016: Journal workflow runs and share local verification

- Status: Accepted
- Date: 2026-07-28

## Context

Nopal already produced generic Workflow Run Ledger projections and exact continuous-enforcement evidence.
Those paths were independently orchestrated across Core, the CLI, and the Pi adapter.
A logical transition could require several filesystem writes, so a process failure could leave `run.json`, JSONL events, transcripts, checkpoints, and immutable enforcement artifacts at different boundaries.
The existing resume query selected the latest matching run but did not identify the exact journal revision or state whether protected proof survived a restart.
Interactive enforcement also owned its gate loop in TypeScript, while public policy, gate, and ledger commands could only approximate the same boundary for local headless use.

The v0.3 boundary excludes a daemon, remote verification service, global session registry, and worktree coordinator.
Core must remain deterministic and effect-free.
Pi must remain the session owner and only interactive approval surface.
Approval, receipt, release, and outcome authentication must remain launch-scoped and exact-call scoped.

## Decision

### Revisioned local run journal

Every native run mutation publishes one immutable `run-ledger-transaction-v1` record under the run's `transactions/` directory.
The record binds a monotonic revision, previous transaction digest, structural command kind and target state, redacted request digest, caller operation identity, replayable command result, projection effects, and its own canonical digest.
The transaction file is the durability point.
`run.json`, `events.jsonl`, `transcript.md`, checkpoints, reports, gate envelopes, and enforcement artifacts remain compatible projections.

The store acquires one bounded cross-process run lock, validates the complete bounded transaction chain, validates the structural state transition, allocates gate attempts, publishes the transaction, and then materializes its projections.
It never holds the run lock while executing a gate or waiting for approval.
A later process repairs a missing or stale projection only when its observed bytes match a prior committed boundary.
Foreign bytes, symlinked state ancestors, multiply linked files, malformed records, unknown or invalid command transitions, chain gaps, digest drift, unsafe paths, and exceeded bounds fail closed.
Caller-supplied operation identities make uncertain post-commit retries return the original result without duplicating a semantic transition.

Legacy `run-ledger-v1` runs remain readable without migration.
Their first native mutation creates a revision-zero anchor over their bounded existing projections while excluding the run-private gate runtime, which is revalidated separately.
Legacy projection bytes remain compatible with the Beislið reference even though native runs add journal files.

The structural lifecycle is `new -> running`, `running -> interrupted|failed|completed`, and `interrupted -> running|failed|completed`.
Failed and completed runs are terminal.
Only `ledger continue` transitions interrupted back to running.
It increments `resume_epoch` and records that all protected facts and gates must be re-observed.

An exact resume query requires a run identity and returns its revision, transaction digest, resume epoch, redacted latest checkpoint and hint, expected next action, and `must_reverify` state.
Legacy latest-match resume remains available for compatibility but does not claim exact revision evidence.
Repository, flow, run, journal, payload, report, depth, count, output, and lock scans are bounded.

### Shared local verification transaction

The CLI owns one `VerificationTransaction` used by both the private Pi adapter protocol and public headless verification.
The transaction gathers fresh workspace evidence, asks Core for the exact plan, durably publishes the decision, executes Core-selected gates through the run-private byte-locked executor manifest, re-observes after each gate, records exact attempts and passing receipts, and replans within a fixed convergence bound.
It returns blocked, approval-required, verified, or released.

The trusted CLI adapter owns bounded Git output and file hashing, gate processes, sanitized environments, process-group timeout through descendant pipe closure, output bounds, capability-descriptor exclusion, and durable ledger effects.
The stable executor digest remains the Core plan identity, while a separate authenticated runtime-manifest digest binds the random scratch path and exact device and inode used for execution and cleanup.
Core continues to return only typed plans and evidence directives.
The Pi adapter retains tool classification, protected-call concurrency leases, Pi UI approval, release of the original tool call, and matching terminal outcome publication.

The public `nopal verify` command uses the evidence-only purpose for the fixed `supervised_auto`, `git.push`, `git_remote`, pre-PR boundary.
It derives workspace, target, changed-file, contract, executor, and distribution facts through trusted adapters.
It performs no push, launches no Pi process, contacts no remote service, and exposes no mode, class, gate-command, executor, capability, or approval override.
An `ask` result is interruption evidence, not a release.

The private Pi purpose may create one exact authenticated action release after current gates and any Pi UI approval.
The public evidence-only purpose never creates a release.
Both purposes use the same canonical Core plan and receipt codecs.
Identical injected intent, authority, workspace, executor, result, and capability inputs therefore produce identical canonical plan and receipt bytes.

## Consequences

Run evidence survives process exit and restart with an auditable commit boundary.
Concurrent writers serialize revisions and gate-attempt allocation without a daemon.
A crash after transaction publication can be repaired without duplicating events or receipts.
A crash before publication leaves no authoritative transition.

Headless local verification now proves the same policy and gate boundary as interactive enforcement without pretending to execute or approve the protected action.
Gate execution moves out of the TypeScript adapter into the trusted Rust CLI adapter, concentrating confinement and timeout behavior behind one seam.
The Pi adapter becomes smaller and keeps only Pi-specific responsibilities.

The journal adds storage and replay complexity.
That complexity remains bounded inside the effectful run store, while callers continue to consume the existing v1 projections and versioned reports.
Restart never reuses old launch-scoped approval, receipt, release, or capability authority.
Historical evidence remains auditable, but a resumed run must re-observe and reverify.

## Alternatives considered

### Keep independent projection writes

This preserved the smallest implementation but could not provide one durable transition boundary or deterministic repair.
It was rejected.

### Use a repository-scoped SQLite event store

SQLite would provide strong transactions but would add a native distribution dependency, migration surface, WAL redaction concerns, and release-platform closure for a bounded local evidence problem.
It was rejected.

### Give the ledger a durable signing key

A durable key could make old receipts reusable after restart.
That would weaken the launch-scoped authority boundary established by ADR 0015.
It was rejected.

### Keep gate execution in the Pi adapter and duplicate it for headless use

Two runners would drift in environment, timeout, confinement, and receipt semantics.
It was rejected in favor of one trusted CLI runner.

### Launch Pi for headless verification

This would make automation depend on a session host and interactive provider even though no protected action needs execution.
It was rejected.
