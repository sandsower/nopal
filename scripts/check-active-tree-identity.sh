#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
failed=0

fail() {
  printf 'active-tree: %s\n' "$*" >&2
  failed=1
}

for path in \
  crates/nopal-feed-client \
  crates/nopal-rondo-client \
  crates/nopal-rondo-lifecycle \
  crates/nopal-native-lifecycle \
  crates/nopal-field-presentation \
  crates/nopal-field \
  crates/nopal-desktop-spike \
  extensions/nopal \
  extensions/show-me \
  extensions/usage-tracker \
  extensions/task-state \
  extensions/subagent-runner \
  extensions/mcp-bridge \
  contracts \
  conformance \
  native \
  scripts/install-macos-app.sh \
  .nopal/integrations.jsonc \
  .nopal/theme.json \
  .pi/settings.json \
  Makefile; do
  [[ ! -e "$path" && ! -L "$path" ]] || fail "removed product path remains active: $path"
done

python3 - "$root/Cargo.toml" <<'PY' || failed=1
import sys,tomllib
members=tomllib.load(open(sys.argv[1],"rb"))["workspace"]["members"]
expected=["crates/nopal-ledger-json","crates/nopal-core","crates/nopal-cli"]
if members != expected:
    raise SystemExit(f"active-tree: workspace members are {members!r}, expected {expected!r}")
PY

extension_dirs=$(find extensions -mindepth 1 -maxdepth 1 -type d -print | LC_ALL=C sort)
if [[ "$extension_dirs" != "extensions/policy-gate" ]]; then
  fail "release-owned extension set is not exactly extensions/policy-gate: $extension_dirs"
fi

if git grep -n -i -E \
  'nopal[-_](field|rondo|desktop|native|feed)|extensions/(nopal|show-me|usage-tracker|task-state|subagent-runner|mcp-bridge)|crates/nopal-(field|rondo|desktop|native|feed)' -- \
  ':(exclude)scripts/check-active-tree-identity.sh' \
  ':(exclude)scripts/check-release-archive.sh' \
  ':(exclude)scripts/test-release-contracts.sh'; then
  fail "active source references a removed implementation package"
fi

if git grep -n -i -E \
  'temporary migration residue|remain(s)? only until|bundle(s|d)? a version-pinned rondo|install .*tmux|high-quality coordination surface' -- \
  README.md CONTEXT.md CONTRIBUTING.md SECURITY.md NOTICE.md .nopal .beislid .github package.json Cargo.toml crates extensions scripts \
  ':(exclude)scripts/check-active-tree-identity.sh'; then
  fail "active product language still describes the superseded product or transition state"
fi

for required in \
  extensions/policy-gate/index.ts \
  resources/beislid/provenance.json \
  resources/beislid/LICENSE \
  resources/beislid/skills/kickoff/SKILL.md \
  scripts/install-release.sh \
  scripts/package-release.sh \
  scripts/check-release-archive.sh; do
  [[ -f "$required" ]] || fail "required v0.3 distribution member is missing from source: $required"
done

if ! grep -Eq 'plain Pi session.*no Nopal enforcement guarantee' README.md; then
  fail "README does not state the plain Pi assurance boundary"
fi
if ! grep -q 'rollback' README.md || ! grep -q 'offline' README.md; then
  fail "README does not document rollback and offline launch"
fi
if ! grep -Eq 'nopal\.migration/v1' crates/nopal-cli/src/main.rs; then
  fail "removed command migration diagnostics are absent"
fi
if ! grep -q 'product_surface_removed' crates/nopal-core/src/diagnostics.rs; then
  fail "removed configuration migration diagnostics are absent"
fi

if (( failed != 0 )); then
  exit 1
fi

printf 'active-tree identity check passed\n'
