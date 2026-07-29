#!/usr/bin/env python3
"""Hash a Pi runtime tree with the same codec as the Nopal launcher."""

from __future__ import annotations

import hashlib
import os
import stat
import sys
from pathlib import Path


def update_path(hasher: "hashlib._Hash", root: Path, path: Path) -> None:
    metadata = path.lstat()
    relative = "." if path == root else path.relative_to(root).as_posix()
    if path.is_symlink():
        resolved = path.resolve(strict=True)
        try:
            resolved.relative_to(root)
        except ValueError as error:
            raise ValueError(f"runtime symlink escapes its package: {path}") from error
        target = os.readlink(path)
        target.encode("utf-8")
        hasher.update(b"link\0")
        hasher.update(relative.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(target.encode("utf-8"))
        hasher.update(b"\0")
        return
    if path.is_file():
        hasher.update(b"file\0")
        hasher.update(relative.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(b"x\0" if metadata.st_mode & 0o111 else b"-\0")
        with path.open("rb") as source:
            while chunk := source.read(64 * 1024):
                hasher.update(chunk)
        return
    if not path.is_dir():
        raise ValueError(f"runtime contains unsupported entry: {path}")
    hasher.update(b"dir\0")
    hasher.update(relative.encode("utf-8"))
    hasher.update(b"\0")
    for child in sorted(path.iterdir(), key=lambda item: item.name):
        update_path(hasher, root, child)


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <runtime-root>", file=sys.stderr)
        return 2
    root = Path(sys.argv[1]).resolve(strict=True)
    if not root.is_dir():
        print(f"runtime root is not a directory: {root}", file=sys.stderr)
        return 1
    hasher = hashlib.sha256()
    try:
        update_path(hasher, root, root)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"cannot hash runtime tree: {error}", file=sys.stderr)
        return 1
    print(f"sha256:{hasher.hexdigest()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
