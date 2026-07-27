#!/usr/bin/env python3
"""Adversarial tests for the Linux release network-isolation gate."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: test.py CHECKER FIXTURE_MATRIX")
    checker = Path(sys.argv[1]).resolve()
    matrix_path = Path(sys.argv[2]).resolve()
    with matrix_path.open(encoding="utf-8") as source:
        matrix = json.load(source)
    assert matrix["schema_version"] == 1
    cases = matrix["cases"]
    assert isinstance(cases, list) and cases

    accepted = 0
    rejected = 0
    with tempfile.TemporaryDirectory(prefix="ctx-release-network-test.") as temporary:
        root = Path(temporary)
        for case in cases:
            name = case["name"]
            fixture = root / f"{name}.json"
            fixture.write_text(
                json.dumps(case["state"], sort_keys=True, separators=(",", ":")),
                encoding="utf-8",
            )
            result = subprocess.run(
                [sys.executable, str(checker), "--fixture", str(fixture)],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if case["accepted"]:
                assert result.returncode == 0, (name, result.stdout, result.stderr)
                assert "offline release network isolation ok" in result.stdout
                accepted += 1
            else:
                assert result.returncode == 1, (name, result.stdout, result.stderr)
                assert case["error"] in result.stderr, (name, result.stderr)
                assert "offline release network isolation failed" in result.stderr
                rejected += 1

    assert accepted >= 2
    assert rejected >= 10
    print(
        "Linux release network-isolation tests passed: "
        f"accepted={accepted} rejected={rejected}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
