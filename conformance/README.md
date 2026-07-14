# Contract conformance homes

This directory is the neutral home for Nopal contract fixtures and runner conventions.

Each contract subdirectory should eventually contain:

- `fixtures/`: versioned positive and negative examples
- `README.md`: contract-specific runner requirements
- `run.sh` or an owner-native runner command: deterministic, no LLM judge, exit 0 on pass and non-zero on conformance failure

Rules:

1. Conformance verdicts are deterministic: schema validation, string/byte comparison, stable diagnostic code, or exit code.
2. LLM output may be an input artifact, but it may never grade conformance.
3. Fixtures must be small enough to live in git or must name an immutable source artifact.
4. Cross-product contracts should test both producer and consumer behavior before Nopal marks them distribution-ready.

Current homes:

- [`surface`](surface/) - Nopal's own config/envelope and process/proof-artifact surface (formerly C1/C3)
- [`execution`](execution/) - active approved single-manifest Rondo Core service API (formerly C2)
- [`memory`](memory/) - provisional MemoryProvider API (formerly C4)
