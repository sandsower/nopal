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

[ "$(release_next_patch_version 0.2.9)" = "0.2.10" ] \
  || fail "patch version did not increment across a digit boundary"
[ "$(release_next_patch_version 4.19.99)" = "4.19.100" ] \
  || fail "patch version did not preserve major and minor components"

bump_fixture="$tmp/version-bump-fixture"
mkdir -p "$bump_fixture/.nopal"
printf '%s\n' \
  '[workspace]' \
  '' \
  '[workspace.package]' \
  'version = "4.19.99"' \
  > "$bump_fixture/Cargo.toml"
printf '%s\n' \
  '{' \
  '  "version": "nopal.bundle/v2",' \
  '  "packages": [{' \
  '    "id": "nopal",' \
  '    "source": { "type": "builtin", "package": "nopal" },' \
  '    "requirement": "=4.19.99",' \
  '    "resources": [{ "kind": "extension", "path": "index.ts" }]' \
  '  }]' \
  '}' \
  > "$bump_fixture/.nopal/bundle.jsonc"
real_manifest_before=$(sha256sum "$repo_root/Cargo.toml" | cut -d ' ' -f1)
real_bundle_before=$(sha256sum "$repo_root/.nopal/bundle.jsonc" | cut -d ' ' -f1)
fixture_next=$(release_bump_project_versions \
  "$bump_fixture/Cargo.toml" "$bump_fixture/.nopal/bundle.jsonc")
[ "$fixture_next" = 4.19.100 ] \
  || fail "fixture bump returned unexpected version $fixture_next"
grep -Fqx 'version = "4.19.100"' "$bump_fixture/Cargo.toml" \
  || fail "fixture workspace version was not bumped"
grep -Fq '"requirement": "=4.19.100"' "$bump_fixture/.nopal/bundle.jsonc" \
  || fail "fixture builtin requirement was not bumped exactly"
[ "$(sha256sum "$repo_root/Cargo.toml" | cut -d ' ' -f1)" = "$real_manifest_before" ] \
  || fail "fixture bump mutated the real workspace version"
[ "$(sha256sum "$repo_root/.nopal/bundle.jsonc" | cut -d ' ' -f1)" = "$real_bundle_before" ] \
  || fail "fixture bump mutated the real distribution contract"

if release_next_patch_version 1.2 >"$tmp/invalid-version.stdout" \
  2>"$tmp/invalid-version.stderr"; then
  fail "invalid workspace version unexpectedly produced a patch version"
fi
grep -q 'workspace version must be numeric major.minor.patch' \
  "$tmp/invalid-version.stderr" \
  || fail "invalid workspace version did not explain the required format"

release_pr_fixture='[
  {"number": 7, "head": {"ref": "feature/other"}, "base": {"ref": "main"}, "merged_at": "2026-07-14T00:00:00Z"},
  {"number": 8, "head": {"ref": "automation/version-bump"}, "base": {"ref": "main"}, "merged_at": null},
  {"number": 9, "head": {"ref": "automation/version-bump"}, "base": {"ref": "main"}, "merged_at": "2026-07-14T00:01:00Z"}
]'
release_pr=$(
  printf '%s\n' "$release_pr_fixture" \
    | RELEASE_BRANCH=automation/version-bump \
      jq -r -f "$repo_root/scripts/release-pr.jq"
)
[ "$release_pr" = 9 ] \
  || fail "release classifier did not select the merged automation pull request"
release_pr=$(
  printf '%s\n' "$release_pr_fixture" \
    | RELEASE_BRANCH=automation/other \
      jq -r -f "$repo_root/scripts/release-pr.jq"
)
[ -z "$release_pr" ] \
  || fail "release classifier accepted an unrelated pull request"

version_workflow="$repo_root/.github/workflows/version-bump.yml"
grep -Fq 'cancel-in-progress: false' "$version_workflow" \
  || fail "release coordination can cancel an in-flight artifact build"
grep -Fq 'base_sha=$GITHUB_SHA' "$version_workflow" \
  || fail "release classification is not anchored to the immutable push commit"
