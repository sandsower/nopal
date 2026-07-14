#!/bin/sh

set -eu

if [ "${NOPAL_INSTALL_TEST_OPEN_MODE:-}" = "fail-new-accept-old" ] && [ "${1:-}" != "-f" ]; then
  if [ -f "$1/old-install-sentinel" ]; then
    cp "$0" "$NOPAL_INSTALL_TEST_OPEN_STATE"
    exit 0
  fi
  exit 1
fi

if [ "${NOPAL_INSTALL_TEST_PGREP_MODE:-}" = "error" ]; then
  exit 2
fi

if [ "${NOPAL_INSTALL_TEST_PGREP_MODE:-}" = "race" ]; then
  if [ ! -e "$NOPAL_INSTALL_TEST_PGREP_STATE" ]; then
    cp "$0" "$NOPAL_INSTALL_TEST_PGREP_STATE"
    exit 1
  fi
  echo 4242
  exit 0
fi

if [ "${NOPAL_INSTALL_TEST_PGREP_MODE:-}" = "running-then-stopped" ]; then
  if [ ! -e "$NOPAL_INSTALL_TEST_PGREP_STATE" ]; then
    cp "$0" "$NOPAL_INSTALL_TEST_PGREP_STATE"
    echo 4242
    exit 0
  fi
  exit 1
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
installer="$script_dir/install-macos-app.sh"
source_app=${1:-"$script_dir/../Nopal.app"}

if [ ! -d "$source_app" ]; then
  echo "source app does not exist: $source_app" >&2
  exit 2
fi

test_root=$(mktemp -d "${TMPDIR:-/tmp}/nopal-install-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

seed_old_install() {
  case_dir=$1
  mkdir -p "$case_dir"
  /usr/bin/ditto "$source_app" "$case_dir/Nopal.app"
  cp "$0" "$case_dir/Nopal.app/old-install-sentinel"
}

assert_old_install_restored() {
  case_dir=$1
  if [ ! -f "$case_dir/Nopal.app/old-install-sentinel" ]; then
    echo "prior Nopal installation was not restored: $case_dir" >&2
    exit 1
  fi
}

open_failure="$test_root/open-failure"
open_failure_pgrep_state="$test_root/open-failure-pgrep-state"
old_reopened_state="$test_root/old-reopened-state"
seed_old_install "$open_failure"
if env \
  NOPAL_INSTALL_OSASCRIPT=/usr/bin/true \
  NOPAL_INSTALL_PGREP="$0" \
  NOPAL_INSTALL_TEST_PGREP_MODE=running-then-stopped \
  NOPAL_INSTALL_TEST_PGREP_STATE="$open_failure_pgrep_state" \
  NOPAL_INSTALL_OPEN="$0" \
  NOPAL_INSTALL_TEST_OPEN_MODE=fail-new-accept-old \
  NOPAL_INSTALL_TEST_OPEN_STATE="$old_reopened_state" \
  "$installer" reinstall "$source_app" "$open_failure"; then
  echo "reinstall unexpectedly succeeded when relaunch failed" >&2
  exit 1
fi
assert_old_install_restored "$open_failure"
if [ ! -e "$old_reopened_state" ]; then
  echo "prior running Nopal installation was not reopened after rollback" >&2
  exit 1
fi

staging_failure="$test_root/staging-failure"
staging_failure_pgrep_state="$test_root/staging-failure-pgrep-state"
staging_failure_reopened_state="$test_root/staging-failure-reopened-state"
seed_old_install "$staging_failure"
if env \
  NOPAL_INSTALL_DITTO=/usr/bin/false \
  NOPAL_INSTALL_OSASCRIPT=/usr/bin/true \
  NOPAL_INSTALL_PGREP="$0" \
  NOPAL_INSTALL_TEST_PGREP_MODE=running-then-stopped \
  NOPAL_INSTALL_TEST_PGREP_STATE="$staging_failure_pgrep_state" \
  NOPAL_INSTALL_OPEN="$0" \
  NOPAL_INSTALL_TEST_OPEN_MODE=fail-new-accept-old \
  NOPAL_INSTALL_TEST_OPEN_STATE="$staging_failure_reopened_state" \
  "$installer" reinstall "$source_app" "$staging_failure"; then
  echo "reinstall unexpectedly succeeded when staging failed" >&2
  exit 1
fi
assert_old_install_restored "$staging_failure"
if [ ! -e "$staging_failure_reopened_state" ]; then
  echo "prior running Nopal installation was not reopened after staging failure" >&2
  exit 1
fi

probe_error="$test_root/probe-error"
seed_old_install "$probe_error"
if env \
  NOPAL_INSTALL_PGREP="$0" \
  NOPAL_INSTALL_TEST_PGREP_MODE=error \
  "$installer" install "$source_app" "$probe_error"; then
  echo "install unexpectedly succeeded when process inspection failed" >&2
  exit 1
fi
assert_old_install_restored "$probe_error"

probe_race="$test_root/probe-race"
probe_race_state="$test_root/probe-race-state"
seed_old_install "$probe_race"
if env \
  NOPAL_INSTALL_PGREP="$0" \
  NOPAL_INSTALL_TEST_PGREP_MODE=race \
  NOPAL_INSTALL_TEST_PGREP_STATE="$probe_race_state" \
  "$installer" install "$source_app" "$probe_race"; then
  echo "install unexpectedly succeeded when Nopal started during staging" >&2
  exit 1
fi
assert_old_install_restored "$probe_race"

echo "macOS installer failure-path tests passed"
