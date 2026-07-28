#!/bin/sh

# Sourced by release scripts and their contract tests.

release_workspace_version() {
  release_manifest=$1
  release_version=$(
    sed -n 's/^version = "\(.*\)"$/\1/p' "$release_manifest" | head -1
  )
  if [ -z "$release_version" ]; then
    echo "workspace version not found in $release_manifest" >&2
    return 1
  fi
  printf '%s\n' "$release_version"
}

release_next_patch_version() {
  release_current_version=$1
  if ! printf '%s\n' "$release_current_version" \
    | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then
    echo "workspace version must be numeric major.minor.patch: $release_current_version" >&2
    return 1
  fi

  release_major=${release_current_version%%.*}
  release_remainder=${release_current_version#*.}
  release_minor=${release_remainder%%.*}
  release_patch=${release_remainder#*.}
  release_patch=$((release_patch + 1))
  printf '%s.%s.%s\n' "$release_major" "$release_minor" "$release_patch"
}

release_replace_single_literal() {
  release_input=$1
  release_output=$2
  release_old=$3
  release_new=$4
  awk -v old="$release_old" -v new="$release_new" '
    {
      line = $0
      rewritten = ""
      while ((position = index(line, old)) != 0) {
        rewritten = rewritten substr(line, 1, position - 1) new
        line = substr(line, position + length(old))
        replacements++
      }
      print rewritten line
    }
    END {
      if (replacements != 1) {
        exit 42
      }
    }
  ' "$release_input" > "$release_output"
}

# Keep the workspace package version and its builtin dogfood requirement as
# one versioned contract. Both proposals are prepared before either source
# file is replaced, so malformed or drifted input cannot leave a partial bump.
release_bump_project_versions() {
  release_manifest=$1
  release_bundle=$2
  release_current=$(release_workspace_version "$release_manifest") || return
  release_next=$(release_next_patch_version "$release_current") || return
  release_manifest_tmp="$release_manifest.release-bump.$$"
  release_bundle_tmp="$release_bundle.release-bump.$$"

  if ! release_replace_single_literal \
    "$release_manifest" "$release_manifest_tmp" \
    "version = \"$release_current\"" "version = \"$release_next\""; then
    rm -f "$release_manifest_tmp" "$release_bundle_tmp"
    echo "workspace version must occur exactly once in $release_manifest" >&2
    return 1
  fi
  if ! release_replace_single_literal \
    "$release_bundle" "$release_bundle_tmp" \
    "\"requirement\": \"=$release_current\"" \
    "\"requirement\": \"=$release_next\""; then
    rm -f "$release_manifest_tmp" "$release_bundle_tmp"
    echo "exact builtin requirement =$release_current must occur exactly once in $release_bundle" >&2
    return 1
  fi

  mv "$release_manifest_tmp" "$release_manifest"
  mv "$release_bundle_tmp" "$release_bundle"
  printf '%s\n' "$release_next"
}

release_validate_tag() {
  release_manifest=$1
  release_tag=$2
  release_version=$(release_workspace_version "$release_manifest") || return
  release_expected_tag="v$release_version"
  if [ "$release_tag" != "$release_expected_tag" ]; then
    echo "release tag $release_tag does not match workspace version $release_version" >&2
    return 1
  fi
  printf '%s\n' "$release_version"
}
