#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <installed-nopal-launcher>" >&2
  exit 2
fi

launcher=$1
case "$launcher" in
  /*) ;;
  *) echo "installed launcher path must be absolute: $launcher" >&2; exit 2 ;;
esac
if [ ! -x "$launcher" ]; then
  echo "installed launcher is not executable: $launcher" >&2
  exit 1
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/nopal-packaged-launch.XXXXXX")
cleanup() {
  find "$tmp" -type d -exec chmod u+rwx {} + 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM
repo=$tmp/repo
poison=$tmp/poison
override=$tmp/untrusted-distribution
mkdir -p "$repo" "$poison" "$tmp/home" "$tmp/data" "$tmp/state" \
  "$override/extensions/policy-gate" "$override/resources/beislid/skills"
for file in index.ts classifier.ts guard.ts nopal-cli.ts; do
  printf 'throw new Error("untrusted override executed");\n' \
    > "$override/extensions/policy-gate/$file"
done
printf 'untrusted license\n' > "$override/resources/beislid/LICENSE"
printf '{}\n' > "$override/resources/beislid/provenance.json"
HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/home/config" \
  git -c init.defaultBranch=main -C "$repo" init -q
set +e
HOME="$tmp/home" \
XDG_CONFIG_HOME="$tmp/home/config" \
NOPAL_DATA_DIR="$tmp/data" \
BEISLID_STATE_DIR="$tmp/state" \
NOPAL_DISTRIBUTION_ROOT="$override" \
PATH=/usr/bin:/bin \
  "$launcher" --dir "$repo" --json > "$tmp/scaffold.json" 2> "$tmp/scaffold.stderr"
scaffold_status=$?
set -e
if [ "$scaffold_status" -ne 1 ] || [ ! -f "$repo/.nopal/nopal.lock" ]; then
  cat "$tmp/scaffold.stderr" >&2
  echo "packaged Nopal did not create the blocked unknown-project baseline" >&2
  exit 1
fi
cat > "$repo/.nopal/gates.jsonc" <<'JSON'
{
  "version": "nopal.gates/v1",
  "gates": [
    { "id": "packaged-launch", "stage": "pre_pr", "argv": ["true"] }
  ]
}
JSON
for name in pi node npm curl; do
  cat > "$poison/$name" <<EOF
#!/bin/sh
touch "$tmp/ambient-$name-ran"
exit 97
EOF
  chmod 0755 "$poison/$name"
done

if ! printf '{"type":"get_state"}\n' \
  | HOME="$tmp/home" \
    XDG_CONFIG_HOME="$tmp/home/config" \
    NOPAL_DATA_DIR="$tmp/data" \
    BEISLID_STATE_DIR="$tmp/state" \
    NOPAL_DISTRIBUTION_ROOT="$override" \
    PATH="$poison:/usr/bin:/bin" \
    "$launcher" --dir "$repo" -- --mode rpc --no-session \
    > "$tmp/rpc.jsonl" 2> "$tmp/stderr"; then
  cat "$tmp/stderr" >&2
  cat "$tmp/rpc.jsonl" >&2
  echo "packaged Nopal failed to launch bundled Pi" >&2
  exit 1
fi

python3 - "$tmp/rpc.jsonl" <<'PY'
import json,sys
records=[]
for line in open(sys.argv[1]):
    line=line.strip()
    if line:
        records.append(json.loads(line))
if not any(
    row.get("type") == "response"
    and row.get("command") == "get_state"
    and row.get("success") is True
    for row in records
):
    raise SystemExit("packaged Pi did not answer the offline RPC state request")
PY

for name in pi node npm curl; do
  if [ -e "$tmp/ambient-$name-ran" ]; then
    echo "packaged launch used poisoned ambient $name" >&2
    exit 1
  fi
done
if [ ! -f "$repo/.nopal/nopal.lock" ] || [ ! -f "$repo/.beislid/workflow.md" ]; then
  echo "packaged launch did not create and consume the project baseline" >&2
  exit 1
fi
if ! find "$tmp/state/runs/enforcement" -name run.json -type f -print -quit | grep -q .; then
  echo "packaged launch did not publish enforcement ledger evidence" >&2
  exit 1
fi

printf 'packaged Nopal launched bundled Pi offline with continuous enforcement\n'
