"""Apply the same pinned notify correction used by Bazel to Cargo builds.

Remove this override and the Bazel annotation when notify releases the fix.
The manifest and dependency graph stay identical to the published package.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile


VERSION = "9.0.0-rc.4"
ARCHIVE_SHA256 = "b44b771d4dd781ef14c84078693e67495da6b47f609f72e8a4da8420a861240e"
MANIFEST_SHA256 = "9a733ea62172ee27f0b0813c47c05960f458f28c5c1df580d235137756d16966"
FSEVENT_SHA256 = "73852fe742a23a9c47504113766e4d0d919bcda97e35cfc7779b5843a32a4600"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
PATCH = Path(__file__).resolve().parents[2] / "tools/bazel/patches/notify-fsevents-stop.patch"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify(source: Path) -> None:
    for name, expected in (("Cargo.toml", MANIFEST_SHA256), ("src/fsevent.rs", FSEVENT_SHA256)):
        if digest(source / name) != expected:
            raise ValueError(f"patched notify source mismatch: {name}")
    if not (source / "NOTICE.ctx-patch").is_file():
        raise ValueError("patched notify notice missing")


def prepare(archive: Path, output: Path) -> Path:
    if digest(archive) != ARCHIVE_SHA256:
        raise ValueError("notify archive checksum mismatch")
    output.mkdir()  # A fresh task-owned directory, never a shared Cargo cache.
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle.getmembers():
            path = Path(member.name)
            if (not member.isfile() or path.is_absolute() or ".." in path.parts
                    or path.parts[0] != f"notify-{VERSION}"):
                raise ValueError("unexpected notify archive member")
        bundle.extractall(output, filter="data")
    source = output / f"notify-{VERSION}"
    subprocess.run(
        ["patch", "--batch", "--fuzz=0", "-p1", "-i", str(PATCH)],
        cwd=source, check=True, stdout=subprocess.DEVNULL,
    )
    (source / "NOTICE.ctx-patch").write_text(
        f"ctx applies a local correction to notify {VERSION}: retry the macOS\n"
        "runloop stop during shutdown so a stop sent before runloop entry is not lost.\n"
        f"Upstream archive SHA-256: {ARCHIVE_SHA256}\n"
        f"Patch: tools/bazel/patches/{PATCH.name}\n"
        f"Patch SHA-256: {digest(PATCH)}\n"
        f"Patched src/fsevent.rs SHA-256: {FSEVENT_SHA256}\n",
        encoding="utf-8",
    )
    verify(source)
    (output / "config.toml").write_text(
        "paths = " + json.dumps([str(source.resolve())]) + "\n", encoding="utf-8"
    )
    return source


def bind_metadata(metadata: dict, source: Path) -> None:
    """Keep the upstream advisory identity, but require the actual patched path."""
    verify(source)
    packages = [package for package in metadata["packages"] if package["name"] == "notify"]
    if (len(packages) != 1 or packages[0]["version"] != VERSION
            or packages[0]["source"] is not None
            or Path(packages[0]["manifest_path"]).resolve() != (source / "Cargo.toml").resolve()):
        raise ValueError("Cargo did not select the patched notify source")
    # Source here identifies the upstream package, not unmodified source bytes.
    # The staged notice and inventory patch record bind the local correction.
    packages[0]["source"] = REGISTRY
    metadata["source_patches"] = [{
        "name": "notify", "version": VERSION, "archive_sha256": ARCHIVE_SHA256,
        "patch_sha256": digest(PATCH), "fsevent_sha256": FSEVENT_SHA256,
    }]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    print(prepare(arguments.archive, arguments.output))
