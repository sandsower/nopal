#!/bin/sh

set -eu

usage() {
  echo "usage: $0 <install|rollback> <absolute-prefix>" >&2
  exit 2
}

[ "$#" -eq 2 ] || usage
mode=$1
prefix=$2
case "$mode" in
  install | rollback) ;;
  *) usage ;;
esac
case "$prefix" in
  /*) ;;
  *) echo "installation prefix must be absolute: $prefix" >&2; exit 2 ;;
esac

source_root=$(CDPATH='' cd "$(dirname "$0")" && pwd)
runtime_node="$source_root/runtime/node"
if [ ! -x "$runtime_node" ]; then
  echo "release archive is incomplete: missing executable runtime/node" >&2
  exit 1
fi
releases="$prefix/lib/nopal"
bin_dir="$prefix/bin"
current="$releases/current"
previous="$releases/previous"

read_link() {
  if [ -L "$1" ]; then
    readlink "$1"
  fi
}

switch_link() {
  link=$1
  target=$2
  temporary="$link.tmp.$$"
  rm -f "$temporary"
  ln -s "$target" "$temporary"
  "$runtime_node" -e 'require("node:fs").renameSync(process.argv[1], process.argv[2])' \
    "$temporary" "$link"
}

preflight_launcher() {
  launcher="$bin_dir/nopal"
  if [ -e "$launcher" ] && [ ! -L "$launcher" ]; then
    echo "refusing to replace non-symlink launcher: $launcher" >&2
    exit 1
  fi
}

ensure_launcher() {
  launcher="$bin_dir/nopal"
  expected="../lib/nopal/current/nopal"
  preflight_launcher
  if [ "$(read_link "$launcher")" != "$expected" ]; then
    switch_link "$launcher" "$expected"
  fi
}

if [ "$mode" = rollback ]; then
  if [ ! -d "$releases" ] || [ ! -d "$bin_dir" ]; then
    echo "rollback requires an existing Nopal installation under $prefix" >&2
    exit 1
  fi
  preflight_launcher
  old_current=$(read_link "$current")
  old_previous=$(read_link "$previous")
  if [ -z "$old_current" ] || [ -z "$old_previous" ]; then
    echo "rollback requires both current and previous installed releases" >&2
    exit 1
  fi
  if [ ! -x "$releases/$old_previous/nopal" ] || [ ! -f "$releases/$old_previous/distribution.json" ]; then
    echo "previous release is incomplete: $releases/$old_previous" >&2
    exit 1
  fi
  switch_link "$current" "$old_previous"
  switch_link "$previous" "$old_current"
  ensure_launcher
  printf 'Rolled back Nopal to %s\n' "$old_previous"
  exit 0
fi

for required in distribution.json nopal runtime/node runtime/pi/dist/cli.js \
  share/nopal/extensions/policy-gate/index.ts share/nopal/resources/beislid/provenance.json; do
  if [ ! -e "$source_root/$required" ]; then
    echo "release archive is incomplete: missing $required" >&2
    exit 1
  fi
done
identity=$("$runtime_node" -e '
const fs = require("node:fs");
const doc = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (doc.kind !== "nopal.release_distribution/v1") throw new Error("invalid release distribution manifest");
if (!/^[0-9]+\.[0-9]+\.[0-9]+$/.test(doc.version ?? "")) throw new Error("invalid release version");
if (!/^[A-Za-z0-9_.-]+$/.test(doc.target ?? "")) throw new Error("invalid release target");
process.stdout.write(`${doc.version}-${doc.target}\n`);
' "$source_root/distribution.json")
destination="$releases/$identity"
mkdir -p "$releases" "$bin_dir"
preflight_launcher
if [ -e "$destination" ]; then
  if [ ! -d "$destination" ] || ! "$runtime_node" -e '
const fs = require("node:fs");
const path = require("node:path");
function kind(stat) {
  if (stat.isSymbolicLink()) return "link";
  if (stat.isDirectory()) return "directory";
  if (stat.isFile()) return "file";
  return "unsupported";
}
function compare(left, right, relative = ".") {
  const a = fs.lstatSync(left);
  const b = fs.lstatSync(right);
  const aKind = kind(a);
  const bKind = kind(b);
  if (aKind !== bKind || aKind === "unsupported") throw new Error(`${relative}: entry kind differs`);
  if ((a.mode & 0o7777) !== (b.mode & 0o7777)) throw new Error(`${relative}: entry mode differs`);
  if (aKind === "link") {
    if (fs.readlinkSync(left) !== fs.readlinkSync(right)) throw new Error(`${relative}: link target differs`);
    return;
  }
  if (aKind === "file") {
    if (a.size !== b.size || !fs.readFileSync(left).equals(fs.readFileSync(right))) {
      throw new Error(`${relative}: file bytes differ`);
    }
    return;
  }
  const aNames = fs.readdirSync(left).sort();
  const bNames = fs.readdirSync(right).sort();
  if (aNames.length !== bNames.length || aNames.some((name, index) => name !== bNames[index])) {
    throw new Error(`${relative}: directory members differ`);
  }
  for (const name of aNames) compare(path.join(left, name), path.join(right, name), path.join(relative, name));
}
compare(process.argv[1], process.argv[2]);
' "$source_root" "$destination"; then
    echo "installed release identity conflicts with different bytes: $destination" >&2
    exit 1
  fi
else
  stage="$releases/.$identity.install.$$"
  trap 'rm -rf "$stage"' EXIT HUP INT TERM
  mkdir "$stage"
  cp -R "$source_root/." "$stage/"
  mv "$stage" "$destination"
  trap - EXIT HUP INT TERM
fi

old_current=$(read_link "$current")
if [ -n "$old_current" ] && [ "$old_current" != "$identity" ]; then
  switch_link "$previous" "$old_current"
fi
switch_link "$current" "$identity"
ensure_launcher
printf 'Installed Nopal %s at %s\n' "$identity" "$destination"
