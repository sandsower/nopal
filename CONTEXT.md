# Nopal

Nopal is an opinionated Pi distribution with deterministic workflow, gate, and action-policy enforcement.
Pi owns interaction and sessions.
Beislið owns prose-first workflow meaning and skills.
Nopal Core compiles typed authority, decides what is permitted and required, and records evidence.
The bundled Pi adapter mediates protected tool calls and executes selected gates.

## Language

**Nopal**:
The distribution users launch with bare `nopal`.
It validates the effective contract, initializes enforcement, and starts Pi directly.
_Avoid_: adding a second interaction surface, process dashboard, or agent manager

**Pi**:
The host and interaction surface for sessions, models, prompts, tools, and agent loops.
Nopal extends Pi rather than wrapping it in another management UI.
_Avoid_: reimplementing Pi sessions or treating Nopal as the conversation owner

**Beislið**:
The prose-first workflow and skill layer.
Beislið explains lifecycle meaning and emits explicitly typed `beislid:*` blocks when deterministic enforcement needs structured authority.
_Avoid_: asking Beislið prose or an agent interpretation to make an authorization decision

**Nopal Core**:
The deterministic engine that compiles recognized typed blocks, combines policy, selects gates, validates receipt freshness, and writes Workflow Run Ledger evidence.
It never executes gate or protected-action commands.
_Avoid_: duplicating decision semantics in TypeScript, skills, or host prompts

**Pi enforcement adapter**:
The bundled extension that intercepts Pi tool calls, asks the Nopal CLI for a Core-authored enforcement plan, delegates selected gate execution to the confined CLI adapter, and blocks or releases the original call.
It remains active for the complete Nopal-launched Pi session and has no disable switch.
_Avoid_: policy caches that outlive their inputs, bypass toggles, or extension-local authority

**Project contract**:
The checked-in `.nopal/` configuration and distribution lock that identify a supported repository and its deterministic modules.
_Avoid_: private local setup as a substitute for the checked-in baseline

**Typed Beislið block**:
A fenced Markdown block whose key is recognized by the Nopal compiler, such as `beislid:gates` or `beislid:action_policy`.
Only the typed body can contribute enforcement authority.
Ordinary Markdown prose is ignored by the compiler.
_Avoid_: inferring requirements from headings, surrounding prose, or model interpretation

**Effective policy**:
The policy produced from user, repository, and compiled workflow sources through the restriction lattice `allow < ask < deny`.
A narrower source may tighten an earlier decision but can never weaken it.
_Avoid_: precedence rules where a repository allow overrides a user deny

**Protected action**:
A Pi tool call whose stable action identity or class requires deterministic mediation before execution.
The v0.3 distribution protects every supported Pi tool call and distinguishes normal push from force push.
_Avoid_: relying on a skill to remember to ask before invoking the tool

**Gate**:
A deterministic check declared by configuration and selected by Nopal Core for a workflow seam.
The Pi adapter executes the command and returns the observed result.
_Avoid_: executing gate commands inside Nopal Core or treating successful process startup as gate evidence

**Gate template**:
A builtin, versioned recipe that maps proven repository evidence to conservative generated gates.
Its stable identity, such as `rust.cargo/v1`, is recorded in the checked-in gate provenance.
_Avoid_: inferring a template from installed executables, undeclared nested projects, or prose

**Evidence scope**:
The repository root or a symlink-free confined workspace explicitly declared by a root manifest.
Only evidence inside these scopes may influence generated gates.
_Avoid_: broad recursive discovery through fixtures, examples, vendors, generated output, caches, or dependency trees

**Detection decision**:
A stable explanation that records whether a gate template was selected, skipped, superseded, ambiguous, or blocked, together with its evidence and reason code.
`nopal doctor` presents the same decisions that first-run scaffolding uses.
_Avoid_: a separate advisory detector whose conclusions can drift from generated authority

**Gate receipt**:
Durable, session-authenticated evidence that one exact gate definition passed against one effective contract and one workspace content fingerprint.
The receipt capability is ephemeral, delivered through anonymous one-shot channels, and never enters gate environments, subprocess arguments, project data, or ledger events.
A workspace, contract, or gate-definition change makes the receipt stale or rejects the in-flight result.
_Avoid_: session-wide booleans, command-string caches, or receipts detached from repository content

**Workflow Run Ledger**:
The bounded durable record of a Nopal-launched Pi session's action decisions, gate attempts, receipts, checkpoints, interruption, and outcome.
It is an evidence surface, not a dashboard or session-management product.
_Avoid_: rebuilding process management, coordination feeds, or a daemon around the ledger

**Enforcement coverage**:
The guarantees actually mediated by a Nopal-launched Pi session.
Nopal makes no enforcement claim for actions performed outside that boundary.
_Avoid_: retroactively describing external work as enforced

## Invariants

- Bare `nopal` launches Pi directly after enforcement initialization succeeds.
- Pi owns all user interaction and session state.
- Nopal never infers authority from prose.
- Invalid recognized typed blocks fail closed.
- Unrecognized Beislið-owned blocks remain diagnostic-only.
- Policy composition can only become more restrictive.
- Nopal Core selects and validates but never executes gates.
- First-run gate detection reads only root and explicitly declared workspace evidence.
- Unknown or ambiguous gate detection cannot start Pi.
- The Pi adapter accepts only one completely classifiable command per shell tool call and executes only the gate plan returned by Core.
- Compound, dynamic, redirected, expanded, or otherwise unsupported shell execution fails closed before any segment runs.
- A protected action cannot run with missing, failed, stale, or unauthenticated required gate evidence.
- Decisions, attempts, and receipts are durably recorded.
- Enforcement has no session bypass or daemon dependency.
- Removed management and coordination surfaces have no active runtime, alias, archive member, or configuration authority in v0.3.

## Example dialogue

> **Developer:** "Can the workflow prose say that force push is allowed?"
>
> **Domain expert:** "No. Prose can guide the agent, but only recognized typed policy blocks enter the effective policy. A repository or workflow source may tighten the user's policy, never weaken it."

## Resolved product decisions

- Nopal v0.3 is a clean break from the v0.2 agent-management product.
- `v0.2.16` is the final agent-management release.
- The default distribution consists of Pi, Beislið, the Nopal CLI and enforcement adapter, and curated resources.
- Bare launch is offline and deterministic.
- Network synchronization and updates are explicit commands rather than launch side effects.
- The active source tree and release archives contain no superseded management or integration implementation.
- Git history and a pre-reset marker preserve the former implementation without runtime compatibility aliases.
- A plain Pi session started outside `nopal` carries no Nopal enforcement guarantee.
- Release archives contain exact Pi and Node runtime bytes, the policy adapter, and pinned Beislið skills.