grep -Fq 'git switch --force-create "$RELEASE_BRANCH" origin/main' \
  "$version_workflow" \
  || fail "the next version branch is not refreshed from current main"
grep -Fq 'release_bump_project_versions Cargo.toml .nopal/bundle.jsonc' \
  "$version_workflow" \
  || fail "automated bumps do not update the workspace and builtin requirement together"
grep -Fq 'update --write' "$version_workflow" \
  || fail "automated bumps do not regenerate the exact distribution lock"
grep -Fq 'cmp -s .nopal/nopal.lock' "$version_workflow" \
  || fail "automated bumps do not verify deterministic lock regeneration"
grep -Fq '.resolved == $version' "$version_workflow" \
  || fail "automated bumps do not verify the resolved builtin version"
grep -Fq -- '--dry-run --json' "$version_workflow" \
  || fail "automated bumps do not validate the cold launch plan"
grep -Fq 'git add Cargo.toml Cargo.lock .nopal/bundle.jsonc .nopal/nopal.lock' \
  "$version_workflow" \
  || fail "automated bumps do not stage the complete version and distribution contract"
grep -Fq 'gh workflow run ci.yml --ref "$RELEASE_BRANCH"' "$version_workflow" \
  || fail "automated version pull requests do not dispatch CI explicitly"
grep -Fq 'gh auth setup-git' "$version_workflow" \
  || fail "credential-free checkout is not reauthenticated before protected writes"
grep -Fq 'statuses: write' "$version_workflow" \
  || fail "release coordinator cannot publish the verified commit status"
grep -Fq 'gh run watch "$ci_id" --exit-status' "$version_workflow" \
  || fail "release coordinator does not wait for exact dispatched CI"
grep -Fq 'repos/${GITHUB_REPOSITORY}/statuses/${RELEASE_COMMIT}' \
  "$version_workflow" \
  || fail "release coordinator does not publish status on its exact commit"
grep -Fq 'conclusion == "action_required"' "$version_workflow" \
  || fail "release coordinator does not identify the inert bot PR run"
grep -Fq 'gh run delete "$run_id"' "$version_workflow" \
  || fail "release coordinator does not remove the exact inert bot PR run"
grep -Fq -- '--match-head-commit "$RELEASE_COMMIT"' "$version_workflow" \
  || fail "automated version pull requests can auto-merge a stale head"
grep -Fq 'release_commit=$(printf' "$version_workflow" \
  || fail "release coordinator does not capture the protected merge commit"
grep -Fq 'echo "release_commit=$release_commit" >> "$GITHUB_OUTPUT"' \
  "$version_workflow" \
  || fail "release coordinator does not expose the protected merge commit"
grep -Fq 'bump) commit=$MERGED_RELEASE_COMMIT ;;' "$version_workflow" \
  || fail "release tagging does not continue from the protected merge"
grep -Fq "if: needs.coordinate.outputs.tag != ''" "$version_workflow" \
  || fail "artifact publication does not continue in the coordinating workflow"
if grep -Fq 'git push origin main' "$version_workflow"; then
  fail "version workflow still pushes directly to main"
fi

target=test-release-target
stem="nopal-$tag-$target"
fake_binary="$tmp/nopal"
fake_rondo="$tmp/rondo"
fake_third_party_licenses="$tmp/THIRD_PARTY_LICENSES.html"
printf 'fake nopal binary\n' > "$fake_binary"
printf '#!/bin/sh\nprintf "fake rondo runtime\\n"\n' > "$fake_rondo"
printf '<html><body>fake dependency licenses</body></html>\n' > "$fake_third_party_licenses"
chmod 0755 "$fake_rondo"

app_fixture="$tmp/app-source"
app_target="$tmp/app-target"
app_tools="$tmp/app-tools"
mkdir -p "$app_fixture/extensions/policy-gate" "$app_target/release" "$app_tools"
cp "$repo_root/Makefile" "$repo_root/Cargo.toml" "$app_fixture/"
for adapter_file in index.ts classifier.ts nopal-cli.ts; do
  cp "$repo_root/extensions/policy-gate/$adapter_file" \
    "$app_fixture/extensions/policy-gate/$adapter_file"
