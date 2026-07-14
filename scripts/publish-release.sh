#!/bin/sh

set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <vX.Y.Z-tag> <asset-directory>" >&2
  exit 2
fi

tag=$1
asset_dir=$2
if [ ! -d "$asset_dir" ]; then
  echo "release asset directory not found: $asset_dir" >&2
  exit 1
fi

asset_count=0
for asset in "$asset_dir"/*; do
  if [ -f "$asset" ]; then
    asset_count=$((asset_count + 1))
  fi
done
if [ "$asset_count" -eq 0 ]; then
  echo "release asset directory is empty: $asset_dir" >&2
  exit 1
fi

if gh release view "$tag" --json isDraft >/dev/null 2>&1; then
  is_draft=$(gh release view "$tag" --json isDraft --jq '.isDraft')
else
  if ! gh release create "$tag" --draft --verify-tag --generate-notes --title "$tag"; then
    # A concurrent run may have created it after the first view.
    if ! gh release view "$tag" --json isDraft >/dev/null; then
      echo "release creation failed and no concurrent release appeared: $tag" >&2
      exit 1
    fi
  fi
  is_draft=$(gh release view "$tag" --json isDraft --jq '.isDraft')
fi

remote_assets=$(gh release view "$tag" --json assets --jq '.assets[].name')
download_dir=$(mktemp -d "${TMPDIR:-/tmp}/nopal-release-assets.XXXXXX")
trap 'rm -rf "$download_dir"' EXIT HUP INT TERM

# Preflight every same-name asset before uploading anything. A conflicting
# published asset must fail the whole reconciliation without partial writes.
for asset in "$asset_dir"/*; do
  [ -f "$asset" ] || continue
  name=$(basename "$asset")
  if printf '%s\n' "$remote_assets" | grep -Fqx "$name"; then
    rm -f "$download_dir/$name"
    gh release download "$tag" --pattern "$name" --dir "$download_dir" --clobber
    if ! cmp -s "$asset" "$download_dir/$name"; then
      echo "release asset $name conflicts with the local file; refusing to overwrite" >&2
      exit 1
    fi
  fi
done

for asset in "$asset_dir"/*; do
  [ -f "$asset" ] || continue
  name=$(basename "$asset")
  if ! printf '%s\n' "$remote_assets" | grep -Fqx "$name"; then
    if ! gh release upload "$tag" "$asset"; then
      # Another reconciler may have uploaded the same asset after our view.
      rm -f "$download_dir/$name"
      if ! gh release download "$tag" --pattern "$name" \
        --dir "$download_dir" --clobber; then
        echo "release asset upload failed and no concurrent asset appeared: $name" >&2
        exit 1
      fi
      if ! cmp -s "$asset" "$download_dir/$name"; then
        echo "concurrent release asset $name conflicts with the local file" >&2
        exit 1
      fi
    fi
  fi
done

if [ "$is_draft" = "true" ]; then
  if ! gh release edit "$tag" --draft=false; then
    # Publishing is also safe to reconcile if a peer won the final race.
    current_is_draft=$(gh release view "$tag" --json isDraft --jq '.isDraft')
    if [ "$current_is_draft" != "false" ]; then
      echo "release stayed draft after publication failed: $tag" >&2
      exit 1
    fi
  fi
fi
