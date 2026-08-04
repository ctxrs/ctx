#!/usr/bin/env python3
"""Validate the deterministic TypeScript SDK package surface without npm."""

from __future__ import annotations

import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_ROOT = ROOT / "sdks" / "typescript"
EXPECTED_RUNTIME_FILES = {
    "index.d.ts",
    "index.js",
    "subprocess.js",
    "windows-job-launcher.ps1",
}


def fail(message: str) -> None:
    raise SystemExit(f"TypeScript package contents check failed: {message}")


def main() -> None:
    package = json.loads((PACKAGE_ROOT / "package.json").read_text(encoding="utf-8"))
    if package.get("private") is not True:
        fail("package.json must remain private")
    if package.get("files") != ["src"]:
        fail('package.json files must be exactly ["src"]')

    source_root = PACKAGE_ROOT / "src"
    actual = {
        path.relative_to(source_root).as_posix()
        for path in source_root.rglob("*")
        if path.is_file()
    }
    if actual != EXPECTED_RUNTIME_FILES:
        fail(
            "src package manifest drifted: "
            f"expected={sorted(EXPECTED_RUNTIME_FILES)} actual={sorted(actual)}"
        )

    exports = package.get("exports", {}).get(".", {})
    for field in ("types", "default"):
        relative = exports.get(field)
        if not isinstance(relative, str) or not relative.startswith("./src/"):
            fail(f"exports.{field} must point inside ./src")
        if not (PACKAGE_ROOT / relative.removeprefix("./")).is_file():
            fail(f"exports.{field} target is missing: {relative}")

    subprocess_source = (source_root / "subprocess.js").read_text(encoding="utf-8")
    if 'new URL("./windows-job-launcher.ps1", import.meta.url)' not in subprocess_source:
        fail("Windows launcher is not resolved from the packaged module")

    print("TypeScript package contents check passed")


if __name__ == "__main__":
    main()
