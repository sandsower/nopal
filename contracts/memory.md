# Memory contract: MemoryProvider

Status: **v1** (2026-07-06).
Formerly cataloged as contract **C4**, this remains an inter-product contract because Memento is a separate product from Nopal.
Owner: **Memento**.
Surface: `nopal.memory_provider/v1`.
Reference implementation: `python3 -m memento.pi_bridge` (Memento Vault 5.0.0 or newer, with queueing, run-lesson ingestion, and fail-closed locking).
Descriptor home: [`../conformance/memory/`](../conformance/memory/).
Schema seed: [`../conformance/memory/schemas/nopal-memory-provider-v1.schema.json`](../conformance/memory/schemas/nopal-memory-provider-v1.schema.json).

## Scope

The memory seam nopal and thin Pi extensions rely on without coupling to Memento host glue:

- session context packet / briefing (lifecycle injection)
- prompt recall
- search and get by durable note id/path
- capture (direct write and queue append) with dead-letter degradation
- run-lesson ingest: deterministic evidence/lesson preservation from Rondo/Beislið runs
- queue listing and health reporting

Memento owns retrieval quality and durable vault semantics.
Nopal consumes a MemoryProvider; it must not become a run ledger and must not embed Memento's queue internals.

## Transport

Short-lived JSON subprocess: each operation is one CLI invocation printing one JSON object to stdout.
A provider may satisfy the contract over another transport (HTTP, MCP) if it exposes the same concepts and passes the conformance runner.
Errors are in-band: `{"error": "...", "reason": "<stable_reason_code>"}`; consumers match on `reason` codes, never on error prose.

## Operations

| Operation | Reference subcommand | Required request concepts | Required response concepts |
|---|---|---|---|
| `provider.status` | `status` | - | `vault_path`, `vault_exists`, `note_count`, `queued_capture_count` (int or degraded), `queue_path`, `pi_bridge_health.status` |
| `context.briefing` | `briefing` | session identity | lifecycle result (below) |
| `context.recall` | `recall` | prompt text | lifecycle result |
| `context.session` | `session-context` | session identity | lifecycle result; `metadata.sections.queue` carries queue provenance |
| `context.tool` | `tool-context` | tool call summary | lifecycle result |
| `memory.search` | `search --query` | query string | hit: `results[{path, title, score, backend}]`; miss: `results: []` plus `miss.{reason, recovery_hints}` |
| `memory.get` | `get` | durable note path | `path`, `title`, `content`; or `error` |
| `memory.capture` | `capture [--queue]` | title, body, provenance | direct: `path`, `queued: false`; queued: `id`, `queued: true`, `queue_path`; dead-letter: `queued: false`, `dead_lettered: true`, `reason` |
| `memory.run_lesson` | `run-lesson --payload <json>` | `run_id`, `ticket_id` (required); title/lesson_text/evidence_paths/tags optional | `created: true`, `path`, `queued: false`; validation errors carry stable `reason` codes |
| `queue.list` | `queue list` | - | `count`, `captures[]`, `queue_path`; or `{"skipped": "lock_unavailable"}` |

Lifecycle result shape (`briefing`/`recall`/`session-context`/`tool-context`): `should_inject` (bool), `content`, `source`, `results`; optional `reason`, `metadata`.

## Guarantees (load-bearing v1 semantics)

1. **Capture-then-recall**: a run lesson ingested with `run_id` and `ticket_id` is deterministically findable by either identifier via `memory.search` immediately afterward, without embeddings or network (proven by memento `tests/test_run_lesson_ingest.py`). `memory.run_lesson` writes a curated note directly; it never merely queues.
2. **Fail-closed lock, degraded reads**: when the queue lock is unavailable, read/status surfaces degrade to `{"skipped": "lock_unavailable"}` instead of crashing or reading unlocked; enqueue retries then dead-letters (`dead_lettered: true`, `reason: "queue_lock_unavailable"`). Consumers must treat these degraded shapes as stable contract surface, not incidental.
3. **Search miss envelope**: misses are machine-readable (`miss.reason` from an open reason vocabulary such as `no_exact_match`, `backend_unavailable`, `empty_vault`) with recovery hints; an empty `results` array alone is not a valid miss.
4. **In-band errors with stable reason codes**: e.g. `missing_required_field`, `payload_invalid_json`, `invalid_automated_run_lesson`, `lock_timeout`, `vault_missing`. Reason codes are additive open vocabulary; consumers must handle unknown codes conservatively.

## Health

Queue health has one graded authority: the provider health check (memento `health.py` `queue health` check) with `PASS`/`WARN`/`FAIL` on backlog-count and oldest-age thresholds.
`provider.status` and the session-context queue section are presence/provenance signals only (counts, paths, legacy-fallback provenance); they carry no grades and must not be interpreted as health verdicts.

## Versioning

`nopal.memory_provider/v1` versions request/response shapes, reason-code semantics, the degraded-shape guarantees above, and the capture-then-recall guarantee.
It does not version search backend internals, scoring, note frontmatter internals, or queue file layout.
Additive fields and new reason codes stay within `/v1`; removing required fields, changing degraded shapes, or weakening guarantee 1 or 2 requires a new version.

## Conformance status

The reference implementation on Memento Vault main passes the runner **9/9**.
The first bug the runner caught made `run-lesson` and `capture` crash while serializing a `set` in deduplication candidate rows on any non-empty vault, silently breaking guarantee 1; it is fixed and regression-tested.

## Known gaps (documented, not contract surface)

- Memento `lifecycle.py` still carries a private JSONL reader for queue counts; the session-context queue block may classify malformed lines slightly differently than `queue.py`. This is follow-up scope on the Memento side, and consumers must not depend on malformed-line sentinel shapes.
- Provider health declaration in `.nopal/integrations.jsonc` (so `nopal status` surfaces memory health) needs an integrations vocabulary addition; follow-up on the Nopal side.

## Conformance home

[`conformance/memory/`](../conformance/memory/) - descriptor schema, minimal fixture, and `run.sh`, which verifies against a real provider checkout over a hermetic fixture vault:

- status shape and degraded-count tolerance
- capture-then-search/get round trip
- run-lesson ingest recallable by `run_id` and by `ticket_id`
- search miss envelope shape
- queue list shape
