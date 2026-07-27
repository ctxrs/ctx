#!/usr/bin/env python3
"""Install one candidate output set without replacing any existing leaf."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import stat
import sys


class InstallError(ValueError):
    pass


def leaf(name: str) -> str:
    if not name or name in {".", ".."} or Path(name).name != name:
        raise InstallError(f"invalid candidate leaf: {name!r}")
    return name


def existing_kind(path: Path) -> str | None:
    try:
        mode = path.lstat().st_mode
    except FileNotFoundError:
        return None
    except OSError as error:
        raise InstallError(f"cannot inspect candidate output leaf: {path}") from error
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "regular file"
    if stat.S_ISDIR(mode):
        return "directory"
    return "nonregular file"


def validate_output_directory(path: Path) -> bool:
    created = False
    try:
        mode = path.lstat().st_mode
    except FileNotFoundError:
        path.mkdir(parents=True)
        created = True
        mode = path.lstat().st_mode
    except OSError as error:
        raise InstallError(f"cannot inspect candidate output directory: {path}") from error
    if stat.S_ISLNK(mode) or not stat.S_ISDIR(mode):
        raise InstallError(f"candidate output directory is not a real directory: {path}")
    return created


def validate_source(path: Path) -> int:
    try:
        mode = path.lstat().st_mode
    except OSError as error:
        raise InstallError(f"staged candidate leaf is unavailable: {path}") from error
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise InstallError(f"staged candidate leaf is not a regular file: {path}")
    return stat.S_IMODE(mode)


def install(
    stage: Path,
    output: Path,
    files: list[str],
    reserved: list[str],
    expected_sha256: list[str],
    check_only: bool,
) -> None:
    names = [leaf(name) for name in files]
    reserved_names = {leaf(name) for name in reserved}
    if not names or len(names) != len(set(names)):
        raise InstallError("candidate install file list is empty or duplicated")
    if not set(names).issubset(reserved_names):
        raise InstallError("every installed candidate leaf must be reserved")
    expected: dict[str, str] = {}
    for entry in expected_sha256:
        name, separator, digest = entry.partition("=")
        name = leaf(name)
        if (
            not separator
            or name in expected
            or name not in names
            or re.fullmatch(r"[0-9a-f]{64}", digest) is None
        ):
            raise InstallError(f"invalid candidate sha256 expectation: {entry!r}")
        expected[name] = digest

    created_output = validate_output_directory(output)
    try:
        for name in sorted(reserved_names):
            destination = output / name
            kind = existing_kind(destination)
            if kind is not None:
                raise InstallError(
                    f"candidate output leaf already exists ({kind}): {destination}"
                )
        if check_only:
            return
        modes = {name: validate_source(stage / name) for name in names}

        created: list[Path] = []
        try:
            for name in names:
                source = stage / name
                destination = output / name
                with source.open("rb") as input_file, destination.open("xb") as output_file:
                    created.append(destination)
                    digest = hashlib.sha256()
                    for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
                        output_file.write(chunk)
                        digest.update(chunk)
                    output_file.flush()
                    os.fsync(output_file.fileno())
                if name in expected and digest.hexdigest() != expected[name]:
                    raise InstallError(
                        f"staged candidate changed during installation: {source}"
                    )
                os.chmod(destination, modes[name])
        except BaseException:
            for path in reversed(created):
                try:
                    path.unlink()
                except OSError:
                    pass
            raise
    finally:
        if created_output:
            try:
                output.rmdir()
            except OSError:
                pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stage", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--file", action="append", dest="files", default=[])
    parser.add_argument("--reserve", action="append", default=[])
    parser.add_argument("--sha256", action="append", default=[])
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    try:
        install(
            args.stage,
            args.output,
            args.files,
            args.reserve,
            args.sha256,
            args.check_only,
        )
    except (InstallError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
