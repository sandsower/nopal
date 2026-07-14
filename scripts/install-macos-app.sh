#!/bin/sh

set -eu

usage() {
  echo "usage: $0 <install|reinstall> <source-app> <applications-dir>" >&2
  exit 2
}

[ "$#" -eq 3 ] || usage

mode=$1
source_app=$2
applications_dir=$3

case "$mode" in
  install | reinstall) ;;
  *) usage ;;
esac

case "$applications_dir" in
  /*) ;;
  *)
    echo "applications directory must be absolute: $applications_dir" >&2
    exit 2
    ;;
esac

ditto_command=${NOPAL_INSTALL_DITTO:-/usr/bin/ditto}
open_command=${NOPAL_INSTALL_OPEN:-/usr/bin/open}
osascript_command=${NOPAL_INSTALL_OSASCRIPT:-/usr/bin/osascript}
pgrep_command=${NOPAL_INSTALL_PGREP:-/usr/bin/pgrep}
plutil_command=${NOPAL_INSTALL_PLUTIL:-/usr/bin/plutil}

source_plist="$source_app/Contents/Info.plist"
source_executable="$source_app/Contents/MacOS/nopal-field-native"
source_helper="$source_app/Contents/MacOS/nopal"
destination="$applications_dir/Nopal.app"
destination_executable="$destination/Contents/MacOS/nopal-field-native"

if [ ! -f "$source_plist" ] || [ ! -x "$source_executable" ] || [ ! -x "$source_helper" ]; then
  echo "source app is incomplete: $source_app" >&2
  exit 1
fi

"$plutil_command" -lint "$source_plist" >/dev/null
source_version=$("$plutil_command" -extract CFBundleShortVersionString raw "$source_plist")

running_pids() {
  "$pgrep_command" -f -x "$destination_executable" 2>/dev/null || true
}

if [ -n "$(running_pids)" ]; then
  if [ "$mode" = "install" ]; then
    echo "Nopal is running from $destination; quit it or use 'make reinstall'" >&2
    exit 1
  fi

  destination_plist="$destination/Contents/Info.plist"
  if [ ! -f "$destination_plist" ]; then
    echo "cannot identify the running Nopal bundle at $destination" >&2
    exit 1
  fi
  bundle_id=$("$plutil_command" -extract CFBundleIdentifier raw "$destination_plist")
  if ! "$osascript_command" -e "tell application id \"$bundle_id\" to quit"; then
    echo "Nopal did not accept a graceful quit request; quit it manually and retry" >&2
    exit 1
  fi

  attempts=0
  while [ -n "$(running_pids)" ] && [ "$attempts" -lt 50 ]; do
    sleep 0.1
    attempts=$((attempts + 1))
  done
  if [ -n "$(running_pids)" ]; then
    echo "Nopal is still running after the graceful quit request" >&2
    exit 1
  fi
fi

mkdir -p "$applications_dir"
stage="$applications_dir/.Nopal.app.install.$$"
backup="$applications_dir/.Nopal.app.backup.$$"

# The destination is never partially overwritten. A complete staged bundle is
# swapped into place, and the prior installation is restored if the swap fails.
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  rm -rf "$stage"
  if [ -e "$backup" ] && [ ! -e "$destination" ]; then
    mv "$backup" "$destination"
  fi
  exit "$status"
}
trap cleanup EXIT HUP INT TERM

rm -rf "$stage" "$backup"
"$ditto_command" "$source_app" "$stage"
"$plutil_command" -lint "$stage/Contents/Info.plist" >/dev/null

if [ -e "$destination" ]; then
  mv "$destination" "$backup"
fi
mv "$stage" "$destination"

installed_version=$(
  "$plutil_command" -extract CFBundleShortVersionString raw \
    "$destination/Contents/Info.plist"
)
if [ "$installed_version" != "$source_version" ]; then
  rm -rf "$destination"
  echo "installed Nopal version $installed_version does not match build $source_version" >&2
  exit 1
fi

rm -rf "$backup"
trap - EXIT HUP INT TERM
printf 'Installed Nopal %s at %s\n' "$installed_version" "$destination"

if [ "$mode" = "reinstall" ]; then
  "$open_command" "$destination"
fi
