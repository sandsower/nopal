#!/bin/sh

set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <target-triple> <vX.Y.Z-tag> [output-dir]" >&2
  exit 2
fi

target=$1
tag=$2
output_dir=${3:-dist}

repo_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
source_root=${NOPAL_RELEASE_SOURCE_ROOT:-$repo_root}
. "$repo_root/scripts/release-version.sh"

release_validate_tag "$repo_root/Cargo.toml" "$tag" >/dev/null

binary=${NOPAL_RELEASE_BINARY:-target/$target/release/nopal}
rondo_runtime=${NOPAL_RELEASE_RONDO_RUNTIME:-_rondo/elixir/bin/rondo}
rondo_provenance=${NOPAL_RELEASE_RONDO_PROVENANCE:-$repo_root/rondo-runtime.json}
rondo_source=${NOPAL_RELEASE_RONDO_SOURCE:-_rondo}
third_party_licenses=${NOPAL_RELEASE_THIRD_PARTY_LICENSES:-$repo_root/THIRD_PARTY_LICENSES.html}
if [ ! -f "$binary" ]; then
  echo "release binary not found: $binary" >&2
  exit 1
fi
if [ ! -f "$rondo_runtime" ] || [ ! -x "$rondo_runtime" ]; then
  echo "release Rondo runtime not found or not executable: $rondo_runtime" >&2
  exit 1
fi
if [ ! -f "$rondo_provenance" ]; then
  echo "release Rondo provenance not found: $rondo_provenance" >&2
  exit 1
fi
for required_file in LICENSE NOTICE; do
  if [ ! -f "$rondo_source/$required_file" ]; then
    echo "release Rondo file not found: $rondo_source/$required_file" >&2
    exit 1
  fi
done
if [ ! -f "$third_party_licenses" ]; then
  echo "release third-party license report not found: $third_party_licenses" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "release provenance validation requires jq" >&2
  exit 1
fi
source_commit=$(
  jq -er '
    select(.schema == "nopal.rondo_runtime_provenance/v1") |
    select(.runtime_version == "0.1.0") |
    select(.artifact_format == "escript") |
    select(.erlang_major == 28) |
    .source_commit |
    select(type == "string" and test("^[0-9a-f]{40}$"))
  ' "$rondo_provenance"
) || {
  echo "release Rondo provenance is malformed or incompatible" >&2
  exit 1
}
checkout_commit=$(git -C "$rondo_source" rev-parse HEAD 2>/dev/null) || {
  echo "release Rondo source checkout is unavailable: $rondo_source" >&2
  exit 1
}
if [ "$checkout_commit" != "$source_commit" ]; then
  echo "release Rondo source commit $checkout_commit does not match provenance $source_commit" >&2
  exit 1
fi

for required_file in LICENSE README.md NOTICE.md; do
  if [ ! -f "$source_root/$required_file" ]; then
    echo "release file not found: $source_root/$required_file" >&2
    exit 1
  fi
done

mkdir -p "$output_dir"
stem="nopal-$tag-$target"
archive="$output_dir/$stem.tar.gz"
stage_root="$output_dir/.$stem.stage.$$"
tar_path="$output_dir/.$stem.tar.$$"
archive_tmp="$output_dir/.$stem.tar.gz.$$"
trap 'rm -rf "$stage_root"; rm -f "$tar_path" "$archive_tmp"' EXIT HUP INT TERM
mkdir -p "$stage_root/$stem"
cp "$binary" "$stage_root/$stem/nopal"
cp "$rondo_runtime" "$stage_root/$stem/rondo"
cp "$rondo_provenance" "$stage_root/$stem/rondo-runtime.json"
cp "$source_root/LICENSE" "$stage_root/$stem/LICENSE"
cp "$source_root/README.md" "$stage_root/$stem/README.md"
cp "$source_root/NOTICE.md" "$stage_root/$stem/NOTICE.md"
cp "$rondo_source/LICENSE" "$stage_root/$stem/Rondo-LICENSE"
cp "$rondo_source/NOTICE" "$stage_root/$stem/Rondo-NOTICE"
cp "$third_party_licenses" "$stage_root/$stem/THIRD_PARTY_LICENSES.html"
chmod 0755 "$stage_root/$stem"
chmod 0755 "$stage_root/$stem/nopal"
chmod 0755 "$stage_root/$stem/rondo"
chmod 0644 \
  "$stage_root/$stem/LICENSE" \
  "$stage_root/$stem/README.md" \
  "$stage_root/$stem/NOTICE.md" \
  "$stage_root/$stem/Rondo-LICENSE" \
  "$stage_root/$stem/Rondo-NOTICE" \
  "$stage_root/$stem/THIRD_PARTY_LICENSES.html" \
  "$stage_root/$stem/rondo-runtime.json"

# Normalize every ustar field that can vary across runs. The fixed timestamp
# is representable by POSIX touch and both tar implementations on the release
# runners. Explicit members plus --no-recursion fix archive order, and gzip -n
# removes the gzip header's source name and timestamp.
TZ=UTC touch -t 200001010000.00 \
  "$stage_root/$stem" \
  "$stage_root/$stem/nopal" \
  "$stage_root/$stem/rondo" \
  "$stage_root/$stem/LICENSE" \
  "$stage_root/$stem/README.md" \
  "$stage_root/$stem/NOTICE.md" \
  "$stage_root/$stem/Rondo-LICENSE" \
  "$stage_root/$stem/Rondo-NOTICE" \
  "$stage_root/$stem/THIRD_PARTY_LICENSES.html" \
  "$stage_root/$stem/rondo-runtime.json"

case $(tar --version 2>/dev/null | head -1) in
  *GNU*)
    tar --format=ustar --owner=root:0 --group=root:0 --no-recursion \
      -cf "$tar_path" -C "$stage_root" \
      "$stem" "$stem/nopal" "$stem/rondo" "$stem/rondo-runtime.json" "$stem/LICENSE" "$stem/README.md" "$stem/NOTICE.md" "$stem/Rondo-LICENSE" "$stem/Rondo-NOTICE" "$stem/THIRD_PARTY_LICENSES.html"
    ;;
  *)
    COPYFILE_DISABLE=1 tar --format ustar \
      --uid 0 --gid 0 --uname root --gname root --no-recursion \
      -cf "$tar_path" -C "$stage_root" \
      "$stem" "$stem/nopal" "$stem/rondo" "$stem/rondo-runtime.json" "$stem/LICENSE" "$stem/README.md" "$stem/NOTICE.md" "$stem/Rondo-LICENSE" "$stem/Rondo-NOTICE" "$stem/THIRD_PARTY_LICENSES.html"
    ;;
esac
gzip -n -9 -c "$tar_path" > "$archive_tmp"
chmod 0644 "$archive_tmp"
mv -f "$archive_tmp" "$archive"
echo "$archive"
