#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"

forbidden_patterns=('crust')
found=0

filter_matches() {
  local input=$1
  local excluded=$2
  local output status

  [[ -n "$input" ]] || return 0
  if output=$(grep -v -F -- "$excluded" <<<"$input"); then
    printf '%s\n' "$output"
    return 0
  else
    status=$?
    if (( status == 1 )); then
      return 0
    fi
    printf 'identity exclusion filter failed for pattern %s\n' "$excluded" >&2
    return "$status"
  fi
}

while IFS= read -r -d '' path; do
  case "$path" in
    plans/*|docs/adr/*|.git/*|node_modules|node_modules/*|target/*|output/*|scripts/check-active-tree-identity.sh)
      continue
      ;;
  esac

  # Deleted paths remain in the index until staging and are not active files.
  [[ -e "$path" ]] || continue

  for pattern in "${forbidden_patterns[@]}"; do
    if grep -q -i -E -- "$pattern" <<<"$path"; then
      printf '%s: path matches active identity pattern %s\n' "$path" "$pattern"
      found=1
    else
      status=$?
      if (( status != 1 )); then
        printf 'identity path scan failed for %s\n' "$path" >&2
        exit "$status"
      fi
    fi

    if matches=$(grep -n -H -I -i -E -- "$pattern" "$path"); then
      :
    else
      status=$?
      if (( status != 1 )); then
        printf 'identity content scan failed for %s\n' "$path" >&2
        exit "$status"
      fi
      matches=
    fi
    case "$path" in
      crates/nopal-core/src/discover.rs)
        if matches=$(filter_matches "$matches" '.crust'); then
          :
        else
          status=$?
          exit "$status"
        fi
        ;;
      crates/nopal-cli/tests/coordinator.rs)
        if matches=$(filter_matches "$matches" 'CRUST_CONFIG_DIR'); then
          :
        else
          status=$?
          exit "$status"
        fi
        ;;
    esac
    if [[ -n "$matches" ]]; then
      printf '%s\n' "$matches"
      found=1
    fi
  done
done < <(git ls-files --cached --others --exclude-standard -z | LC_ALL=C sort -z)

if (( found != 0 )); then
  printf 'active-tree identity check failed\n' >&2
  exit 1
fi

printf 'active-tree identity check passed\n'
