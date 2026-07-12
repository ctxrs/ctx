#!/usr/bin/env python3
"""Verify a semantic Core ML bundle without loading or compiling its models."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path

from bundle_format import MANIFEST_NAME, verify_bundle


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    args = parser.parse_args()
    manifest = verify_bundle(args.bundle)
    digest = hashlib.sha256((args.bundle / MANIFEST_NAME).read_bytes()).hexdigest()
    print(f"verified bundle_id={manifest['bundle_id']}")
    print(f"bundle_version={manifest['bundle_version']}")
    print(f"manifest_sha256={digest}")


if __name__ == "__main__":
    main()
