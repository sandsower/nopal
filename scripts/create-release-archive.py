#!/usr/bin/env python3
"""Create a byte-reproducible PAX tar.gz release archive."""

from __future__ import annotations

import gzip
import os
import stat
import sys
import tarfile
import tempfile
from pathlib import Path

FIXED_MTIME = 946684800


def entries(root: Path) -> list[Path]:
    result = [root]
    for directory, names, files in os.walk(root, topdown=True, followlinks=False):
        names.sort()
        files.sort()
        base = Path(directory)
        for name in list(names):
            path = base / name
            result.append(path)
            if path.is_symlink():
                names.remove(name)
        result.extend(base / name for name in files)
    return sorted(set(result), key=lambda path: path.relative_to(root.parent).as_posix())


def normalized_info(path: Path, arcname: str) -> tarfile.TarInfo:
    metadata = path.lstat()
    info = tarfile.TarInfo(arcname)
    info.uid = 0
    info.gid = 0
    info.uname = "root"
    info.gname = "root"
    info.mtime = FIXED_MTIME
    info.mode = stat.S_IMODE(metadata.st_mode)
    info.pax_headers = {}
    if path.is_symlink():
        info.type = tarfile.SYMTYPE
        info.linkname = os.readlink(path)
    elif path.is_dir():
        info.type = tarfile.DIRTYPE
        info.size = 0
    elif path.is_file():
        info.type = tarfile.REGTYPE
        info.size = metadata.st_size
    else:
        raise ValueError(f"unsupported release entry: {path}")
    return info


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <stage-directory> <archive.tar.gz>", file=sys.stderr)
        return 2
    stage = Path(sys.argv[1]).resolve(strict=True)
    archive = Path(sys.argv[2]).absolute()
    if not stage.is_dir():
        print(f"stage is not a directory: {stage}", file=sys.stderr)
        return 1
    archive.parent.mkdir(parents=True, exist_ok=True)
    temporary = tempfile.NamedTemporaryFile(prefix=f".{archive.name}.", dir=archive.parent, delete=False)
    temporary_path = Path(temporary.name)
    temporary.close()
    tar_path = temporary_path.with_suffix(".tar")
    try:
        with tarfile.open(tar_path, mode="w", format=tarfile.PAX_FORMAT) as output:
            for path in entries(stage):
                arcname = path.relative_to(stage.parent).as_posix()
                info = normalized_info(path, arcname)
                if path.is_file() and not path.is_symlink():
                    with path.open("rb") as source:
                        output.addfile(info, source)
                else:
                    output.addfile(info)
        with tar_path.open("rb") as source, temporary_path.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", compresslevel=9, mtime=0, fileobj=raw) as compressed:
                while chunk := source.read(1024 * 1024):
                    compressed.write(chunk)
        os.chmod(temporary_path, 0o644)
        os.replace(temporary_path, archive)
    except Exception as error:
        print(f"cannot create release archive: {error}", file=sys.stderr)
        temporary_path.unlink(missing_ok=True)
        return 1
    finally:
        tar_path.unlink(missing_ok=True)
    print(archive)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
