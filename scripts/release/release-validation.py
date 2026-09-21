#!/usr/bin/env python3
"""Bind executed CI and explicitly selected native checks to release bytes."""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import re

POLICIES = ("native-receipts-required-v1", "factory-only-human-override-v1")
PLATFORMS = ("linux-x64", "linux-aarch64", "macos-arm64", "macos-x64", "windows-x64")


def record(path: Path):
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 16 * 1024 * 1024:
        raise ValueError(f"invalid release evidence: {path}")
    raw = path.read_bytes()
    return {"file": path.name, "sha256": hashlib.sha256(raw).hexdigest(), "size_bytes": len(raw)}


def normal_ci(path: Path, source: str):
    receipt = record(path)
    value = json.loads(path.read_bytes())
    if (set(value) != {"kind", "schema_version", "mode", "source_commit", "status"}
            or value["kind"] != "ctx-normal-ci-result" or value["schema_version"] != 1
            or value["mode"] != "ci" or value["status"] != "passed"
            or value["source_commit"] != source):
        raise ValueError("release requires passed normal CI for its exact public source")
    return receipt


def coverage(policy: str, source: str, ci: Path, proof_dir: Path):
    if policy not in POLICIES or re.fullmatch(r"[0-9a-f]{40}", source) is None or source == "0" * 40:
        raise ValueError("invalid release validation selection or source")
    native = {}
    for platform in PLATFORMS:
        if policy == POLICIES[0]:
            native[platform] = {"status": "passed", "receipt": record(
                proof_dir / platform / f"ctx-{platform}.native-execution.json")}
        else:
            native[platform] = {"status": "not_run"}
    return {"kind": "ctx-release-validation", "schema_version": 1,
            "source_commit": source, "validation_policy": policy,
            "normal_ci": {"status": "passed", "receipt": normal_ci(ci, source)},
            "nightly": "not_run", "release_tier": "not_run", "native_execution": native}


def validate_coverage(value, source):
    if (not isinstance(value, dict) or set(value) != {"kind", "schema_version", "source_commit",
            "validation_policy", "normal_ci", "nightly", "release_tier", "native_execution"}
            or value["kind"] != "ctx-release-validation" or value["schema_version"] != 1
            or value["source_commit"] != source or value["validation_policy"] not in POLICIES
            or value["nightly"] != "not_run" or value["release_tier"] != "not_run"
            or set(value["native_execution"]) != set(PLATFORMS)):
        raise ValueError("release validation does not describe the selected construction")
    expected = "passed" if value["validation_policy"] == POLICIES[0] else "not_run"
    for item in value["native_execution"].values():
        if item.get("status") != expected or set(item) != ({"status", "receipt"} if expected == "passed" else {"status"}):
            raise ValueError("native coverage differs from executed selection")
    if set(value["normal_ci"]) != {"status", "receipt"} or value["normal_ci"]["status"] != "passed":
        raise ValueError("normal CI is not passing")
    for receipt in [value["normal_ci"]["receipt"], *[i["receipt"] for i in value["native_execution"].values() if "receipt" in i]]:
        if (set(receipt) != {"file", "sha256", "size_bytes"}
                or not isinstance(receipt["file"], str) or Path(receipt["file"]).name != receipt["file"]
                or re.fullmatch(r"[0-9a-f]{64}", str(receipt["sha256"])) is None
                or type(receipt["size_bytes"]) is not int or receipt["size_bytes"] <= 0):
            raise ValueError("invalid validation receipt binding")
    return value


def verify_handoff(handoff, handoff_document, source_commit, factory, read_canonical_json,
                   require_document_record):
    sha256_bytes = lambda value: hashlib.sha256(value).hexdigest()
    MAX_FACTORY_JSON_BYTES = 16 * 1024 * 1024
    validation, validation_bytes = read_canonical_json(
        handoff / "release-validation.json", "release validation", MAX_FACTORY_JSON_BYTES
    )
    require_document_record(handoff_document.get("validation"), {
        "file": "release-validation.json", "sha256": sha256_bytes(validation_bytes),
        "size_bytes": len(validation_bytes),
    }, "release validation")
    validate_coverage(validation, source_commit)
    ci_record = normal_ci(handoff / "normal-ci.json", source_commit)
    require_document_record(validation["normal_ci"]["receipt"], ci_record, "normal CI")
    signature, signature_bytes = read_canonical_json(
        handoff / "windows-authenticode.json", "Windows signature", MAX_FACTORY_JSON_BYTES
    )
    require_document_record(handoff_document.get("windows_signature"), {
        "file": "windows-authenticode.json", "sha256": sha256_bytes(signature_bytes),
        "size_bytes": len(signature_bytes),
    }, "Windows signature")
    if (signature.get("kind") != "ctx-windows-authenticode-signing"
            or signature.get("schema_version") != 1
            or signature.get("artifact_sha256") != factory["ctx.exe"]["sha256"]
            or signature.get("artifact_size") != factory["ctx.exe"]["size_bytes"]):
        raise ValueError("Windows signature evidence differs from the final executable")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--policy", choices=POLICIES, required=True)
    parser.add_argument("--ci-receipt", type=Path, required=True)
    parser.add_argument("--native-proof-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    value = coverage(args.policy, args.source_commit, args.ci_receipt, args.native_proof_dir)
    validate_coverage(value, args.source_commit)
    with args.output.open("x", encoding="utf-8") as output:
        output.write(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")

if __name__ == "__main__":
    main()
