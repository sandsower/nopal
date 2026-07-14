#!/usr/bin/env python3
"""Assertion helper for conformance/memory/run.sh.

Each subcommand loads one or more JSON response files produced by
``python -m memento.pi_bridge <op>`` and asserts the response-concept shape
the memory contract (contracts/memory.md, nopal.memory_provider/v1) requires.

Exit 0 and print nothing on success. Exit 1 and print one reason line on
failure. This script never grades conformance itself -- run.sh turns its
exit code into the "PASS <name>" / "FAIL <name>: <why>" lines the contract
asks for.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any, NoReturn


def fail(message: str) -> NoReturn:
    print(message)
    sys.exit(1)


def load(path: str) -> Any:
    try:
        raw = Path(path).read_text(encoding="utf-8")
    except OSError as exc:
        fail(f"could not read {path}: {exc}")
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        fail(f"{path} is not valid JSON: {exc}\n--- raw ---\n{raw[:500]}")


def require_no_error(payload: dict, context: str) -> None:
    if isinstance(payload, dict) and payload.get("error"):
        fail(f"{context}: unexpected error {payload.get('error')!r} (reason={payload.get('reason')!r})")


def require_keys(payload: dict, keys: list[str], context: str) -> None:
    missing = [k for k in keys if k not in payload]
    if missing:
        fail(f"{context}: missing key(s) {missing}; got keys {sorted(payload.keys())}")


# --- status ---


def check_status(args: list[str]) -> None:
    (status_path,) = args
    payload = load(status_path)
    if not isinstance(payload, dict):
        fail("status response is not a JSON object")
    require_no_error(payload, "status")
    require_keys(payload, ["vault_path", "vault_exists", "note_count", "queue_path"], "status")
    if "queued_capture_count" not in payload:
        fail(f"status: missing key ['queued_capture_count']; got keys {sorted(payload.keys())}")
    if not isinstance(payload["vault_exists"], bool):
        fail(f"status.vault_exists is not a bool: {payload['vault_exists']!r}")
    if payload["vault_exists"] is not True:
        fail("status.vault_exists is False for a fixture vault that was created before this call")
    if not isinstance(payload["note_count"], int):
        fail(f"status.note_count is not an int: {payload['note_count']!r}")
    qcc = payload["queued_capture_count"]
    if isinstance(qcc, dict):
        if qcc != {"skipped": "lock_unavailable"}:
            fail(f"status.queued_capture_count degraded shape unexpected: {qcc!r}")
    elif not isinstance(qcc, int):
        fail(f"status.queued_capture_count is neither int nor lock_unavailable skip shape: {qcc!r}")
    if not isinstance(payload.get("pi_bridge_health"), dict) or "status" not in payload["pi_bridge_health"]:
        fail(f"status.pi_bridge_health.status missing: {payload.get('pi_bridge_health')!r}")


# --- capture (direct write) ---


def check_capture(args: list[str]) -> None:
    (capture_path,) = args
    payload = load(capture_path)
    if not isinstance(payload, dict):
        fail("capture response is not a JSON object")
    require_no_error(payload, "capture")
    require_keys(payload, ["path", "queued"], "capture")
    if not isinstance(payload["path"], str) or not payload["path"].strip():
        fail(f"capture.path is not a non-empty string: {payload['path']!r}")
    if payload["queued"] is not False:
        fail(f"capture.queued expected False for a direct (non --queue) capture, got {payload['queued']!r}")


# --- get (round trip after capture) ---


def check_get(args: list[str]) -> None:
    get_path, expected_title, expected_body_substr = args
    payload = load(get_path)
    if not isinstance(payload, dict):
        fail("get response is not a JSON object")
    require_no_error(payload, "get")
    require_keys(payload, ["path", "title", "content"], "get")
    if payload["title"] != expected_title:
        fail(f"get.title {payload['title']!r} != captured title {expected_title!r}")
    if expected_body_substr not in payload["content"]:
        fail(f"get.content does not contain the captured body marker {expected_body_substr!r}")


# --- search hit (capture round trip, and run-lesson recall by run_id/ticket_id) ---


def check_search_hit(args: list[str]) -> None:
    search_path, expected_path = args
    payload = load(search_path)
    if not isinstance(payload, dict):
        fail("search response is not a JSON object")
    require_no_error(payload, "search")
    if "results" not in payload:
        fail(f"search: missing key ['results']; got keys {sorted(payload.keys())}")
    results = payload["results"]
    if not isinstance(results, list) or not results:
        fail(f"search.results is empty for a query expected to hit {expected_path!r}: {payload!r}")
    paths = [r.get("path") for r in results if isinstance(r, dict)]
    if expected_path not in paths:
        fail(f"search.results paths {paths!r} do not include expected {expected_path!r}")


# --- run-lesson ingest ---


def check_run_lesson(args: list[str]) -> None:
    (run_lesson_path,) = args
    payload = load(run_lesson_path)
    if not isinstance(payload, dict):
        fail("run-lesson response is not a JSON object")
    require_no_error(payload, "run-lesson")
    require_keys(payload, ["created", "path", "queued"], "run-lesson")
    if payload["created"] is not True:
        fail(f"run-lesson.created expected True, got {payload['created']!r}")
    if payload["queued"] is not False:
        fail(f"run-lesson.queued expected False (run-lesson writes directly, never queues), got {payload['queued']!r}")
    if not isinstance(payload["path"], str) or not payload["path"].strip():
        fail(f"run-lesson.path is not a non-empty string: {payload['path']!r}")


# --- search miss envelope ---


def check_search_miss(args: list[str]) -> None:
    (search_path,) = args
    payload = load(search_path)
    if not isinstance(payload, dict):
        fail("search (miss) response is not a JSON object")
    if payload.get("results") != []:
        fail(f"search.results expected [] for a no-such-token query, got {payload.get('results')!r}")
    miss = payload.get("miss")
    top_level_reason = payload.get("reason")
    if isinstance(miss, dict):
        reason = miss.get("reason")
    else:
        reason = None
    if not reason and not top_level_reason:
        fail(f"search miss envelope carries no miss.reason or top-level reason: {payload!r}")


# --- queue list ---


def check_queue_list(args: list[str]) -> None:
    (queue_path,) = args
    payload = load(queue_path)
    if not isinstance(payload, dict):
        fail("queue list response is not a JSON object")
    if payload == {"skipped": "lock_unavailable"}:
        return
    require_no_error(payload, "queue list")
    require_keys(payload, ["count", "captures", "queue_path"], "queue list")
    if not isinstance(payload["count"], int):
        fail(f"queue.count is not an int: {payload['count']!r}")
    if not isinstance(payload["captures"], list):
        fail(f"queue.captures is not a list: {payload['captures']!r}")
    if not isinstance(payload["queue_path"], str) or not payload["queue_path"].strip():
        fail(f"queue.queue_path is not a non-empty string: {payload['queue_path']!r}")


# --- extract (bash helper: pull one top-level string field out of a JSON file) ---


def check_extract(args: list[str]) -> None:
    json_path, key = args
    payload = load(json_path)
    if not isinstance(payload, dict) or key not in payload:
        fail(f"{json_path}: missing key {key!r}; got keys {sorted(payload.keys()) if isinstance(payload, dict) else type(payload)}")
    value = payload[key]
    if not isinstance(value, str):
        fail(f"{json_path}.{key} is not a string: {value!r}")
    print(value)


_MODES = {
    "status": check_status,
    "capture": check_capture,
    "get": check_get,
    "search_hit": check_search_hit,
    "run_lesson": check_run_lesson,
    "search_miss": check_search_miss,
    "queue_list": check_queue_list,
    "extract": check_extract,
}


def main(argv: list[str]) -> int:
    if not argv or argv[0] not in _MODES:
        print(f"usage: check.py <{'|'.join(_MODES)}> [args...]")
        return 2
    _MODES[argv[0]](argv[1:])
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
