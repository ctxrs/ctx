#!/usr/bin/env python3
"""Create and safely extract the fixed ONNX Runtime sidecar archive shape."""

import argparse
import datetime
import posixpath
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path


DOCUMENTS = ("LICENSE", "ThirdPartyNotices.txt", "VERSION_NUMBER", "GIT_COMMIT_ID")
WINDOWS_FILES = (
    "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
    "lib/msvcp140.dll",
    "lib/msvcp140_1.dll",
    "lib/vcruntime140.dll",
    "lib/vcruntime140_1.dll",
)


def archive_files(library: str) -> tuple[str, ...]:
    files = DOCUMENTS + (f"lib/{library}",)
    if library == "onnxruntime.dll":
        files += WINDOWS_FILES
    return files


def write_tar(
    source: Path, output: Path, library: str, source_date_epoch: int
) -> None:
    with tarfile.open(output, "w", format=tarfile.USTAR_FORMAT) as bundle:
        directory = tarfile.TarInfo("lib/")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        directory.uid = directory.gid = 0
        directory.uname = directory.gname = "root"
        directory.mtime = source_date_epoch
        bundle.addfile(directory)
        for name in archive_files(library):
            path = source.joinpath(*name.split("/"))
            info = tarfile.TarInfo(name)
            info.size = path.stat().st_size
            info.mode = 0o755 if name.startswith("lib/") else 0o644
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mtime = source_date_epoch
            with path.open("rb") as handle:
                bundle.addfile(info, handle)


def zip_info(name: str, mode: int, timestamp: tuple[int, ...]) -> zipfile.ZipInfo:
    entry = zipfile.ZipInfo(name, timestamp)
    entry.create_system = 3
    entry.compress_type = zipfile.ZIP_DEFLATED
    entry.external_attr = mode << 16
    return entry


def write_zip(
    source: Path, output: Path, library: str, source_date_epoch: int
) -> None:
    timestamp = datetime.datetime.fromtimestamp(
        source_date_epoch, datetime.timezone.utc
    ).timetuple()[:6]
    with zipfile.ZipFile(
        output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
    ) as bundle:
        bundle.writestr(zip_info("lib/", 0o40755, timestamp), b"")
        for name in archive_files(library):
            path = source.joinpath(*name.split("/"))
            mode = 0o100755 if name.startswith("lib/") else 0o100644
            bundle.writestr(zip_info(name, mode, timestamp), path.read_bytes())


def create_archive(
    kind: str,
    source: Path,
    output: Path,
    library: str,
    source_date_epoch: int,
) -> None:
    if kind == "tar.zst":
        tar_path = Path(f"{output}.tar")
        write_tar(source, tar_path, library, source_date_epoch)
        subprocess.run(
            ["zstd", "-q", "-19", "--threads=0", "-f", str(tar_path), "-o", str(output)],
            check=True,
        )
        tar_path.unlink()
    elif kind == "zip":
        write_zip(source, output, library, source_date_epoch)
    else:
        raise SystemExit(f"unsupported sidecar archive kind: {kind}")


def canonical_name(raw: str) -> str:
    if not raw or "\\" in raw or raw.startswith("/"):
        raise SystemExit(f"unsafe sidecar archive path: {raw!r}")
    while raw.startswith("./"):
        raw = raw[2:]
    name = posixpath.normpath(raw.rstrip("/"))
    if name in ("", ".", "..") or name.startswith("../"):
        raise SystemExit(f"unsafe sidecar archive path: {raw!r}")
    return name