done
cp "$fake_binary" "$app_target/release/nopal"
printf 'fake native app binary\n' > "$app_target/release/nopal-field-native"
chmod 0755 "$app_target/release/nopal" "$app_target/release/nopal-field-native"
printf '#!/bin/sh\nexit 0\n' > "$app_tools/cargo"
printf '#!/bin/sh\nexit 0\n' > "$app_tools/plutil"
chmod 0755 "$app_tools/cargo" "$app_tools/plutil"

PATH="$app_tools:$PATH" make -s -C "$app_fixture" macos \
  HOST_OS=Darwin CARGO_TARGET_DIR="$app_target" \
  >"$tmp/macos-package.stdout"
macos_adapter="$app_fixture/Nopal.app/Contents/Resources/extensions/policy-gate"
[ -d "$macos_adapter" ] \
  || fail "macOS app does not contain the source-free built-in adapter layout"
find "$macos_adapter" -type f -exec basename {} \; | LC_ALL=C sort \
  > "$tmp/macos-adapter-files"
printf '%s\n' classifier.ts index.ts nopal-cli.ts \
  > "$tmp/expected-adapter-files"
cmp "$tmp/expected-adapter-files" "$tmp/macos-adapter-files" \
  || fail "macOS app does not contain exactly the three built-in adapter files"
for adapter_file in index.ts classifier.ts nopal-cli.ts; do
  cmp "$repo_root/extensions/policy-gate/$adapter_file" \
    "$macos_adapter/$adapter_file" \
    || fail "macOS app adapter $adapter_file bytes changed"
done

PATH="$app_tools:$PATH" make -s -C "$app_fixture" linux \
  HOST_OS=Linux CARGO_TARGET_DIR="$app_target" \
  >"$tmp/linux-package.stdout"
linux_adapter="$app_fixture/Nopal-linux/extensions/policy-gate"
[ -d "$linux_adapter" ] \
  || fail "Linux app does not contain the source-free built-in adapter layout"
find "$linux_adapter" -type f -exec basename {} \; | LC_ALL=C sort \
  > "$tmp/linux-adapter-files"
cmp "$tmp/expected-adapter-files" "$tmp/linux-adapter-files" \
  || fail "Linux app does not contain exactly the three built-in adapter files"
for adapter_file in index.ts classifier.ts nopal-cli.ts; do
  cmp "$repo_root/extensions/policy-gate/$adapter_file" \
    "$linux_adapter/$adapter_file" \
    || fail "Linux app adapter $adapter_file bytes changed"
done

rondo_commit=$(sed -n 's/.*"source_commit": "\([0-9a-f]*\)".*/\1/p' "$repo_root/rondo-runtime.json")
[ "${#rondo_commit}" -eq 40 ] || fail "Rondo provenance source commit is not a full SHA"
grep -q "ref: $rondo_commit" "$repo_root/.github/workflows/release.yml" \
  || fail "release workflow Rondo checkout drifted from provenance"

fake_rondo_source="$tmp/rondo-source"
mkdir -p "$fake_rondo_source"
cp "$fake_rondo" "$fake_rondo_source/rondo"
printf 'fake Rondo license\n' > "$fake_rondo_source/LICENSE"
printf 'fake Rondo notice\n' > "$fake_rondo_source/NOTICE"
git -C "$fake_rondo_source" init -q
git -C "$fake_rondo_source" add rondo LICENSE NOTICE
git -C "$fake_rondo_source" \
  -c user.name='Release Contract' -c user.email='release-contract@example.test' \
  commit -q -m seed
fake_rondo_commit=$(git -C "$fake_rondo_source" rev-parse HEAD)
fake_provenance="$tmp/rondo-runtime.json"
printf '{"schema":"nopal.rondo_runtime_provenance/v1","runtime_version":"0.1.0","source_repository":"https://example.test/rondo","source_commit":"%s","artifact_format":"escript","erlang_major":28}\n' \
  "$fake_rondo_commit" > "$fake_provenance"
