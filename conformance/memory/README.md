# Memory contract conformance (MemoryProvider)

Status: authoritative runner, added against `nopal.memory_provider/v1` (see [`../../contracts/memory.md`](../../contracts/memory.md)).
Formerly cataloged as C4; the provisional placeholder has been replaced.

## Usage

```sh
conformance/memory/run.sh --memento <path-to-memento-vault-checkout>
```

The path must be a memento-vault checkout whose `memento` package is importable, either via an editable install at `<checkout>/.venv/bin/python` or via `python3` on `PATH`.
The runner prefers `<checkout>/.venv/bin/python` when it exists and falls back to `python3` otherwise.

The runner is self-contained: `run.sh` builds a hermetic fixture vault in a temp directory, drives the provider's reference CLI (`python -m memento.pi_bridge`) against it, and deletes the temp directory on exit.
It never touches a real vault, queue, or config.

Exit code is 0 only if every check passes.
Non-zero otherwise.
Output is one `PASS <name>` or `FAIL <name>: <why>` line per check, followed by a summary line.

## What is verified

Nine checks, each a response-concept from the contract's operations table:

1. `status` - `provider.status` returns `vault_path`, `vault_exists` (true), `note_count` (int), `queue_path`, and `queued_capture_count` as either an int or the `{"skipped": "lock_unavailable"}` degraded shape.
2. `capture_direct` - `memory.capture` (direct, no `--queue`) returns a non-empty `path` and `queued: false`.
3. `get_roundtrip` - `memory.get` on that path returns the captured `title` and `content`.
4. `search_finds_capture` - `memory.search` for a distinctive token from the captured body returns a hit whose `path` matches the capture.
5. `run_lesson_ingest` - `memory.run_lesson` on a payload with `run_id` and `ticket_id` returns `created: true`, `queued: false`, and a non-empty `path`.
6. `search_finds_run_lesson_by_run_id` - `memory.search` for the `run_id` returns a hit at that path.
7. `search_finds_run_lesson_by_ticket_id` - `memory.search` for the `ticket_id` returns a hit at that path.
8. `search_miss_envelope` - `memory.search` for a token that cannot exist returns `results: []` plus a `miss.reason` (or top-level `reason`).
9. `queue_list` - `queue.list` returns `count`/`captures`/`queue_path`, or the `{"skipped": "lock_unavailable"}` degraded shape.

Checks 5-7 exercise contract guarantee 1 (capture-then-recall): a run lesson must be deterministically findable by either identifier immediately after ingest, with no embeddings and no network.

## Hermetic vault approach

The fixture vault mirrors memento's own `hermetic_vault_config` test fixture (`tests/test_run_lesson_ingest.py` on the provider's `main`): real `notes/`, `fleeting/`, `projects/`, `archive/` directories and the real grep search backend, with no qmd process and no embedding model.

Reached entirely through environment, since each CLI call is a fresh `python -m memento.pi_bridge` subprocess (no monkeypatching available):

- `MEMENTO_VAULT_PATH` points at the fixture vault directory.
- `MEMENTO_SEARCH_BACKEND=grep` forces the dependency-free backend regardless of what is installed on the host.
- `MEMENTO_VAULT_URL` and `MEMENTO_API_KEY` are unset so the provider never attempts remote mode.
- `HOME`, `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, and `XDG_STATE_HOME` all point inside the temp directory, so stray triage-health logs, access logs, vault-write locks, and any real `~/.config/memento-vault/memento.yml` on the host never leak in or out.
- `MEMENTO_PI_STATE_HOME` isolates the capture queue state root.

The whole temp directory is removed on exit via a trap, pass or fail.

## Files

- `run.sh` - the runner. Orchestrates the fixture vault, the `pi_bridge` calls, and the PASS/FAIL reporting.
- `check.py` - colocated assertion helper. Loads one JSON response file per check and asserts contract shape; also used by `run.sh` to extract a `path` field between dependent calls (e.g. capture then get). No new dependencies: stdlib `json` only.
- `schemas/nopal-memory-provider-v1.schema.json` - descriptor schema for the contract shape.
- `fixtures/minimal-provider-contract.json` - minimal descriptor fixture the schema validates.

## Reference implementation history

The runner originally found a response-serialization defect when deduplication candidates contained an internal Python `set`.
The reference implementation now removes internal-only fields before emitting JSON and carries a regression test for this case.
Checks 5-7 preserve the original failure shape by running after the fixture vault already contains a note.
