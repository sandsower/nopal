#!/usr/bin/env bash
# Conformance runner for nopal.memory_provider/v1 (contracts/memory.md).
#
# Proves that a MemoryProvider implementation (reference: memento-vault's
# `python -m memento.pi_bridge`) satisfies the contract by exercising it
# against a hermetic fixture vault: grep search backend, no qmd, no
# embeddings, no network, an isolated MEMENTO_PI_STATE_HOME/HOME/XDG tree so
# nothing here reads or writes the caller's real vault, queue, or config.
#
# Usage:
#   conformance/memory/run.sh --memento <path-to-memento-vault-checkout>
#
# Exit 0 if every check passes, non-zero otherwise. Prints one
# "PASS <name>" / "FAIL <name>: <why>" line per check plus a final summary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_PY="$SCRIPT_DIR/check.py"

MEMENTO_ROOT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --memento)
      MEMENTO_ROOT="${2:-}"
      shift 2
      ;;
    -h|--help)
      echo "usage: $0 --memento <path-to-memento-vault-checkout>"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      echo "usage: $0 --memento <path-to-memento-vault-checkout>" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$MEMENTO_ROOT" ]]; then
  echo "usage: $0 --memento <path-to-memento-vault-checkout>" >&2
  exit 2
fi
if [[ ! -d "$MEMENTO_ROOT" ]]; then
  echo "not a directory: $MEMENTO_ROOT" >&2
  exit 2
fi
MEMENTO_ROOT="$(cd "$MEMENTO_ROOT" && pwd)"

if [[ -x "$MEMENTO_ROOT/.venv/bin/python" ]]; then
  PYTHON="$MEMENTO_ROOT/.venv/bin/python"
else
  PYTHON="python3"
fi

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/nopal-memory-conformance.XXXXXX")"
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT

# --- Hermetic fixture vault (mirrors memento's own hermetic_vault_config
# fixture in tests/test_run_lesson_ingest.py: real vault dirs + real grep
# search backend, everything else isolated to this temp tree). ---
VAULT="$WORKDIR/vault"
mkdir -p "$VAULT/notes" "$VAULT/fleeting" "$VAULT/projects" "$VAULT/archive"
mkdir -p "$WORKDIR/home/.config" "$WORKDIR/runtime" "$WORKDIR/xdg-state" "$WORKDIR/pi-state"

export HOME="$WORKDIR/home"
export XDG_CONFIG_HOME="$WORKDIR/home/.config"
export XDG_RUNTIME_DIR="$WORKDIR/runtime"
export XDG_STATE_HOME="$WORKDIR/xdg-state"
export MEMENTO_PI_STATE_HOME="$WORKDIR/pi-state"
export MEMENTO_VAULT_PATH="$VAULT"
export MEMENTO_SEARCH_BACKEND="grep"
export PYTHONPATH="$MEMENTO_ROOT${PYTHONPATH:+:$PYTHONPATH}"
unset MEMENTO_VAULT_URL MEMENTO_API_KEY MEMENTO_DEBUG MEMENTO_PI_PROCESSOR MEMENTO_PI_CAPTURE_QUEUE 2>/dev/null || true

bridge() {
  "$PYTHON" -m memento.pi_bridge "$@"
}

PASS_COUNT=0
FAIL_COUNT=0

# check <name> <check.py-mode> [args...]
# Runs check.py with the given mode/args; check.py itself loaded the JSON
# response file(s) that a prior `bridge ...` call wrote to $WORKDIR.
check() {
  local name="$1"
  shift
  local why
  if why="$("$PYTHON" "$CHECK_PY" "$@" 2>&1)"; then
    echo "PASS $name"
    PASS_COUNT=$((PASS_COUNT + 1))
  else
    echo "FAIL $name: $why"
    FAIL_COUNT=$((FAIL_COUNT + 1))
  fi
}

# --- a. provider.status ---
bridge status > "$WORKDIR/status.json" 2>"$WORKDIR/status.stderr" || true
check "status" status "$WORKDIR/status.json"

# --- b. capture-then-get/search round trip ---
TOKEN="CONF-CAPTURE-$(date -u +%Y%m%dT%H%M%SZ)-$$"
TITLE="Nopal memory conformance capture $TOKEN"
BODY="Distinctive body marker $TOKEN for the nopal.memory_provider/v1 capture round trip."

