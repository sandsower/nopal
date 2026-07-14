# Nopal contract catalog

Status: two inter-product contracts.

Nopal Core, the `nopal` binary and Pi distribution, and Beislið are sibling surfaces over the same engine, not separate parties.
That leaves only two boundaries where a genuinely separate product is on the other side, so "contract" is reserved for those: **execution** (nopal <-> rondo) and **memory** (nopal <-> memento).

The former C1 (config/envelope schemas) and C3 (process/proof artifacts) are no longer contracts - they are Nopal's own versioned product surface, documented under [`../docs/surface/`](../docs/surface/) and held to the same conformance discipline at [`../conformance/surface/`](../conformance/surface/), but evolvable: only the closed safety lattices are frozen ABI, and vocabularies stay open.

The machine-readable index is [`catalog.json`](catalog.json).
Human notes live beside it:

| ID | Contract | Owner | Status | Notes | Conformance home |
|---|---|---|---|---|---|
| execution | Rondo Core service API (formerly C2) | Rondo Core | Provisional | [`execution.md`](execution.md) | [`../conformance/execution`](../conformance/execution) |
| memory | MemoryProvider (formerly C4) | Memento | Provisional | [`memory.md`](memory.md) | [`../conformance/memory`](../conformance/memory) |

## Versioning rule

For these two inter-product contracts, versions are semantic by wire/on-disk compatibility, not by implementation release.
Additive optional fields and additive vocabulary tokens may stay on the same `/vN` only when existing consumers keep passing.
Required fields, enum tightening, path layout changes, or verdict/diagnostic semantic changes require a new `/vN` or an explicitly documented fix-forward migration window.

Stable diagnostic codes are part of the contract.
Consumers match on `code` and `severity`, never message prose.
When a newer token appears on an older consumer, the safe rule is conservative degradation, not silent widening.

In practice that means additive tokens are data-shaped compatibility, not closed lattices, while closed lattices stay ABI-sensitive and must be called out explicitly in their contract notes and conformance homes.
Contract docs should not imply every vocabulary axis is closed.

This "enum tightening requires a new `/vN`" discipline now applies only to the execution and memory contracts.
Nopal's own product surface (`../docs/surface/`) follows a lighter rule: only closed safety lattices are ABI-frozen; other vocabulary is additive and open, because there is no foreign consumer to protect against silent widening.

## Distribution readiness rule

A contract is distribution-ready when:

1. The owner has a schema pointer or normative parser/validator pointer.
2. The catalog entry names a reference implementation.
3. The conformance home has fixtures and a runner convention.
4. At least one producer and one consumer pass the fixtures when the contract is cross-product.

Execution and memory remain provisional until each has complete integration evidence and a hardened provider seam.

## Distro manifest seed

The first distro-manifest sketch is [`distro-manifest.md`](distro-manifest.md).
It records the bootstrap-time compatibility model: a Nopal bundle pins extension package versions, core binary/service versions, and the contract versions each package claims to produce or consume.
