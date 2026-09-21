#!/usr/bin/env python3
"""Verify final Authenticode bytes using the existing pinned public inspector."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def verify(artifact: Path, output: Path):
    contract = json.loads((ROOT / "contracts/windows-authenticode-v1.json").read_bytes())
    jsign = contract["jsign"]
    cache = Path(os.environ.get("CTX_RELEASE_WORK_ROOT", ROOT / "target")) / "release-toolchain"
    cache.mkdir(parents=True, exist_ok=True)
    jar = Path(os.environ.get("CTX_WINDOWS_JSIGN_JAR", cache / f"jsign-{jsign['version']}.jar"))
    if not jar.exists():
        # Public checksum-pinned build tooling; no signing/service credential.
        if not jsign["url"].startswith("https://"):
            raise ValueError("Jsign source must use HTTPS")
        with urllib.request.urlopen(jsign["url"], timeout=60) as response:
            body = response.read(64 * 1024 * 1024 + 1)
        if len(body) > 64 * 1024 * 1024 or hashlib.sha256(body).hexdigest() != jsign["sha256"]:
            raise ValueError("Jsign download differs from public pin")
        with jar.open("xb") as stream:
            stream.write(body)
    if jar.is_symlink() or not jar.is_file() or hashlib.sha256(jar.read_bytes()).hexdigest() != jsign["sha256"]:
        raise ValueError("Jsign cache differs from public pin")
    if artifact.is_symlink() or not artifact.is_file() or not 0 < artifact.stat().st_size <= 128 * 1024 * 1024:
        raise ValueError("Windows release artifact exceeds incoming download bounds")
    if output.exists() or output.is_symlink():
        raise ValueError("signature verification output already exists")
    subprocess.run(["java", "--source", "11", "--class-path", str(jar),
        str(ROOT / "scripts/release/WindowsAuthenticodeInspect.java"), str(artifact), str(output),
        contract["authority"], contract["account"], contract["certificate_profile"],
        contract["code_signing_endpoint"], contract["expected_common_name"],
        contract["expected_organization"], jsign["sha256"], contract["timestamp_url"]], check=True)
    evidence = json.loads(output.read_bytes())
    output.write_text(json.dumps(evidence, sort_keys=True, separators=(",", ":")) + "\n")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    verify(args.artifact, args.output)
