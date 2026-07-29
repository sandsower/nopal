#!/bin/sh

set -eu

repo_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
. "$repo_root/scripts/release-version.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/nopal-release-contracts.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

fail() {
  echo "release contract failed: $*" >&2
  exit 1
}

version=$(release_workspace_version "$repo_root/Cargo.toml")
tag="v$version"
[ "$(release_next_version 0.2.16)" = 0.3.0 ] \
  || fail "the final v0.2 product line does not advance to v0.3.0"
[ "$(release_next_version 0.3.9)" = 0.3.10 ] \
  || fail "v0.3 patch releases do not increment normally"
[ "$(release_next_version 4.19.99)" = 4.19.100 ] \
  || fail "ordinary release patch arithmetic regressed"
if release_next_version 1.2 >"$tmp/invalid.stdout" 2>"$tmp/invalid.stderr"; then
  fail "invalid workspace version unexpectedly produced a release version"
fi
grep -q 'workspace version must be numeric major.minor.patch' "$tmp/invalid.stderr" \
  || fail "invalid version did not explain the required format"

bump="$tmp/bump"
mkdir -p "$bump/.nopal"
printf '%s\n' '[workspace]' '' '[workspace.package]' 'version = "0.2.44"' > "$bump/Cargo.toml"
printf '%s\n' '{' '  "requirement": "=0.2.44"' '}' > "$bump/.nopal/bundle.jsonc"
[ "$(release_bump_project_versions "$bump/Cargo.toml" "$bump/.nopal/bundle.jsonc")" = 0.3.0 ] \
  || fail "project bump did not cross the v0.3 boundary"
grep -Fqx 'version = "0.3.0"' "$bump/Cargo.toml" || fail "manifest was not bumped"
grep -Fq '"requirement": "=0.3.0"' "$bump/.nopal/bundle.jsonc" \
  || fail "builtin requirement was not bumped with the manifest"

workflow="$repo_root/.github/workflows/release.yml"
for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
  grep -Fq "target: $target" "$workflow" || fail "release workflow omitted $target"
done
if grep -Eiq 'rondo|field-native|desktop-spike|tmux' "$workflow"; then
  fail "release workflow still builds a removed product component"
fi
grep -Fq '@earendil-works/pi-coding-agent@0.80.6' "$workflow" \
  || fail "release workflow does not install exact Pi 0.80.6"
grep -Fq 'node-v22.22.0' "$workflow" \
  || fail "release workflow does not fetch official Node 22.22.0"
grep -Fq 'scripts/package-release.sh' "$workflow" \
  || fail "release workflow does not use the deterministic packager"
[ "$(grep -Fc 'ref: ${{ inputs.commit }}' "$workflow")" -eq 3 ] \
  || fail "every release job must check out the immutable input commit"
[ "$(grep -Fc 'git rev-parse "$RELEASE_TAG^{commit}"' "$workflow")" -eq 3 ] \
  || fail "every release job must bind the tag to the immutable input commit"
if grep -q '^  push:' "$workflow" || grep -Fq 'ref: ${{ env.RELEASE_TAG }}' "$workflow"; then
  fail "release workflow permits mutable tag-triggered script execution"
fi

fake_binary="$tmp/nopal"
fake_node="$tmp/node"
fake_node_license="$tmp/node-LICENSE"
fake_licenses="$tmp/THIRD_PARTY_LICENSES.html"
fake_pi="$tmp/pi"
mkdir -p "$fake_pi/dist" "$fake_pi/node_modules/example"
source_commit=$(git -C "$repo_root" rev-parse HEAD)
cat > "$fake_binary" <<EOF
#!/bin/sh
if [ "\${1:-}" = info ] && [ "\${2:-}" = --json ]; then
  printf '%s\\n' '{"kind":"nopal.info/v1","ok":true,"version":"$version","commit":"$source_commit"}'
else
  printf 'fake nopal $version\\n'
fi
EOF
node_program=$(command -v node)
printf '#!/bin/sh\nexec "%s" "$@"\n' "$node_program" > "$fake_node"
printf '#!/usr/bin/env node\nconsole.log("fake pi")\n' > "$fake_pi/dist/cli.js"
printf 'dependency bytes\n' > "$fake_pi/node_modules/example/index.js"
printf '%s\n' '{' \
  '  "name": "@earendil-works/pi-coding-agent",' \
  '  "version": "0.80.6",' \
  '  "bin": { "pi": "dist/cli.js" }' \
  '}' > "$fake_pi/package.json"
