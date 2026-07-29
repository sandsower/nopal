#!/bin/sh

set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <target-triple> <vX.Y.Z-tag> [output-dir]" >&2
  exit 2
fi

target=$1
tag=$2
output_dir=${3:-dist}
repo_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
source_root=${NOPAL_RELEASE_SOURCE_ROOT:-$repo_root}
. "$repo_root/scripts/release-version.sh"
release_validate_tag "$source_root/Cargo.toml" "$tag" >/dev/null

binary=${NOPAL_RELEASE_BINARY:-target/$target/release/nopal}
pi_root=${NOPAL_RELEASE_PI_ROOT:-_runtime/pi}
node_binary=${NOPAL_RELEASE_NODE:-_runtime/node}
node_license=${NOPAL_RELEASE_NODE_LICENSE:-_runtime/node-LICENSE}
third_party_licenses=${NOPAL_RELEASE_THIRD_PARTY_LICENSES:-$repo_root/THIRD_PARTY_LICENSES.html}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f1
  else
    shasum -a 256 "$1" | cut -d ' ' -f1
  fi
}

case "$target" in
  aarch64-apple-darwin)
    expected_node_sha=913b144fdb40638b1acef7974ab3c33fbd527cc0974cb5da467ab1e6ac51b4d4
    expected_pi_integrity=sha256:1849e5f3271e6386319323a9e0dbf0f171c6d22558c5e9c05717a20b116915e8
    ;;
  x86_64-apple-darwin)
    expected_node_sha=bf0e0ff20d4e5a16436d1ec372e47161e52be8e487db8070ae3f06b01efbba0c
    expected_pi_integrity=sha256:2a49edc0cbfae11a095051da5d6d79cd994c85c555ee4f24c74ec3700ef34b4e
    ;;
  x86_64-unknown-linux-gnu)
    expected_node_sha=1bec56ef7cfa9a76f3e0b7c0a87f220eb73f23102b9c0b4c7529a3f7c3ce7c31
    expected_pi_integrity=sha256:7496869e98bed7cdfa6f12130e4d0bb05c2fc3e7494cb8cb3be5dc81fdff063e
    ;;
  test-release-target)
    expected_node_sha=${NOPAL_RELEASE_TEST_NODE_SHA256:?test target requires NOPAL_RELEASE_TEST_NODE_SHA256}
    expected_pi_integrity=${NOPAL_RELEASE_TEST_PI_INTEGRITY:?test target requires NOPAL_RELEASE_TEST_PI_INTEGRITY}
    ;;
  *)
    echo "unsupported release target: $target" >&2
    exit 1
    ;;
esac

for executable in "$binary" "$node_binary"; do
  if [ ! -f "$executable" ] || [ ! -x "$executable" ]; then
    echo "release executable not found or not executable: $executable" >&2
    exit 1
  fi
done
version=$(release_workspace_version "$source_root/Cargo.toml")
source_commit=$(git -C "$source_root" rev-parse HEAD)
python3 - "$binary" "$version" "$source_commit" <<'PY'
import json,subprocess,sys
binary,version,commit=sys.argv[1:]
document=json.loads(subprocess.check_output([binary,"info","--json"],text=True))
if document.get("kind") != "nopal.info/v1" or document.get("ok") is not True:
    raise SystemExit("release binary did not emit a valid nopal.info/v1 identity")
if document.get("version") != version:
    raise SystemExit("release binary version does not match the workspace")
if document.get("commit") != commit:
    raise SystemExit("release binary commit does not match the tagged source commit")
