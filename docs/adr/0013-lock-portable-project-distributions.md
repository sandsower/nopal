# ADR 0013: Lock portable project distributions

- Status: Accepted
- Date: 2026-07-27

## Context

Nopal launches Pi with repository policy and resources that can affect every subsequent tool call.
A path-only bundle cannot prove that two contributors loaded the same package bytes, and ambient Pi settings can silently widen the active resource surface.
Package resolution during bare launch would also make startup depend on the network and allow registry changes to alter behavior without a reviewed repository change.

Nopal Core must remain deterministic and effect-free.
It may parse, normalize, hash, compare, and explain package evidence, but it must not execute package managers or contact registries.

## Decision

A portable project checks in a `nopal.bundle/v2` contract and a `nopal.lock/v1` lock.
The contract names packages by stable `builtin`, `workspace`, or `npm` source identity and declares exported Pi resources.
The initial v0.3 contract accepts only exact semantic-version requirements, optionally prefixed by `=`, so selecting another version requires an explicit reviewed contract edit.
The lock records the normalized contract digest, exact resolved package version, artifact integrity, installed-tree integrity, and integrity of every exported resource.

Bare `nopal` only inspects local evidence.
It never resolves, installs, updates, or contacts a registry, and it always starts Pi in offline mode.
Missing, changed, duplicated, unsafe, or incomplete lock evidence prevents launch.

`nopal update` is the only command that resolves a contract into a lock proposal.
Without `--write` it leaves the checked-in lock untouched.
With `--write` it atomically replaces the lock after every package tree and resource has been validated.

`nopal sync` consumes the checked-in lock exactly and never changes it.
Builtin packages resolve from the executing Nopal distribution, workspace packages resolve from checked-in repository paths, and npm packages are downloaded at their locked exact version.
The npm adapter verifies SHA-512 SRI, rejects traversal, links, special files, duplicate paths, and oversized archives, then installs into a content-addressed store.
Core re-hashes the installed result through the same inspection path used by launch.

Ambient Pi extensions, skills, prompt templates, and themes are disabled by default.
A checked-in contract may explicitly inherit non-executable ambient resource kinds.
Executable extensions remain restricted to the byte-verified Nopal adapter because allowing an ambient or third-party extension would bypass the enforcement boundary.

A fresh supported Git repository receives one six-file baseline containing the project manifest, policy, gates, bundle, lock, and Beislið workflow.
Any partial Nopal state, existing Beislið state, or legacy `.crust` state is preserved and rejected instead of being merged with generated authority.

## Consequences

Project startup is reproducible and offline after explicit synchronization.
Reviewers can see package identity and exact evidence in repository changes.
Registry failures and integrity failures are attributed to the package, source, and control boundary that failed.

Updates require an explicit effectful command and may be slower because package trees are extracted and hashed before a lock is proposed.
The initial npm implementation requires an `npm` executable for explicit update and synchronization, while bare launch has no such dependency.
Packages containing symbolic links or non-portable archive paths are intentionally unsupported.
