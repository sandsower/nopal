# ADR 0014: Detect first-run gates from confined evidence

## Status

Accepted.

## Context

A portable Nopal project needs useful validation gates before its first Pi session.
A universal hard-coded gate is too weak, while prose interpretation, executable discovery, and repository-wide scanning are not deterministic authority.
The supported ecosystem set includes language manifests, package managers, build systems, repository task runners, and mixed monorepos.
Detection must remain explainable, fail closed on ambiguity, and preserve Nopal Core's non-executing boundary.

Generated readiness also affects whether Pi may launch.
An older binary must not silently ignore that semantic.
Preview and publication must use one evidence snapshot, and later manifest drift must not leave stale generated gates authoritative.

## Decision

Nopal Core owns one deep `gate_scaffold` planner with a compiled, versioned template registry.
The planner reads only root evidence and workspace paths explicitly declared by root manifests.
It does not search `PATH`, execute tools, contact registries, or recursively discover undeclared ecosystems.

Every selected template has a stable identity such as `rust.cargo/v1` or `task.just/v1`.
The immutable plan contains readiness, selected templates, generated gates, evidence paths, decisions, and stable diagnostics.
Scaffold, launch, and `nopal doctor` consume that same plan.

Generated baselines use `nopal.gates/v2` with `nopal.gate-scaffold/v1` provenance.
Every generated gate uses the reserved `detected.*` namespace and must appear in the provenance's complete generated-gate list.
Version 1 gate documents remain accepted as explicit checked-in authority.
Older Nopal binaries reject version 2 instead of launching while ignoring readiness.

Precedence within an evidence scope is:

1. explicit checked-in Nopal or typed Beislið gates;
2. explicit repository tasks;
3. family-specific package scripts;
4. proven ecosystem defaults;
5. the baseline Git diff check.

Same-stage explicit gates suppress generated defaults only when selected explicit proof covers the complete change set.
An unmatched or partially matched selector cannot remove generated proof for uncovered files.
Unknown-project readiness requires executable repository-wide proof, so selector-scoped authority cannot unblock launch before changed-file evidence exists.
Repository-wide typed Beislið gates satisfy the same readiness rule whether the checked-in Nopal gate document is explicit version 1 or generated version 2.

Independent ecosystems and declared workspaces compose deterministically.
Conflicting package managers, build systems, or repository task runners stop generation and name all conflicting evidence.
Fixed exclusions and manifest-declared negated patterns prevent globs from turning fixtures, examples, vendors, generated output, caches, or dependency trees into accidental project authority.
Manifest exclusions apply only to workspace declarations from that same manifest, so one ecosystem cannot erase a workspace independently declared by another.
Pnpm workspace YAML is parsed structurally so quoted negations and inline comments retain their declared meaning.
Line-oriented build declarations must be syntactic calls outside comments, CMake bracket arguments, and multiline strings rather than arbitrary text matches.
Go and Maven workspace declarations inside their language comment forms are ignored.
CMake command names are matched case-insensitively as required by the language.
Composer script aliases are not proof of PHPUnit because the alias target can be an arbitrary passing command.
Symlinked or traversing workspace declarations fail closed.

An unknown repository still receives the complete six-file baseline.
Its gate provenance remains `needs_configuration`, so configured replanning refuses to start Pi until explicit gates exist.
A blocked or ambiguous plan is never published.

A generated version 2 document is compared with current detection evidence before every launch.
Evidence drift blocks launch unless explicit checked-in gates supersede generated defaults.
Authority reads open every component with no-follow semantics, and enforcement hashes the exact confined bytes it parsed rather than rereading ambient paths.

## Alternatives considered

### Public ecosystem adapter traits

One adapter trait per ecosystem would create many shallow modules and move ordering, conflicts, and precedence into callers.
Cross-ecosystem composition would become harder to test as one contract.

### Checked-in external template files

External templates would create another package and parser authority for builtin defaults.
They would weaken the binding between detector behavior and the executing Nopal distribution.

### Prose or tool-availability inference

Agent interpretation and `PATH` probing would make two machines derive different authority from the same repository.
They also cross Nopal Core's deterministic, non-executing boundary.

## Consequences

First-run gates are useful across the supported ecosystem matrix without executing project code during detection.
The checked-in document records why each gate exists and which versioned recipe produced it.
Unknown and ambiguous projects fail closed with actionable diagnostics.

The compiled registry is a public compatibility surface.
Changing a recipe requires a new template identity when its generated contract changes materially.
Adding a new template requires positive, negative, ambiguity, and composition fixture evidence.

Some ecosystems have no universally safe conventional validation command.
Nopal requires stronger explicit configuration for those cases rather than guessing from an installed executable.
