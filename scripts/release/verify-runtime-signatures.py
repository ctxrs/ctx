#!/usr/bin/env python3
"""Check final macOS runtime signatures independently of execution coverage."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def verify(runtime: Path, authority: Path, policy: str):
    coverage = json.loads((authority / "release-validation.json").read_bytes())
    if coverage["validation_policy"] != policy:
        raise ValueError("assembly and staged validation selections differ")
    environment = dict(os.environ, CTX_MACOS_RELEASE_SOURCE_COMMIT=coverage["source_commit"])
    with tempfile.TemporaryDirectory(prefix="ctx-runtime-signatures-", dir=runtime.parent) as work:
        for platform in ("macos-arm64", "macos-x64"):
            stem = f"ctx-onnxruntime-{platform}"
            archive = runtime / f"{stem}.tar.gz"
            nested = Path(work) / platform / "libonnxruntime.dylib"
            nested.parent.mkdir()
            with tarfile.open(archive, "r:gz") as bundle:
                members = [m for m in bundle.getmembers() if m.name == "lib/libonnxruntime.dylib"]
                if len(members) != 1 or not members[0].isfile() or not 0 < members[0].size <= 256 * 1024 * 1024:
                    raise ValueError("runtime archive lacks one bounded regular dylib")
                with bundle.extractfile(members[0]) as source, nested.open("xb") as output:
                    shutil.copyfileobj(source, output)
            subprocess.run(["python3", str(ROOT / "scripts/macos-release-signing-evidence.py"),
                "verify-archive", "--evidence", str(runtime / f"{stem}.signing.json"),
                "--platform", platform, "--archive", str(archive), "--checksum", str(archive) + ".sha256",
                "--nested-artifact", str(nested), "--role", "release"], check=True, env=environment)
            subprocess.run([str(ROOT / "scripts/verify-macos-release-attestation.sh"),
                "--runtime-archive", platform, str(archive), str(nested),
                str(runtime / f"{stem}.release-attestation.json"),
                str(runtime / f"{stem}.release-attestation.cms")], check=True, env=environment)

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime-dir", type=Path, required=True)
    parser.add_argument("--authority-dir", type=Path, required=True)
    parser.add_argument("--policy", required=True)
    args = parser.parse_args()
    verify(args.runtime_dir, args.authority_dir, args.policy)
