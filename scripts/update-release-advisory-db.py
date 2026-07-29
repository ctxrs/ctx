#!/usr/bin/env python3
"""Download the OSV ecosystem snapshots used by the offline release gate."""

from __future__ import annotations

import argparse
from datetime import UTC, datetime
from email.utils import parsedate_to_datetime
import hashlib
import json
import os
from pathlib import Path
import tempfile
import urllib.request
import zipfile


SOURCES = {
    "crates.io": "https://osv-vulnerabilities.storage.googleapis.com/crates.io/all.zip",
    "npm": "https://osv-vulnerabilities.storage.googleapis.com/npm/all.zip",
}


def download(ecosystem: str, destination: Path) -> dict[str, object]:
    request = urllib.request.Request(
        SOURCES[ecosystem], headers={"User-Agent": "ctx-release-advisory-db/1"}
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    size = 0
    with urllib.request.urlopen(request, timeout=120) as response:
        modified_header = response.headers.get("Last-Modified")
        generation = response.headers.get("x-goog-generation")
        if not modified_header or not generation:
            raise SystemExit(f"OSV response lacks source metadata: {ecosystem}")
        modified = parsedate_to_datetime(modified_header).astimezone(UTC)
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
            temporary = Path(output.name)
            while block := response.read(1024 * 1024):
                output.write(block)
                digest.update(block)
                size += len(block)
    try:
        with zipfile.ZipFile(temporary) as archive:
            names = archive.namelist()
            if not names or any(name.startswith("/") or ".." in Path(name).parts for name in names):
                raise SystemExit(f"OSV archive is malformed: {ecosystem}")
            if not any(name.endswith(".json") for name in names):
                raise SystemExit(f"OSV archive contains no advisories: {ecosystem}")
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
    return {
        "ecosystem": ecosystem,
        "path": f"osv-scanner/{ecosystem}/all.zip",
        "sha256": digest.hexdigest(),
        "size": size,
        "source_generation": generation,
        "source_last_modified": modified.isoformat().replace("+00:00", "Z"),
        "source_url": SOURCES[ecosystem],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--database-root", type=Path, required=True)
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument(
        "--ecosystem",
        action="append",
        choices=sorted(SOURCES),
        required=True,
    )
    args = parser.parse_args()
    root = args.database_root.resolve()
    records = [
        download(ecosystem, root / f"osv-scanner/{ecosystem}/all.zip")
        for ecosystem in sorted(set(args.ecosystem))
    ]
    metadata = {
        "schema_version": 1,
        "fetched_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "databases": records,
    }
    args.metadata.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=args.metadata.parent, delete=False
    ) as output:
        output.write(payload)
        temporary = Path(output.name)
    os.replace(temporary, args.metadata)
    print(args.metadata)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
