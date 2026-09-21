#!/usr/bin/env python3
"""Author static verifier fixtures; retain no private key or executable code."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess
import re

ROOT = Path(__file__).resolve().parents[3]
OUT = Path(__file__).with_name("fixtures") / "unified"
canonical = lambda value: json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
sha = lambda value: hashlib.sha256(value).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--qualification-binary", type=Path)
    parser.add_argument("--build-info", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--rollback-generation", type=int, default=27)
    args = parser.parse_args()
    if any((args.qualification_binary, args.build_info, args.output_dir)) and not all(
            (args.qualification_binary, args.build_info, args.output_dir)):
        parser.error("qualification requires binary, actual build-info and new output directory")
    if not 1 <= args.rollback_generation <= 2**53 - 1:
        parser.error("rollback generation is out of range")
    output = args.output_dir or OUT
    source = "1" * 40
    fingerprint = sha(b"authored public candidate")
    release_name = "v1.5.0"
    artifact = b"authored unified ctx 1.5.0 verifier fixture; not an executable\n"
    qualification = None
    if args.qualification_binary:
        binary, info = args.qualification_binary, args.build_info
        for path, limit in ((binary, 128 * 1024 * 1024), (info, 64 * 1024)):
            if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= limit:
                parser.error("qualification input must be a bounded regular non-symlink file")
        artifact = binary.read_bytes()
        build_bytes = info.read_bytes()
        build = json.loads(build_bytes)
        source = build.get("source", {}).get("commit", "")
        if (re.fullmatch(r"[0-9a-f]{40}", source) is None or source == "0" * 40
                or build.get("source", {}).get("clean") is not True
                or build.get("artifact_sha256") != sha(artifact)
                or build.get("target") != "x86_64-pc-windows-gnu"
                or not artifact.startswith(b"MZ")):
            parser.error("qualification build-info does not bind exact clean Windows candidate bytes")
        version = build.get("version", "")
        if re.fullmatch(r"1\.(?:[5-9]|[1-9][0-9]+)\.[0-9]+", version) is None:
            parser.error("qualification candidate must be unified 1.5 or newer")
        release_name = "v" + version
        fingerprint = sha(build_bytes)
        qualification = {"kind": "ctx-managed-pair-qualification-fixture", "schema_version": 1,
            "source_commit": source, "artifact_sha256": sha(artifact), "artifact_size_bytes": len(artifact),
            "build_info_sha256": fingerprint, "native_target": "windows-x64",
            "trust_input": "CTX_RELEASE_MANAGED_PAIR_AUTHORITY_JSON",
            "required_build_cfg": "ctx_release_qualification", "source_binding": "provided-build-info",
            "native_execution": "not_run", "signed_release_identity": False, "stock_1_4_proof": False}
        output.mkdir(parents=True, exist_ok=False)
        (output / "candidate-build-info.json").write_bytes(build_bytes)
    else:
        output.mkdir(parents=True, exist_ok=True)
    matrix_bytes = (ROOT / "contracts/release-targets-v1.json").read_bytes()
    matrix = json.loads(matrix_bytes)
    private = subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt",
                              "rsa_keygen_bits:2048"], check=True, capture_output=True).stdout
    public = subprocess.run(["openssl", "rsa", "-RSAPublicKey_out"], input=private,
                            check=True, capture_output=True).stdout
    der = subprocess.run(["openssl", "rsa", "-RSAPublicKey_out", "-outform", "DER"],
                         input=private, check=True, capture_output=True).stdout
    (output / ("ctx.exe" if qualification else "artifact.txt")).write_bytes(artifact)
    authority = {"contract": "ctx-managed-pair-release-authority", "schema_version": 1,
                 "channels": [{"id": channel, "key_id": f"fixture-{channel}",
                     "signature_algorithm": "rsa-pkcs1v15-sha256", "public_key_der_sha256": sha(der),
                     "public_key_pem": public.decode()} for channel in ("stable", "staging")]}
    (output / "authority.json").write_bytes(canonical(authority) + b"\n")
    # Openssl receives the ephemeral key through a pipe; no key file is retained.
    import os
    for target in matrix["targets"]:
        components = {}
        for role in ("core", "companion"):
            name = target["public_artifact" if role == "core" else "helper_artifact"]
            components[role] = {"artifact_name": name, "object_key": f"sha256/{sha(artifact)}/{name}",
                "sha256": sha(artifact), "size_bytes": len(artifact),
                "install_slot": "<install-root>/" + target[f"managed_pair_{role}_slot"],
                "build_identity": {"component": role, "rust_target": target["public_rust_target"],
                    "source_revision": source, "build_fingerprint": fingerprint}}
        payload = {"contract": "ctx-managed-pair-manifest", "schema_version": 1, "channel": "stable",
            "release_authority_key_id": "fixture-stable", "release_name": release_name,
            "target": {"id": target["id"], "os": target["os"], "arch": target["arch"],
                "core_rust_target": target["public_rust_target"], "companion_rust_target": target["public_rust_target"]},
            "install_geometry": {"install_root": "<install-root>", "managed_bin_dir": "<install-root>/bin",
                "core_slot": "<install-root>/" + target["managed_pair_core_slot"],
                "companion_slot": "<install-root>/" + target["managed_pair_companion_slot"]},
            "target_matrix_sha256": sha(matrix_bytes), "rollback_generation": args.rollback_generation,
            "snapshot": {"contract": "ctx-managed-pair-snapshot-v1", "fingerprint": sha(canonical({"source": source, "build": fingerprint, "artifact": sha(artifact)}))},
            "compatibility": {"invocation_fingerprint": sha(matrix_bytes),
                "core_capability_fingerprint": "4be5325aa95a6fdd22e59340abfbadefd8b73ffd2a1e3f55ea75deef9e956e34"},
            "components": components}
        raw = canonical(payload)
        read_fd, write_fd = os.pipe()
        try:
            os.write(write_fd, private)
            os.close(write_fd)
            write_fd = None
            signature = subprocess.run(["openssl", "dgst", "-sha256", "-sign", f"/dev/fd/{read_fd}"],
                input=raw, pass_fds=(read_fd,), check=True, capture_output=True).stdout
        finally:
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)
        envelope = {"schema_version": 1,
            "manifest_base64": base64.b64encode(raw).decode(), "signature_base64": base64.b64encode(signature).decode()}
        (output / f"{target['id']}.json").write_bytes(canonical(envelope) + b"\n")
    if qualification:
        authority_bytes = (output / "authority.json").read_bytes()
        if not 0 < len(authority_bytes) <= 16 * 1024:
            raise ValueError("qualification public authority exceeds the verifier bound")
        qualification["authority_sha256"] = sha(authority_bytes)
        qualification["envelope_sha256"] = sha((output / "windows-x64.json").read_bytes())
        (output / "qualification-fixture.json").write_bytes(canonical(qualification) + b"\n")
    print(f"wrote public-key-only envelope fixtures: {output}")

if __name__ == "__main__":
    main()