ln -s ../dist/cli.js "$fake_pi/node_modules/pi-entry"
printf 'Node license\n' > "$fake_node_license"
printf '<html><body>dependency licenses</body></html>\n' > "$fake_licenses"
chmod 0755 "$fake_binary" "$fake_node" "$fake_pi/dist/cli.js"
node_sha=$(sha256sum "$fake_node" 2>/dev/null | cut -d ' ' -f1 || shasum -a 256 "$fake_node" | cut -d ' ' -f1)
pi_integrity=$(python3 "$repo_root/scripts/hash-runtime-tree.py" "$fake_pi")

export NOPAL_RELEASE_BINARY="$fake_binary"
export NOPAL_RELEASE_NODE="$fake_node"
export NOPAL_RELEASE_NODE_LICENSE="$fake_node_license"
export NOPAL_RELEASE_PI_ROOT="$fake_pi"
export NOPAL_RELEASE_THIRD_PARTY_LICENSES="$fake_licenses"
export NOPAL_RELEASE_TEST_NODE_SHA256="$node_sha"
export NOPAL_RELEASE_TEST_PI_INTEGRITY="$pi_integrity"

runtime_mismatch_tree="$tmp/runtime-mismatch-tree"
mkdir "$runtime_mismatch_tree"
printf 'native closure\n' > "$runtime_mismatch_tree/member"
wrong_runtime_integrity=sha256:0000000000000000000000000000000000000000000000000000000000000000
if "$repo_root/scripts/verify-runtime-integrity.sh" \
  "test runtime" "$runtime_mismatch_tree" "$wrong_runtime_integrity" \
  >"$tmp/runtime-mismatch.out" 2>"$tmp/runtime-mismatch.err"; then
  fail "runtime verifier accepted an incorrect complete-tree identity"
fi
actual_runtime_integrity=$(python3 "$repo_root/scripts/hash-runtime-tree.py" \
  "$runtime_mismatch_tree")
grep -q "test runtime has integrity $actual_runtime_integrity" \
  "$tmp/runtime-mismatch.err" \
  || fail "runtime verifier did not report actual complete-tree identity"
grep -q "expected $wrong_runtime_integrity" "$tmp/runtime-mismatch.err" \
  || fail "runtime verifier did not report expected complete-tree identity"

linux_pi_integrity=sha256:1b7f4f85e0f36eafd10f3db15a6d4ba58087cf10e96ec3cee2e0d5bd00c5e2c1
for release_contract in \
  "$repo_root/crates/nopal-cli/src/main.rs" \
  "$repo_root/scripts/package-release.sh" \
  "$repo_root/.github/workflows/release.yml"; do
  grep -q "$linux_pi_integrity" "$release_contract" \
    || fail "Linux Pi closure identity is not pinned in $release_contract"
done

source_fixture="$tmp/source-fixture"
mkdir "$source_fixture"
git -c init.templateDir= -c init.defaultBranch=main -C "$source_fixture" init -q
git -C "$source_fixture" config user.name "Release Contract"
git -C "$source_fixture" config user.email "release-contract@nopal.invalid"
printf '[workspace]\n\n[workspace.package]\nversion = "%s"\n' "$version" \
  > "$source_fixture/Cargo.toml"
printf 'committed\n' > "$source_fixture/tracked.txt"
git -C "$source_fixture" add Cargo.toml tracked.txt
git -C "$source_fixture" commit -q -m source
git -C "$source_fixture" tag "$tag"
printf 'dirty\n' > "$source_fixture/tracked.txt"
if NOPAL_RELEASE_SOURCE_ROOT="$source_fixture" sh "$repo_root/scripts/package-release.sh" \
  aarch64-apple-darwin "$tag" "$tmp/dirty-output" \
  >"$tmp/dirty-source.out" 2>"$tmp/dirty-source.err"; then
  fail "production packaging accepted a dirty tagged source tree"
fi
grep -q 'release source must be a clean tagged worktree' "$tmp/dirty-source.err" \
  || fail "dirty release source rejection was not explained"
git -C "$source_fixture" checkout -q -- tracked.txt
printf 'next commit\n' > "$source_fixture/tracked.txt"
git -C "$source_fixture" add tracked.txt
git -C "$source_fixture" commit -q -m next
if NOPAL_RELEASE_SOURCE_ROOT="$source_fixture" sh "$repo_root/scripts/package-release.sh" \
  aarch64-apple-darwin "$tag" "$tmp/mismatched-tag-output" \
  >"$tmp/mismatched-tag.out" 2>"$tmp/mismatched-tag.err"; then
  fail "production packaging accepted a tag that did not name source HEAD"
fi
grep -q 'expected source HEAD' "$tmp/mismatched-tag.err" \
  || fail "tag/source mismatch rejection was not explained"

