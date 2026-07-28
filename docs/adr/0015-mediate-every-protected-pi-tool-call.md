# ADR 0015: Mediate every protected Pi tool call

- Status: Accepted
- Date: 2026-07-28

## Context

Launch-time policy checks do not control actions taken later in a Pi session.
A skill-level prerequisite is also insufficient because an agent can invoke a built-in tool directly or choose a different shell spelling.
Transient approval and unbound gate receipts can be replayed after the repository, command, policy, workflow, target, or placement changes.
Nopal must enforce its contract without taking ownership of Pi sessions or introducing a daemon, worktree allocator, or machine-wide security claim.

## Decision

Nopal installs one trusted `PiActionGuard` in every Nopal-launched Pi process.
The launcher resolves the canonical Pi entrypoint, verifies its exact package version and pinned `dist` tree integrity, hashes the executable, starts a bounded internal probe with that exact path, and requires an exact launch-token acknowledgement after the guard has registered its hooks.
The executable identity is checked again before handoff.
A missing, malformed, timed-out, substituted, or changed executable acknowledgement prevents the user-facing Pi process from starting.
The acknowledgement requires the complete audited built-in Pi tool catalog and rejects missing built-ins, unknown tools, or tool-catalog overrides.
Release launch also verifies a platform-specific digest over the complete Pi package and dependency closure, including manifests, native/WASM assets, symlink targets, and executable modes, and verifies an exact byte-locked Node runtime.
It clones the Pi closure and official Node executable into a private read-only content-addressed run snapshot, rehashes both, and invokes only the private copies.
The pinned Node build has no non-system dynamic-library closure.
The package closure, entrypoint, and Node bytes are revalidated after probing.
Debug test builds expose a deliberately named test-only Pi path seam, while release builds ignore it.

The guard mediates every supported built-in Pi tool call from `tool_call` until its matching `tool_result`.
It admits only a closed, audited tool and action vocabulary.
Filesystem inspection uses Pi's audited direct tools rather than ambient shell executables.
The shell read grammar admits only non-file identity commands and separately audited Git reads; other commands whose option or positional surfaces can mutate, execute helpers, or disclose ambient secrets fail closed.
Unknown tools, malformed input, unsupported shell syntax, path escapes, foreign roots, infrastructure failures, and unavailable placement block without an approval escape hatch.
Read-only protected calls may remain in flight concurrently under independent exact releases only when every required gate explicitly declares `parallel_safe: true`, `mutates: false`, and no autofix.
A mutating, approval-bearing, conservatively unspecified, or otherwise workspace-sensitive call is exclusive against every other protected call, so sibling effects cannot share pre-mutation evidence.
Credential mutation protection follows canonical nearest-existing-ancestor resolution, so direct paths and symlink aliases receive the same hard floor.

Core receives a versioned exact intent containing the launch, session, tool call, tool name, canonical input digest, target digest, changed-file selectors, mutation flag, workspace fingerprint, and run-private gate executor digest.
Core combines the built-in safety floor, user policy, repository tighten-only policy, and typed Beislið workflow policy through `allow < ask < deny`.
Force options, deletion refspecs, mirror, prune, and ambiguous destructive push forms share the non-approvable `git.push_force` safety-floor identity.
Core returns the winning policy and placement sources, selected prerequisite stages, exact gate definitions, and one authorization binding.

Every protected action requires the continuous stage.
Workspace writes also require `per_edit`, commits require `pre_commit`, and normal pushes and pull-request mutation require `pre_pr`.
Direct tool invocation therefore cannot bypass workflow prerequisites by avoiding a Beislið skill.

