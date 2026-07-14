# ADR 0010: Adopt Nopal as the product identity with a clean technical cutover

- Status: Accepted
- Date: 2026-07-10
- Decision owners: Nopal maintainers
- Supersedes the repository's pre-Nopal product identity and naming decisions.

## Context

The product was previously named Crust and exposed that name throughout its binary, configuration, schemas, packages, extensions, documentation, and coordination UI.
Pi remains the internal agent runtime and familiar Pi workflows remain important, but neither Pi nor the former Crust identity should define the product users encounter.
The product needs a coherent identity centered on trust and assurance, with Nopal as the final product name and `nopal.sh` as its canonical domain.
There are no external users requiring a compatibility window.

## Decision

The product is named Nopal.
Its shared deterministic engine is named Nopal Core.
Nopal Core decides, selects, normalizes, and explains, but does not execute gates or contact agents or external services.
Nopal and Beislið are sibling surfaces over Nopal Core, and Beislið remains usable without the Nopal application or Pi distribution.

The rename is an atomic clean break across every active Nopal-owned technical surface.
The executable is `nopal`, repository configuration lives under `.nopal/`, user configuration and state use Nopal paths, environment variables use the `NOPAL_` prefix, packages use `nopal-*` names, and Nopal-owned envelopes use the `nopal.*/v1` namespace.
No `crust` executable alias, legacy configuration discovery, environment-variable alias, dual schema support, or automatic migration behavior is provided.
Existing local configuration and state may be moved or discarded manually.

The flagship interactive surface is the Field.
Bare `nopal` opens the Field, `nopal field` opens it explicitly, and `nopal field inspect` exposes the read-only coordination snapshot previously described as the herd query.
Herdr keeps its own product name at the `nopal bridge herdr` integration boundary.

The GitHub repository moves from `sandsower/crust` to `sandsower/nopal` as a coordinated release step.
Active repository metadata, installation instructions, automation, and links target `sandsower/nopal` and do not depend on repository redirects as compatibility behavior.

Pre-Nopal implementation plans and internal workflow artifacts are not part of the public source tree.
The public repository keeps current architectural decisions and explains their rationale without requiring access to a private issue tracker.

This decision changes identity and vocabulary only.
It does not implement the future Plot persistence model, the broader Workflow and Spine domain model, or a host-neutral Nopal Core API.

## Consequences

Nopal has one outward and technical identity instead of a facade over legacy names.
Repositories containing only `.crust/` are unconfigured from Nopal's perspective.
Consumers must adopt the Nopal binary, paths, environment variables, and envelopes together.
The clean break avoids permanent compatibility code and prevents legacy terminology from leaking back into active surfaces.
The coordinated cutover has a wide verification surface, so release proof must include configuration discovery, schema output, Field entry points, Pi launch behavior, active-tree identity checks, and repository-link checks.

## Rejected alternatives

### Facade-first rename

Renaming only the binary, UI, and prose would leave users encountering Crust in configuration, schemas, packages, and diagnostics.
That would weaken Nopal's identity and make the technical boundary harder to understand.

### Compatibility window

Supporting both identities would require aliases, dual discovery, migration rules, and mixed-version behavior without serving an external user need.
That complexity would also keep the superseded identity active indefinitely.