PY
for required_file in LICENSE README.md NOTICE.md scripts/install-release.sh \
  scripts/create-release-archive.py resources/beislid/LICENSE \
  resources/beislid/provenance.json "$node_license" "$third_party_licenses"; do
  case "$required_file" in
    /*) path=$required_file ;;
    *) path=$source_root/$required_file ;;
  esac
  if [ ! -f "$path" ]; then
    echo "release file not found: $path" >&2
    exit 1
  fi
done
for adapter_file in index.ts classifier.ts guard.ts nopal-cli.ts; do
  if [ ! -f "$source_root/extensions/policy-gate/$adapter_file" ]; then
    echo "release enforcement adapter file not found: $source_root/extensions/policy-gate/$adapter_file" >&2
    exit 1
  fi
done
if [ ! -d "$source_root/resources/beislid/skills" ]; then
  echo "release Beislið skills not found" >&2
  exit 1
fi
if [ ! -f "$pi_root/package.json" ] || [ ! -f "$pi_root/dist/cli.js" ]; then
  echo "release Pi package is incomplete: $pi_root" >&2
  exit 1
fi

python3 - "$pi_root/package.json" "$source_root/resources/beislid/provenance.json" <<'PY'
import json,sys
pi=json.load(open(sys.argv[1]))
if pi.get("name") != "@earendil-works/pi-coding-agent" or pi.get("version") != "0.80.6":
    raise SystemExit("release Pi package must be @earendil-works/pi-coding-agent@0.80.6")
if pi.get("bin",{}).get("pi") != "dist/cli.js":
    raise SystemExit("release Pi package does not declare dist/cli.js as its entrypoint")
beislid=json.load(open(sys.argv[2]))
if beislid.get("kind") != "nopal.beislid_provenance/v1" or beislid.get("version") != "0.4.16":
    raise SystemExit("release Beislið provenance must identify v0.4.16")
if beislid.get("commit") != "d7b6262ef1079f8a148dcfa3ca689c8f9c848220":
    raise SystemExit("release Beislið provenance commit is not pinned")
if beislid.get("skills_tree") != "git-sha1:d99efdb5a004c41673fa12627901d51f76b9a64c":
    raise SystemExit("release Beislið provenance tree is not pinned")
if beislid.get("materialized_integrity") != "sha256:3bf4fc7e423a541377e6de06b024db949adc457f08b1313c265ab5aa41bfb73a":
    raise SystemExit("release Beislið materialized integrity is not pinned")
PY

beislid_skills_integrity=$(python3 "$repo_root/scripts/hash-runtime-tree.py" \
  "$source_root/resources/beislid/skills")
if [ "$beislid_skills_integrity" != sha256:3bf4fc7e423a541377e6de06b024db949adc457f08b1313c265ab5aa41bfb73a ]; then
  echo "release Beislið skills have integrity $beislid_skills_integrity, expected pinned v0.4.16 bytes" >&2
  exit 1
fi
node_sha=$(sha256_file "$node_binary")
if [ "$node_sha" != "$expected_node_sha" ]; then
  echo "release Node executable has SHA-256 $node_sha, expected $expected_node_sha" >&2
  exit 1
fi
pi_integrity=$(python3 "$repo_root/scripts/hash-runtime-tree.py" "$pi_root")
if [ "$pi_integrity" != "$expected_pi_integrity" ]; then
  echo "release Pi runtime has integrity $pi_integrity, expected $expected_pi_integrity" >&2
  exit 1
fi

mkdir -p "$output_dir"
stem="nopal-$tag-$target"
archive="$output_dir/$stem.tar.gz"
stage_parent="$output_dir/.$stem.stage.$$"
stage="$stage_parent/$stem"
trap 'rm -rf "$stage_parent"' EXIT HUP INT TERM
mkdir -p "$stage/runtime" "$stage/share/nopal/extensions" "$stage/share/nopal/resources"
cp "$binary" "$stage/nopal"
cp "$node_binary" "$stage/runtime/node"
cp "$node_license" "$stage/runtime/Node-LICENSE"
cp -R "$pi_root" "$stage/runtime/pi"
cp -R "$source_root/extensions/policy-gate" "$stage/share/nopal/extensions/policy-gate"
cp -R "$source_root/resources/beislid" "$stage/share/nopal/resources/beislid"
cp "$source_root/scripts/install-release.sh" "$stage/install"
cp "$source_root/LICENSE" "$stage/LICENSE"
cp "$source_root/README.md" "$stage/README.md"
cp "$source_root/NOTICE.md" "$stage/NOTICE.md"
cp "$third_party_licenses" "$stage/THIRD_PARTY_LICENSES.html"
chmod 0755 "$stage/nopal" "$stage/runtime/node" "$stage/install"
chmod 0644 "$stage/LICENSE" "$stage/README.md" "$stage/NOTICE.md" \
  "$stage/THIRD_PARTY_LICENSES.html" "$stage/runtime/Node-LICENSE"

binary_integrity=sha256:$(sha256_file "$stage/nopal")
adapter_integrity=$(python3 "$repo_root/scripts/hash-runtime-tree.py" "$stage/share/nopal/extensions/policy-gate")
beislid_integrity=$(python3 "$repo_root/scripts/hash-runtime-tree.py" "$stage/share/nopal/resources/beislid")
python3 - "$stage/distribution.json" "$version" "$target" "$tag" "$source_commit" \
  "$binary_integrity" "$node_sha" "$pi_integrity" "$adapter_integrity" \
  "$beislid_integrity" <<'PY'
import json,sys
(path,version,target,tag,commit,binary_integrity,node_sha,pi_integrity,
 adapter_integrity,beislid_integrity)=sys.argv[1:]
document={
  "kind":"nopal.release_distribution/v1",
  "version":version,
  "target":target,
  "source":{"tag":tag,"commit":commit},
  "nopal":{"binary_integrity":binary_integrity},
  "node":{"version":"22.22.0","executable_integrity":f"sha256:{node_sha}"},
  "pi":{"package":"@earendil-works/pi-coding-agent","version":"0.80.6","runtime_integrity":pi_integrity},
  "policy_adapter":{"integrity":adapter_integrity},
  "beislid":{
    "version":"0.4.16",
    "commit":"d7b6262ef1079f8a148dcfa3ca689c8f9c848220",
    "tree_integrity":beislid_integrity
  },
  "resources":["extensions/policy-gate","resources/beislid/skills"]
}
open(path,"w").write(json.dumps(document,indent=2)+"\n")
PY
chmod 0644 "$stage/distribution.json"

python3 "$repo_root/scripts/create-release-archive.py" "$stage" "$archive" >/dev/null
printf '%s\n' "$archive"