for parent in mode-a mode-b; do
  mkdir -p "$tmp/$parent/root/nested"
  printf 'regular\n' > "$tmp/$parent/root/nested/regular"
  printf '#!/bin/sh\nexit 0\n' > "$tmp/$parent/root/executable"
  chmod 0755 "$tmp/$parent/root/executable"
  ln -s nested/regular "$tmp/$parent/root/link"
done
chmod 0700 "$tmp/mode-a/root" "$tmp/mode-a/root/nested"
chmod 0600 "$tmp/mode-a/root/nested/regular"
chmod 4711 "$tmp/mode-a/root/executable"
chmod 0755 "$tmp/mode-b/root" "$tmp/mode-b/root/nested"
chmod 0644 "$tmp/mode-b/root/nested/regular"
chmod 0755 "$tmp/mode-b/root/executable"
python3 "$repo_root/scripts/create-release-archive.py" \
  "$tmp/mode-a/root" "$tmp/mode-a.tar.gz" >/dev/null
python3 "$repo_root/scripts/create-release-archive.py" \
  "$tmp/mode-b/root" "$tmp/mode-b.tar.gz" >/dev/null
cmp "$tmp/mode-a.tar.gz" "$tmp/mode-b.tar.gz" \
  || fail "identity-equivalent input permissions changed archive bytes"

target=test-release-target
stem="nopal-$tag-$target"
archive=$(sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$tmp/dist-one")
second=$(sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$tmp/dist-two")
cmp "$archive" "$second" || fail "identical inputs did not produce identical archives"

contents="$tmp/contents"
tar -tzf "$archive" | sed 's:/$::' | LC_ALL=C sort > "$contents"
for required in \
  "$stem/nopal" \
  "$stem/runtime/node" \
  "$stem/runtime/pi/package.json" \
  "$stem/runtime/pi/dist/cli.js" \
  "$stem/share/nopal/extensions/policy-gate/index.ts" \
  "$stem/share/nopal/resources/beislid/provenance.json" \
  "$stem/share/nopal/resources/beislid/skills/kickoff/SKILL.md" \
  "$stem/distribution.json" \
  "$stem/install"; do
  grep -Fqx "$required" "$contents" || fail "archive omitted $required"
done
if grep -Eiq '(^|/)(rondo|field|cockpit|desktop|memento|herdr|tmux)(/|$)|nopal-field-native' "$contents"; then
  fail "archive contains a removed product member"
fi

unpacked="$tmp/unpacked"
mkdir "$unpacked"
tar -xzf "$archive" -C "$unpacked"
root="$unpacked/$stem"
cmp "$fake_binary" "$root/nopal" || fail "Nopal binary bytes changed"
cmp "$fake_node" "$root/runtime/node" || fail "Node bytes changed"
cmp "$fake_pi/dist/cli.js" "$root/runtime/pi/dist/cli.js" || fail "Pi bytes changed"
cmp "$repo_root/extensions/policy-gate/guard.ts" \
  "$root/share/nopal/extensions/policy-gate/guard.ts" || fail "adapter bytes changed"
cmp "$repo_root/resources/beislid/provenance.json" \
  "$root/share/nopal/resources/beislid/provenance.json" || fail "Beislið provenance changed"
python3 - "$root/distribution.json" "$version" "$node_sha" "$pi_integrity" \
  "$tag" "$source_commit" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1]))
assert doc["kind"] == "nopal.release_distribution/v1"
assert doc["version"] == sys.argv[2]
assert doc["node"]["executable_integrity"] == "sha256:" + sys.argv[3]
assert doc["pi"]["runtime_integrity"] == sys.argv[4]
assert doc["source"] == {"tag":sys.argv[5],"commit":sys.argv[6]}
assert doc["nopal"]["binary_integrity"].startswith("sha256:")
assert doc["policy_adapter"]["integrity"].startswith("sha256:")
assert doc["beislid"]["version"] == "0.4.16"
assert doc["beislid"]["tree_integrity"].startswith("sha256:")
PY
[ -x "$root/nopal" ] || fail "packaged Nopal is not executable"
[ -x "$root/runtime/node" ] || fail "packaged Node is not executable"
[ -x "$root/install" ] || fail "packaged installer is not executable"

prefix="$tmp/prefix"
"$root/install" install "$prefix" > "$tmp/install-one.out"
installed_one=$(readlink "$prefix/lib/nopal/current")
[ -x "$prefix/bin/nopal" ] || fail "installer did not create the launcher"
[ "$("$prefix/bin/nopal")" = "fake nopal $version" ] || fail "installed launcher selected wrong bytes"