The trusted adapter executes only Core-selected gates.
Before Pi starts, the CLI resolves every top-level executor that any `continuous`, `per_edit`, `pre_commit`, or `pre_pr` definition may require, independent of current policy, selectors, and changed files.
It rejects repository and temporary shadows, creates a run-private alias manifest over canonical executable paths and bytes, and proves required executors are available.
Gate processes use that private alias directory plus the operating-system path, a canonical working directory beneath the project root, a private home and cache surface, non-profile Bash, bounded time, bounded captured output, process-group termination, and no inherited enforcement capability.
The manifest digest is authorization input and is revalidated before each private transition, so executor substitution invalidates proof rather than inheriting ambient `PATH`.
The Pi runtime itself receives an explicit environment allowlist, a system-only production base path, a protected private per-run home, and a private configuration directory containing only bounded no-follow authentication state.
Ambient user Git, curl, package-manager, Kubernetes, and related configuration is disabled or redirected into that empty home; system and repository Git configuration remains observed and bound by the workspace adapter.
External transfer authorization admits only the exact no-other-options shape `curl --disable <literal-http-url>` (or `-q`); redirects, config, proxies, URL globbing, multiple targets, alternate protocols, and unaudited transfer tools fail closed.
Executable, symlinked, or multiply-linked project settings fail launch, remain protected enforcement authority, and are revalidated before every private authorization transaction, so aliases or indirect workspace mutations cannot install a custom shell or command prefix for a later reload.
Executable Git and ripgrep configuration carriers are rejected or bound to exact trusted helper bytes.
Core returns authenticated evidence directives only when the exact contract, workspace, gate definition, and authorization binding remain current.
Each passing receipt is immutably keyed by both gate identity and exact authorization binding, preventing concurrent calls from replacing each other's proof.
The CLI adapter alone publishes those directives to durable run state.

An `ask` decision uses Pi's UI.
The response is recorded as authenticated durable evidence for the exact authorization binding.
Core reauthorizes the subject and consumes the approval before the guard releases the tool call, making approval single-use.
The release itself is atomically recorded once and returns an authenticated release identity.
The guard retains that identity until the matching result and records success, error, cancellation, or shutdown interruption before clearing the in-flight lease.
An unrecordable or mismatched result poisons later authorization until a successful shutdown interruption closes the release.

The launch-scoped adapter capability is held in an anonymous inherited descriptor that the trusted extension reads and closes before agent activity.
Each private adapter subprocess receives a fresh unlinked mode-0600 one-shot capability descriptor, while the matching proof travels through bounded stdin rather than command-line arguments.
The original descriptor is not retained by Pi, and the capability is never written to run state, inherited by a gate process, or exposed as a public bypass flag.

Nopal imports typed Beislið `agent_isolation` requirements and validates current placement evidence.
Nopal does not allocate worktrees or coordinate agents.
If the required placement cannot be proven by the active launch, protected activity fails closed.
The direct launcher also blocks every non-empty runtime profile because it has no trusted profile allocator or capability receipt.
Every present isolation field is type-checked exactly, so malformed recognized values cannot downgrade to default placement.

## Consequences

Authorization now follows the exact effect rather than a launch-time approximation.
Relevant source, command, selector, policy, workflow, target, worktree, distribution, placement, or run drift invalidates prior evidence.
The Core remains deterministic and effect-free while the CLI and trusted Pi adapter retain filesystem, Git, process, prompt, and ledger effects.

Supporting another Pi tool requires a reviewed classifier codec and matching Core action-class vocabulary.
This deliberate closed-world cost is preferable to silently trusting ambient custom tools.

The guarantee applies only to actions performed through a Nopal-launched Pi session.
Nopal makes no claim about another terminal, another Pi process, or machine-wide enforcement.

## Alternatives considered

### Check policy only at launch

This leaves every later tool call outside the authorization boundary and cannot satisfy continuous enforcement.

### Rely on Beislið skills to run gates

This makes prerequisites advisory because direct tool calls can avoid the skill path.

### Let Core execute gates and Git commands

This would mix deterministic decisions with ambient process effects and make the assurance boundary harder to test and embed.

### Expose a user bypass or approvable infrastructure failure

This would allow the exact missing-enforcement and unknown-context cases that must fail closed.

### Add a session or worktree coordinator

Pi and Beislið already own those lifecycle concerns.
Nopal only validates the placement contract needed for the current authorization.
