#!/bin/sh
set -eu

# Declare the non-system executable used by native workspace tests so Nopal can byte-lock it before gate execution.
command -v tmux >/dev/null

exec cargo test --workspace
