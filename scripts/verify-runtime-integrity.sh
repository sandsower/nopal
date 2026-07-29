#!/bin/sh

set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <runtime-label> <runtime-root> <expected-sha256-identity>" >&2
  exit 2
fi

label=$1
root=$2
expected=$3
repo_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)

actual=$(python3 "$repo_root/scripts/hash-runtime-tree.py" "$root")
if [ "$actual" != "$expected" ]; then
  echo "$label has integrity $actual, expected $expected" >&2
  exit 1
fi

printf '%s\n' "$actual"
