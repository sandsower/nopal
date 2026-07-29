# ADR 0017: Ship a self-contained versioned Pi distribution

- Status: Accepted
- Date: 2026-07-29
- Decision owners: Nopal maintainers

## Context

Nopal's enforcement guarantee depends on the exact Nopal CLI, Pi runtime closure, Node executable, policy adapter, and curated workflow resources agreeing at launch.
Requiring users to assemble those pieces from `PATH` would make installation nondeterministic and would let unrelated package-manager updates change the protected execution boundary.
The v0.3 clean break also requires release artifacts to exclude superseded product components rather than merely stop invoking them.
Installation, intentional update, rollback, and offline launch need direct and independently inspectable behavior.

## Decision

Each supported platform receives one deterministic `tar.gz` archive containing the Nopal CLI, official Node.js `22.22.0`, exact Pi `0.80.6` and its complete dependency tree, the Nopal policy adapter, pinned Beislið skills, licenses, provenance, and installer material.
The x86-64 GNU/Linux archive targets glibc 2.35 or newer, is built on Ubuntu 22.04, and must launch successfully on the pinned Debian 12 compatibility image before publication.
The archive contains no former management UI, native desktop runtime, coordination protocol, compatibility executable, or unrelated Pi extension.

The release workflow downloads Node from the official release archive and verifies its platform archive digest before staging the executable and license.
It installs Pi with the npm client shipped by that exact Node archive and verifies a launcher-compatible hash of every regular file, symlink target, and executable mode in the runtime tree.
The packager verifies exact source tag, binary version, source commit, target runtime profile, Pi package identity, Pi tree integrity, Beislið provenance, and required licenses before publishing any archive.
A canonical PAX writer fixes member order, metadata, and gzip headers so identical inputs produce identical bytes.
`distribution.json` records the exact source, target, runtime, adapter, and workflow-resource identities inside the archive.

The archive installer copies into an immutable version-and-target directory before atomically switching a `current` symlink.
A prior current release becomes `previous`.
Rollback exchanges those two links after validating that the previous release contains a manifest and executable.
The stable launcher under `$prefix/bin` points only through `current`, so installation and rollback do not rewrite executable bytes in place.
Conflicting bytes under an already installed release identity fail rather than being merged or overwritten.

Installed launch resolves Pi and Node relative to the selected Nopal executable before considering source-development or explicit test candidates.
Launch copies verified runtime bytes into a private content-addressed snapshot and starts Pi offline.
Plain Pi sessions started outside the installed Nopal launcher remain outside Nopal's assurance boundary.
Project package synchronization stays explicit through `nopal sync` and `nopal update`; it is separate from installing a new Nopal release archive.

The v0.2 release line advances intentionally to `v0.3.0` on the first release containing this boundary.
Later v0.3 releases resume normal patch increments.

## Consequences

A release is larger because it includes Node and Pi dependencies, but its enforcement boundary no longer depends on ambient runtime installation.
Platform runtime hashes and official Node archive digests are release inputs that must be reviewed when Pi or Node changes.
Installers require an absolute prefix and use the archive's verified Node executable for manifest validation and atomic symlink replacement.
Users can install and roll back without contacting a service after downloading and verifying archives.
Release tests can prove exact membership, reproducibility, installation switching, rollback, and publication conflict behavior with synthetic runtime fixtures.
The release workflow provides the production proof against exact official runtime bytes.

## Rejected alternatives

### Depend on system Pi and Node

This would let `PATH`, package managers, and global dependency mutation alter the runtime after release publication.
Version checks alone do not bind dependency bytes, native assets, symlink targets, or executable modes.

### Package only the Nopal binary

A thin archive cannot substantiate offline launch or establish which Pi adapter and workflow resources receive authority.
It would transfer the most important integrity decisions to installation time.

### Overwrite one installation directory in place

An interrupted copy could leave a mixed runtime and would destroy the immediate rollback target.
Versioned immutable directories plus an atomic link switch preserve a complete old or new selection.

### Keep removed components in an unused archive directory

A build-only exclusion would leave an active attic and make the product boundary ambiguous.
Git history and the final v0.2 release marker are the recovery surface for the removed implementation.
