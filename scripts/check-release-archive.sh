#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <release.tar.gz>" >&2
  exit 2
fi

archive=$1
if [ ! -f "$archive" ]; then
  echo "release archive not found: $archive" >&2
  exit 1
fi
if tar -tzf "$archive" \
  | grep -Eiq '(^|/)(rondo|field|cockpit|desktop|memento|herdr|tmux)(/|$)|nopal-field-native'; then
  echo "release archive contains a removed product member" >&2
  exit 1
fi
