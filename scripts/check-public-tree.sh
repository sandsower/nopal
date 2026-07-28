#!/usr/bin/env bash
set -euo pipefail

failed=0

for path in plans .codex WORKFLOW.md; do
  if git ls-files "$path" "$path/**" | rg -q .; then
    echo "public-tree: tracked internal path: $path" >&2
    failed=1
  fi
done

# The checked-in Beislið workflow is project authority in the public distribution.
# Other Beislið paths remain execution residue and must not enter the source tree.
unexpected_beislid=$(git ls-files '.beislid/**' | rg -v '^\.beislid/workflow\.md$' || true)
if [[ -n "$unexpected_beislid" ]]; then
  printf 'public-tree: tracked internal Beislið path: %s\n' "$unexpected_beislid" >&2
  failed=1
fi

if git grep -nE '(OLI|RON|BEI|MEM)-[0-9]+' -- \
  ':(exclude)scripts/check-public-tree.sh'; then
  echo "public-tree: tracked files must explain behavior without private ticket shorthand" >&2
  failed=1
fi

if git grep -nE '/Users/|/private/tmp/|mcp__personal|linear\.app/(teotl|nopal)|victor@dala\.care|vicvalenzuela' -- \
  ':(exclude)scripts/check-public-tree.sh'; then
  echo "public-tree: found a private path, identity, or integration marker" >&2
  failed=1
fi

if (( failed != 0 )); then
  exit 1
fi

echo "public-tree: clean"