export NOPAL_RELEASE_RONDO_SOURCE="$fake_rondo_source"
export NOPAL_RELEASE_RONDO_PROVENANCE="$fake_provenance"
export NOPAL_RELEASE_THIRD_PARTY_LICENSES="$fake_third_party_licenses"

package_dir="$tmp/package"
archive=$(
  NOPAL_RELEASE_BINARY="$fake_binary" \
    NOPAL_RELEASE_RONDO_RUNTIME="$fake_rondo" \
    sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$package_dir"
)
expected_archive="$package_dir/$stem.tar.gz"
[ "$archive" = "$expected_archive" ] || fail "unexpected archive name: $archive"
[ -f "$expected_archive" ] || fail "archive was not created"

sleep 1
second_package_dir="$tmp/package-second"
second_archive=$(
  NOPAL_RELEASE_BINARY="$fake_binary" \
    NOPAL_RELEASE_RONDO_RUNTIME="$fake_rondo" \
    sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$second_package_dir"
)
cmp "$archive" "$second_archive" \
  || fail "identical release inputs did not produce byte-identical archives"

saved_archive="$tmp/saved-archive.tar.gz"
cp "$archive" "$saved_archive"
failing_tools="$tmp/failing-tools"
mkdir -p "$failing_tools"
printf '%s\n' \
  '#!/bin/sh' \
  'printf "partial gzip output\n"' \
  'exit 1' \
  > "$failing_tools/gzip"
chmod 0755 "$failing_tools/gzip"
if PATH="$failing_tools:$PATH" NOPAL_RELEASE_BINARY="$fake_binary" \
  NOPAL_RELEASE_RONDO_RUNTIME="$fake_rondo" \
  sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$package_dir" \
  >"$tmp/gzip-failure.stdout" 2>"$tmp/gzip-failure.stderr"; then
  fail "failing gzip unexpectedly succeeded"
fi
cmp "$saved_archive" "$archive" \
  || fail "failed packaging replaced the previous complete archive"
if find "$package_dir" -name ".$stem.tar.gz.*" -print | grep -q .; then
  fail "failed packaging left an archive temporary file"
fi

actual_contents="$tmp/archive-contents"
tar -tzf "$archive" | sed 's:/$::' | LC_ALL=C sort > "$actual_contents"
expected_contents="$tmp/expected-contents"
printf '%s\n' \
  "$stem" \
  "$stem/LICENSE" \
  "$stem/NOTICE.md" \
  "$stem/README.md" \
  "$stem/Rondo-LICENSE" \
  "$stem/Rondo-NOTICE" \
  "$stem/THIRD_PARTY_LICENSES.html" \
  "$stem/extensions" \
  "$stem/extensions/policy-gate" \
  "$stem/extensions/policy-gate/classifier.ts" \
  "$stem/extensions/policy-gate/index.ts" \
  "$stem/extensions/policy-gate/nopal-cli.ts" \
  "$stem/nopal" \
  "$stem/rondo" \
  "$stem/rondo-runtime.json" \
  | LC_ALL=C sort > "$expected_contents"
cmp "$expected_contents" "$actual_contents" || fail "archive contents differ"

unpacked="$tmp/unpacked"
mkdir -p "$unpacked"
tar -xzf "$archive" -C "$unpacked"
cmp "$fake_binary" "$unpacked/$stem/nopal" || fail "binary bytes changed"
cmp "$fake_rondo" "$unpacked/$stem/rondo" || fail "Rondo runtime bytes changed"
cmp "$fake_provenance" "$unpacked/$stem/rondo-runtime.json" \
  || fail "Rondo provenance bytes changed"
cmp "$repo_root/LICENSE" "$unpacked/$stem/LICENSE" || fail "LICENSE bytes changed"
cmp "$repo_root/README.md" "$unpacked/$stem/README.md" || fail "README bytes changed"
cmp "$repo_root/NOTICE.md" "$unpacked/$stem/NOTICE.md" || fail "NOTICE bytes changed"
cmp "$fake_rondo_source/LICENSE" "$unpacked/$stem/Rondo-LICENSE" \
  || fail "Rondo LICENSE bytes changed"