def extract_checked(kind: str, archive: Path, destination: Path, library: str) -> None:
    expected_files = set(archive_files(library))
    expected_entries = expected_files | {"lib"}
    destination.joinpath("lib").mkdir(parents=True, exist_ok=True)
    seen: set[str] = set()

    if kind == "tar.zst":
        with tarfile.open(archive, "r:") as bundle:
            members: dict[str, tarfile.TarInfo] = {}
            for member in bundle.getmembers():
                name = canonical_name(member.name)
                if name in seen:
                    raise SystemExit(f"duplicate sidecar archive entry: {name}")
                seen.add(name)
                if name not in expected_entries:
                    raise SystemExit(f"unexpected sidecar archive entry: {name}")
                if member.mode & 0o7000:
                    raise SystemExit(
                        f"unsafe permission bits on sidecar archive entry: {name}"
                    )
                if name == "lib":
                    if not member.isdir():
                        raise SystemExit("sidecar lib entry is not a directory")
                elif not member.isfile():
                    raise SystemExit(f"sidecar entry is not a regular file: {name}")
                members[name] = member
            if seen != expected_entries:
                missing = sorted(expected_entries - seen)
                raise SystemExit(
                    "sidecar archive entries missing: " + ", ".join(missing)
                )
            for name in expected_files:
                source_file = bundle.extractfile(members[name])
                if source_file is None:
                    raise SystemExit(f"could not read sidecar archive member: {name}")
                target = destination.joinpath(*name.split("/"))
                with source_file, target.open("wb") as output:
                    shutil.copyfileobj(source_file, output)
    elif kind == "zip":
        with zipfile.ZipFile(archive) as bundle:
            members: dict[str, zipfile.ZipInfo] = {}
            for member in bundle.infolist():
                name = canonical_name(member.filename)
                if name in seen:
                    raise SystemExit(f"duplicate sidecar archive entry: {name}")
                seen.add(name)
                if name not in expected_entries:
                    raise SystemExit(f"unexpected sidecar archive entry: {name}")
                if member.flag_bits & 1:
                    raise SystemExit(f"encrypted sidecar archive entry: {name}")
                mode = member.external_attr >> 16
                if stat.S_ISLNK(mode):
                    raise SystemExit(f"sidecar zip contains a symbolic link: {name}")
                if mode & 0o7000:
                    raise SystemExit(
                        f"unsafe permission bits on sidecar archive entry: {name}"
                    )
                if name == "lib":
                    if not member.is_dir():
                        raise SystemExit("sidecar lib entry is not a directory")
                elif member.is_dir():
                    raise SystemExit(f"sidecar entry is not a regular file: {name}")
                members[name] = member
            if seen != expected_entries:
                missing = sorted(expected_entries - seen)
                raise SystemExit(
                    "sidecar archive entries missing: " + ", ".join(missing)
                )
            for name in expected_files:
                target = destination.joinpath(*name.split("/"))
                with bundle.open(members[name]) as source_file, target.open("wb") as output:
                    shutil.copyfileobj(source_file, output)
    else:
        raise SystemExit(f"unsupported sidecar archive kind: {kind}")


def extract_archive(
    kind: str,
    archive: Path,
    destination: Path,
    library: str,
    work_dir: Path | None = None,
) -> None:
    if kind == "zip":
        extract_checked(kind, archive, destination, library)
        return
    if kind != "tar.zst":
        raise SystemExit(f"unsupported sidecar archive kind: {kind}")

    subprocess.run(["zstd", "-q", "-t", str(archive)], check=True)
    if work_dir is not None:
        inspection_archive = work_dir / "validated.tar"
        subprocess.run(
            ["zstd", "-q", "-d", "-f", str(archive), "-o", str(inspection_archive)],
            check=True,
        )
        extract_checked(kind, inspection_archive, destination, library)
        return

    with tempfile.TemporaryDirectory(prefix="ctx-onnxruntime-archive.") as temporary:
        inspection_archive = Path(temporary) / "validated.tar"
        subprocess.run(
            ["zstd", "-q", "-d", "-f", str(archive), "-o", str(inspection_archive)],
            check=True,
        )
        extract_checked(kind, inspection_archive, destination, library)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    create = subparsers.add_parser("create")
    create.add_argument("--kind", choices=("tar.zst", "zip"), required=True)
    create.add_argument("--library", required=True)
    create.add_argument("--source", type=Path, required=True)
    create.add_argument("--output", type=Path, required=True)
    create.add_argument("--source-date-epoch", type=int, required=True)

    extract = subparsers.add_parser("extract")
    extract.add_argument("--kind", choices=("tar.zst", "zip"), required=True)
    extract.add_argument("--library", required=True)
    extract.add_argument("--archive", type=Path, required=True)
    extract.add_argument("--destination", type=Path, required=True)
    extract.add_argument("--work-dir", type=Path)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.command == "create":
        create_archive(
            args.kind,
            args.source,
            args.output,
            args.library,
            args.source_date_epoch,
        )
    else:
        extract_archive(
            args.kind, args.archive, args.destination, args.library, args.work_dir
        )


if __name__ == "__main__":
    main()
