# Nopal product surface conformance

Status: merged from the retired `c1-olin` and `c3-beislid-process` conformance homes.

Nopal's config/envelope surface (formerly cataloged as C1) and its process/proof-artifact surface (formerly cataloged as C3) are no longer inter-product contracts.
Only the Rondo execution and Memento memory boundaries remain genuinely foreign parties.
This home still holds both surfaces to the same conformance discipline; see [`docs/surface/config-and-envelopes.md`](../../docs/surface/config-and-envelopes.md) and [`docs/surface/process-and-proof.md`](../../docs/surface/process-and-proof.md) for scope.

This is where the closed safety lattices stay explicit:

- policy decisions (`allow` < `ask` < `deny`)
- policy placements (`shared_user_runtime` < `dedicated_repo_runtime` < `dedicated_run_runtime` < `blocked`)
- run-ledger statuses
- the destructive and secret-bearing safety floors
- review-risk classification (`low` < `medium` < `high`): unknown `total_changes` can never produce `low` and never triggers the high-total-changes rule; a missing `review_policy.jsonc` fails the seam (`ok: false`) rather than guessing; a missing or broken `gates.jsonc` degrades to the `repo_root` scope model rather than failing the seam; missing gate `parallel_safe` or `mutates` metadata conservatively fails the multi-scope fast-path check. See `crates/nopal-core/src/review_policy.rs` unit tests and `crates/nopal-cli/tests/cli.rs::review_risk_*` for the fixtures exercising these fallbacks.

Additive vocabulary tokens elsewhere are data, not fixed ABI vocabulary, and do not require a new `/vN`.
Unknown safety vocabulary must degrade conservatively instead of widening behavior.
Fixtures should make that conservative fallback observable, not just described, so the closed lattices stay explicit in evidence.

## Structured Session surface

The native Field and Pi Session walking skeleton uses the versioned command and event contracts documented in [`docs/surface/session.md`](../../docs/surface/session.md).
Checked fixtures under [`session/`](session/) cover the prompt command, all v1 event variants, additive fields, a stale kind, and a foreign Session identity.
Their consumer runner is `cargo test -p nopal-feed-client session::tests`.

## Config/envelope surface (ex-C1)

Runner convention:

```sh
cargo test --workspace
```

Future dedicated runner:

```sh
conformance/surface/config-and-envelopes-run.sh
```

Fixture sources are the checked-in `examples/` trees, CLI golden files under `crates/nopal-cli/tests/golden/`, and run-ledger fixtures under `crates/nopal-cli/tests/fixtures/run-ledger/`.

## Process/proof-artifact surface (ex-C3)

Producer-side runner convention:

```sh
python3 <path-to-beislid>/scripts/validate_export.py <fixture-or-export>
```

Future cross-consumer runner:

```sh
conformance/surface/process-and-proof-run.sh --beislid <path-to-beislid> --rondo <path-to-rondo>
```

The first cross-consumer fixtures should reproduce the known compatibility gap: a valid approved-slice export with structured `boundaries` and `proof_requirements` must load through Rondo's intake or through an explicitly versioned adapter.