bridge capture --title "$TITLE" --body "$BODY" --session-id "conf-session" \
  > "$WORKDIR/capture.json" 2>"$WORKDIR/capture.stderr" || true
check "capture_direct" capture "$WORKDIR/capture.json"

if CAPTURE_NOTE_PATH="$("$PYTHON" "$CHECK_PY" extract "$WORKDIR/capture.json" path 2>"$WORKDIR/extract-capture-path.stderr")"; then
  bridge get --path "$CAPTURE_NOTE_PATH" > "$WORKDIR/get.json" 2>"$WORKDIR/get.stderr" || true
  check "get_roundtrip" get "$WORKDIR/get.json" "$TITLE" "$TOKEN"

  bridge search --query "$TOKEN" > "$WORKDIR/search-capture.json" 2>"$WORKDIR/search-capture.stderr" || true
  check "search_finds_capture" search_hit "$WORKDIR/search-capture.json" "$CAPTURE_NOTE_PATH"
else
  echo "FAIL get_roundtrip: capture response had no usable path ($(cat "$WORKDIR/extract-capture-path.stderr"))"
  FAIL_COUNT=$((FAIL_COUNT + 1))
  echo "FAIL search_finds_capture: capture response had no usable path, skipped"
  FAIL_COUNT=$((FAIL_COUNT + 1))
fi

# --- c. run-lesson recall by run_id and by ticket_id ---
RUN_ID="CONF-RUN-20260706T000000Z-cafe0001"
TICKET_ID="CONF-1"
cat > "$WORKDIR/run-lesson-payload.json" <<JSON
{
  "run_id": "$RUN_ID",
  "ticket_id": "$TICKET_ID",
  "title": "Conformance run-lesson: $TICKET_ID ($RUN_ID)",
  "lesson_text": "Conformance fixture run lesson proving capture-then-recall by run_id and ticket_id.",
  "evidence_paths": ["conformance://memory/run-lesson-fixture"],
  "tags": ["conformance"]
}
JSON

bridge run-lesson --payload "$WORKDIR/run-lesson-payload.json" \
  > "$WORKDIR/run-lesson.json" 2>"$WORKDIR/run-lesson.stderr" || true
check "run_lesson_ingest" run_lesson "$WORKDIR/run-lesson.json"

if RUN_LESSON_PATH="$("$PYTHON" "$CHECK_PY" extract "$WORKDIR/run-lesson.json" path 2>"$WORKDIR/extract-run-lesson-path.stderr")"; then
  bridge search --query "$RUN_ID" > "$WORKDIR/search-run-id.json" 2>"$WORKDIR/search-run-id.stderr" || true
  check "search_finds_run_lesson_by_run_id" search_hit "$WORKDIR/search-run-id.json" "$RUN_LESSON_PATH"

  bridge search --query "$TICKET_ID" > "$WORKDIR/search-ticket-id.json" 2>"$WORKDIR/search-ticket-id.stderr" || true
  check "search_finds_run_lesson_by_ticket_id" search_hit "$WORKDIR/search-ticket-id.json" "$RUN_LESSON_PATH"
else
  echo "FAIL search_finds_run_lesson_by_run_id: run-lesson response had no usable path ($(cat "$WORKDIR/extract-run-lesson-path.stderr"))"
  FAIL_COUNT=$((FAIL_COUNT + 1))
  echo "FAIL search_finds_run_lesson_by_ticket_id: run-lesson response had no usable path, skipped"
  FAIL_COUNT=$((FAIL_COUNT + 1))
fi

# --- d. search miss envelope ---
bridge search --query "zzz-no-such-token-zzz" > "$WORKDIR/search-miss.json" 2>"$WORKDIR/search-miss.stderr" || true
check "search_miss_envelope" search_miss "$WORKDIR/search-miss.json"

# --- e. queue list ---
bridge queue list > "$WORKDIR/queue-list.json" 2>"$WORKDIR/queue-list.stderr" || true
check "queue_list" queue_list "$WORKDIR/queue-list.json"

echo "---"
echo "nopal.memory_provider/v1 conformance: $PASS_COUNT passed, $FAIL_COUNT failed (provider: $MEMENTO_ROOT)"

if [[ "$FAIL_COUNT" -gt 0 ]]; then
  exit 1
fi
exit 0