cmp "$fake_rondo_source/NOTICE" "$unpacked/$stem/Rondo-NOTICE" \
  || fail "Rondo NOTICE bytes changed"
cmp "$fake_third_party_licenses" "$unpacked/$stem/THIRD_PARTY_LICENSES.html" \
  || fail "third-party license report bytes changed"
for adapter_file in index.ts classifier.ts nopal-cli.ts; do
  cmp "$repo_root/extensions/policy-gate/$adapter_file" \
    "$unpacked/$stem/extensions/policy-gate/$adapter_file" \
    || fail "enforcement adapter $adapter_file bytes changed"
done
[ -x "$unpacked/$stem/nopal" ] || fail "packaged binary is not executable"
[ -x "$unpacked/$stem/rondo" ] || fail "packaged Rondo runtime is not executable"
[ ! -x "$unpacked/$stem/rondo-runtime.json" ] \
  || fail "packaged Rondo provenance is unexpectedly executable"
[ ! -x "$unpacked/$stem/LICENSE" ] || fail "packaged LICENSE is unexpectedly executable"
[ ! -x "$unpacked/$stem/README.md" ] || fail "packaged README is unexpectedly executable"
[ ! -x "$unpacked/$stem/NOTICE.md" ] || fail "packaged NOTICE is unexpectedly executable"
[ ! -x "$unpacked/$stem/Rondo-LICENSE" ] \
  || fail "packaged Rondo LICENSE is unexpectedly executable"
[ ! -x "$unpacked/$stem/Rondo-NOTICE" ] \
  || fail "packaged Rondo NOTICE is unexpectedly executable"
[ ! -x "$unpacked/$stem/THIRD_PARTY_LICENSES.html" ] \
  || fail "packaged third-party license report is unexpectedly executable"
for adapter_file in index.ts classifier.ts nopal-cli.ts; do
  [ ! -x "$unpacked/$stem/extensions/policy-gate/$adapter_file" ] \
    || fail "packaged enforcement adapter $adapter_file is unexpectedly executable"
done

if NOPAL_RELEASE_BINARY="$fake_binary" NOPAL_RELEASE_RONDO_RUNTIME="$fake_rondo" \
  sh "$repo_root/scripts/package-release.sh" "$target" v9.9.9 "$tmp/mismatch" \
  >"$tmp/mismatch.stdout" 2>"$tmp/mismatch.stderr"; then
  fail "tag mismatch unexpectedly succeeded"
fi
grep -q "does not match workspace version $version" "$tmp/mismatch.stderr" \
  || fail "tag mismatch did not explain the expected version"

if NOPAL_RELEASE_BINARY="$tmp/missing-nopal" NOPAL_RELEASE_RONDO_RUNTIME="$fake_rondo" \
  sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$tmp/missing" \
  >"$tmp/missing.stdout" 2>"$tmp/missing.stderr"; then
  fail "missing binary unexpectedly succeeded"
fi
grep -q 'release binary not found' "$tmp/missing.stderr" \
  || fail "missing binary did not produce the expected error"

if NOPAL_RELEASE_BINARY="$fake_binary" \
  NOPAL_RELEASE_RONDO_RUNTIME="$tmp/missing-rondo" \
  sh "$repo_root/scripts/package-release.sh" "$target" "$tag" "$tmp/missing-rondo-package" \
  >"$tmp/missing-rondo.stdout" 2>"$tmp/missing-rondo.stderr"; then
  fail "missing Rondo runtime unexpectedly succeeded"
fi
grep -q 'release Rondo runtime not found or not executable' "$tmp/missing-rondo.stderr" \
  || fail "missing Rondo runtime did not produce the expected error"

mock_state="$tmp/mock-gh"
dist_dir="$tmp/dist"
mkdir -p "$mock_state" "$dist_dir"
printf 'archive bytes\n' > "$dist_dir/$stem.tar.gz"
printf 'checksum bytes\n' > "$dist_dir/SHA256SUMS"
export MOCK_GH_STATE="$mock_state"
PATH="$repo_root/scripts/test-fixtures:$PATH"
export PATH

sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir"
[ "$(cat "$mock_state/release-state")" = published ] || fail "new release stayed draft"
cmp "$dist_dir/$stem.tar.gz" "$mock_state/remote-assets/$stem.tar.gz" \
  || fail "new archive asset was not uploaded"
cmp "$dist_dir/SHA256SUMS" "$mock_state/remote-assets/SHA256SUMS" \
  || fail "new checksum asset was not uploaded"
grep -q '^release create ' "$mock_state/gh.log" || fail "new release was not created"
grep -q '^release edit ' "$mock_state/gh.log" || fail "new release was not published"

: > "$mock_state/gh.log"
sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir"
if grep -Eq '^release (create|upload|edit) ' "$mock_state/gh.log"; then
  fail "identical rerun performed a mutating gh operation"
fi

printf 'draft\n' > "$mock_state/release-state"
rm "$mock_state/remote-assets/SHA256SUMS"
: > "$mock_state/gh.log"
sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir"
[ "$(cat "$mock_state/release-state")" = published ] \
  || fail "partial draft release was not published after convergence"
grep -q "^release upload $tag $dist_dir/SHA256SUMS" "$mock_state/gh.log" \
  || fail "partial draft did not upload its missing asset"
grep -q '^release edit ' "$mock_state/gh.log" \
  || fail "converged draft was not published"

rm "$mock_state/remote-assets/SHA256SUMS"
: > "$mock_state/gh.log"
sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir"
grep -q "^release upload $tag $dist_dir/SHA256SUMS" "$mock_state/gh.log" \
  || fail "partial release did not upload its missing asset"
if grep -Eq '^release (create|edit) ' "$mock_state/gh.log"; then
  fail "partial published release was recreated or republished"
fi

printf 'conflicting remote bytes\n' > "$mock_state/remote-assets/$stem.tar.gz"
rm "$mock_state/remote-assets/SHA256SUMS"
: > "$mock_state/gh.log"
if sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir" \
  >"$tmp/conflict.stdout" 2>"$tmp/conflict.stderr"; then
  fail "conflicting remote asset unexpectedly succeeded"
fi
grep -q 'conflicts with the local file' "$tmp/conflict.stderr" \
  || fail "conflicting asset did not fail with an explanation"
if grep -Eq '^release (create|upload|edit) ' "$mock_state/gh.log"; then
  fail "conflict path performed a mutating gh operation"
fi

create_race_state="$tmp/mock-gh-create-race"
mkdir -p "$create_race_state"
export MOCK_GH_STATE="$create_race_state"
MOCK_GH_CREATE_RACE=1 \
  sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir"
[ "$(cat "$create_race_state/release-state")" = published ] \
  || fail "concurrent release creation did not converge to published"
cmp "$dist_dir/$stem.tar.gz" \
  "$create_race_state/remote-assets/$stem.tar.gz" \
  || fail "concurrent-create path did not upload the archive"
cmp "$dist_dir/SHA256SUMS" "$create_race_state/remote-assets/SHA256SUMS" \
  || fail "concurrent-create path did not upload checksums"
grep -q '^release create ' "$create_race_state/gh.log" \
  || fail "concurrent-create contract did not exercise release creation"

upload_race_state="$tmp/mock-gh-upload-race"
mkdir -p "$upload_race_state"
printf 'draft\n' > "$upload_race_state/release-state"
export MOCK_GH_STATE="$upload_race_state"
MOCK_GH_UPLOAD_RACE_ASSET=SHA256SUMS \
  sh "$repo_root/scripts/publish-release.sh" "$tag" "$dist_dir"
[ "$(cat "$upload_race_state/release-state")" = published ] \
  || fail "concurrent asset upload did not converge to published"
cmp "$dist_dir/$stem.tar.gz" \
  "$upload_race_state/remote-assets/$stem.tar.gz" \
  || fail "concurrent-upload path did not preserve the archive"
cmp "$dist_dir/SHA256SUMS" "$upload_race_state/remote-assets/SHA256SUMS" \
  || fail "concurrent-upload path did not preserve checksums"

echo "release packaging and publication contracts passed"