second_root="$tmp/second-root"
cp -R "$root" "$second_root"
python3 - "$second_root/distribution.json" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1]))
doc["version"]="9.9.9"
open(sys.argv[1],"w").write(json.dumps(doc,indent=2)+"\n")
PY
printf '#!/bin/sh\nprintf "fake nopal 9.9.9\\n"\n' > "$second_root/nopal"
chmod 0755 "$second_root/nopal"
"$second_root/install" install "$prefix" > "$tmp/install-two.out"
[ "$("$prefix/bin/nopal")" = "fake nopal 9.9.9" ] || fail "second install did not become current"
[ "$(readlink "$prefix/lib/nopal/previous")" = "$installed_one" ] \
  || fail "second install did not retain the previous release"
"$second_root/install" rollback "$prefix" > "$tmp/rollback.out"
[ "$("$prefix/bin/nopal")" = "fake nopal $version" ] || fail "rollback did not restore prior bytes"

blocked_prefix="$tmp/blocked-prefix"
"$root/install" install "$blocked_prefix" >/dev/null
blocked_current=$(readlink "$blocked_prefix/lib/nopal/current")
rm "$blocked_prefix/bin/nopal"
printf 'owner-managed launcher\n' > "$blocked_prefix/bin/nopal"
if "$second_root/install" install "$blocked_prefix" >"$tmp/blocked.out" 2>"$tmp/blocked.err"; then
  fail "installer replaced an owner-managed launcher"
fi
[ "$(readlink "$blocked_prefix/lib/nopal/current")" = "$blocked_current" ] \
  || fail "failed install changed the current release"
[ ! -e "$blocked_prefix/lib/nopal/9.9.9-$target" ] \
  || fail "failed install copied release bytes before launcher preflight"

conflict_prefix="$tmp/conflict-prefix"
"$root/install" install "$conflict_prefix" >/dev/null
conflict_release="$conflict_prefix/lib/nopal/$version-$target"
printf 'tampered Pi bytes\n' > "$conflict_release/runtime/pi/dist/cli.js"
if "$root/install" install "$conflict_prefix" >"$tmp/reinstall.out" 2>"$tmp/reinstall.err"; then
  fail "reinstallation accepted conflicting Pi runtime bytes"
fi
grep -q 'installed release identity conflicts' "$tmp/reinstall.err" \
  || fail "full-tree reinstall conflict was not explained"

for privileged_mode in 4755 2755 1755; do
  mode_prefix="$tmp/mode-prefix-$privileged_mode"
  "$root/install" install "$mode_prefix" >/dev/null
  mode_release="$mode_prefix/lib/nopal/$version-$target"
  chmod "$privileged_mode" "$mode_release/nopal"
  if "$root/install" install "$mode_prefix" \
    >"$tmp/mode-reinstall-$privileged_mode.out" \
    2>"$tmp/mode-reinstall-$privileged_mode.err"; then
    fail "reinstallation accepted privileged mode $privileged_mode"
  fi
  grep -q 'installed release identity conflicts' \
    "$tmp/mode-reinstall-$privileged_mode.err" \
    || fail "privileged mode $privileged_mode conflict was not explained"
done

saved="$tmp/saved.tar.gz"
cp "$archive" "$saved"
if NOPAL_RELEASE_NODE="$tmp/missing-node" sh "$repo_root/scripts/package-release.sh" \
  "$target" "$tag" "$tmp/dist-one" >"$tmp/failure.out" 2>"$tmp/failure.err"; then
  fail "missing Node unexpectedly packaged"
fi
cmp "$archive" "$saved" || fail "failed packaging replaced a complete archive"

mock_state="$tmp/mock-gh"
mock_dist="$tmp/mock-dist"
mkdir -p "$mock_state" "$mock_dist"
printf 'archive bytes\n' > "$mock_dist/$stem.tar.gz"
printf 'checksum bytes\n' > "$mock_dist/SHA256SUMS"
export MOCK_GH_STATE="$mock_state"
PATH="$repo_root/scripts/test-fixtures:$PATH"
export PATH
sh "$repo_root/scripts/publish-release.sh" "$tag" "$mock_dist"
[ "$(cat "$mock_state/release-state")" = published ] || fail "release stayed draft"
: > "$mock_state/gh.log"
sh "$repo_root/scripts/publish-release.sh" "$tag" "$mock_dist"
if grep -Eq '^release (create|upload|edit) ' "$mock_state/gh.log"; then
  fail "identical publication rerun mutated the remote release"
fi
printf 'conflicting bytes\n' > "$mock_state/remote-assets/$stem.tar.gz"
if sh "$repo_root/scripts/publish-release.sh" "$tag" "$mock_dist" \
  >"$tmp/conflict.out" 2>"$tmp/conflict.err"; then
  fail "conflicting remote asset unexpectedly succeeded"
fi
grep -q 'conflicts with the local file' "$tmp/conflict.err" \
  || fail "asset conflict was not explained"

echo "release packaging, installation, rollback, and publication contracts passed"
