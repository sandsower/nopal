# ADR 0012: Reset Nopal to an enforced Pi distribution

- Status: Accepted
- Date: 2026-07-27
- Decision owners: Nopal maintainers
- Supersedes the Field and agent-management product decisions in ADR 0010.
- The capability transport mechanism below is superseded by the anonymous inherited-descriptor design in ADR 0015.

## Context

Nopal grew into an agent-management product with a Field, desktop application, session registry, coordination services, and product-specific integrations.
Those surfaces duplicated responsibilities already owned by Pi and made deterministic enforcement one concern among many.
The product needs a smaller assurance boundary that can be explained, tested, and maintained without a daemon or alternate interaction surface.

Beislið already owns prose-first workflow meaning and skill lifecycle guidance.
Pi already owns sessions, interaction, models, tools, and the agent loop.
Nopal Core already owns deterministic policy, gate selection, and Workflow Run Ledger semantics.

## Decision

Nopal v0.3 is an opinionated Pi distribution with a deterministic enforcement kernel.
Pi remains the host, session owner, and interaction surface.
Bare `nopal` validates the effective project contract, initializes enforcement, and replaces itself with Pi.
Nopal does not launch a Field, Cockpit, desktop application, session registry, or coordination daemon.

Beislið remains prose-first.
Nopal reads authority only from recognized typed `beislid:*` Markdown fences.
Ordinary prose has no enforcement authority.
Invalid recognized blocks fail closed.
Unrecognized Beislið-owned blocks produce diagnostics without becoming authority.

User policy, repository policy, and compiled workflow policy compose through the fixed restriction lattice `allow < ask < deny`.
Repository and workflow sources may tighten user policy but can never weaken it.
Gate declarations accumulate, and conflicting definitions fail closed.

Nopal Core compiles typed declarations, evaluates policy, selects gates, validates receipt freshness, and records evidence.
Nopal Core never executes gate commands.
The bundled Pi adapter intercepts protected tool calls, requires each shell tool call to contain one completely classifiable command, executes exactly the gate plan returned by Core, and allows the original call only after Core observes current passing receipts.
Compound, dynamically constructed, redirected, expanded, or otherwise unsupported shell execution fails closed before any segment runs.
The adapter has no disable switch and internal enforcement commands are not agent-callable actions.

A gate receipt is bound to repository and workspace content, the effective enforcement contract, and the exact gate definition.
This ADR originally stored each receipt capability in a protected mode-0600 run-directory file.
ADR 0015 supersedes that transport with anonymous inherited one-shot descriptors; no capability file is published.
The launcher verifies every executable extension against identities embedded in the Nopal executable, rejects ambient or injected extensions, and makes the enforcement adapter the only executable extension in the default bundle.
Adapter subprocesses invoke the resolved Nopal executable rather than a PATH lookup.
Gate recording rejects results when the contract, workspace, or gate definition differs from the execution plan.
A relevant workspace or contract change makes the receipt stale.
Decisions, gate attempts, and receipts are recorded in the Workflow Run Ledger.

The v0.3 distribution includes Pi, Beislið, the Nopal CLI and enforcement adapter, and curated resources.
Rondo, Memento, Herdr, and agent-management integrations are not part of the active product contract.
Optional integration profiles may be designed later without restoring those dependencies to the default distribution.

## Consequences

Nopal has one interaction path and one enforceable assurance boundary.
Actions outside Nopal-launched Pi sessions receive no Nopal enforcement claim.
A missing extension, invalid effective contract, unavailable ledger, or failed Core round trip stops launch or blocks the protected action rather than falling back to plain Pi.

The current agent-management implementation is removed from active `main` before v0.3 ships.
Git history and a pre-reset marker preserve the former product without compatibility runtime aliases.
The smaller product leaves Pi free to evolve its own interaction surfaces while Nopal concentrates on deterministic process enforcement and evidence.

## Rejected alternatives

### Keep Field as an optional default surface

Even an optional Field preserves two interaction models and keeps session-management concepts in the core product.
Pi should own interaction without a Nopal facade.

### Infer enforcement requirements from workflow prose

Model interpretation cannot provide deterministic authority or fail-closed validation.
Only recognized typed blocks cross the compiler boundary.

### Execute gates inside Nopal Core

Executing commands would mix deterministic decision semantics with host effects and prevent Core from serving several adapters consistently.
The Pi adapter is the execution boundary.

### Preserve v0.2 runtime aliases

Compatibility aliases would keep the superseded product model active and make the clean assurance boundary harder to inspect.
The v0.3 release is an intentional clean break.
