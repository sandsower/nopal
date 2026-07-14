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
