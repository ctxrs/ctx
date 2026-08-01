#!/usr/bin/env python3
"""Create and verify macOS standalone release signing evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

EXPECTED_TEAM_ID = "SJSNARH4TG"


def is_expected_authority(value: object) -> bool:
    if not isinstance(value, str) or "\n" in value or "\r" in value:
        return False
    prefix = "Developer ID Application: "
    suffix = f" ({EXPECTED_TEAM_ID})"
    return (
        value.startswith(prefix)
        and value.endswith(suffix)
        and len(value) > len(prefix + suffix)
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"invalid JSON evidence {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"JSON evidence must be an object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_text(payload, encoding="utf-8")
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def read_single_line(path: Path) -> str:
    value = path.read_text(encoding="utf-8").strip()
    if not re.fullmatch(r"[0-9a-fA-F]{64}", value):
        raise SystemExit(f"checksum sidecar is not a SHA-256 digest: {path}")
    return value.lower()


def detail_value(details: str, name: str) -> str:
    match = re.search(rf"^{re.escape(name)}=(.+)$", details, re.MULTILINE)
    if not match:
        raise SystemExit(f"codesign details are missing {name}")
    return match.group(1).strip()


def code_directory_flags(details: str) -> set[str]:
    flags: set[str] = set()
    for match in re.finditer(
        r"^CodeDirectory\s+.*?\bflags=[^\s()]*(?:\(([^)]*)\))?",
        details,
        re.MULTILINE,
    ):
        if match.group(1):
            flags.update(value.strip() for value in match.group(1).split(","))
    return flags


def require_base_document(
    document: dict[str, Any],
    platform: str,
    kind: str,
    *,
    allow_pending_cli: bool = False,
) -> dict[str, Any]:
    if document.get("schema_version") != 2:
        raise SystemExit("unsupported macOS signing evidence schema")
    if document.get("platform") != platform:
        raise SystemExit(
            f"signing evidence platform mismatch: expected {platform}, "
            f"got {document.get('platform')!r}"
        )
    if document.get("artifact_kind") != kind:
        raise SystemExit(
            f"signing evidence kind mismatch: expected {kind}, "
            f"got {document.get('artifact_kind')!r}"
        )
    signing = document.get("codesign")
    notarization = document.get("notarization")
    verification = document.get("artifact_verification")
    if not isinstance(signing, dict) or signing.get("verified") is not True:
        raise SystemExit("signing evidence does not record strict codesign verification")
    if not is_expected_authority(signing.get("authority")):
        raise SystemExit("signing evidence does not record the pinned ctx Apple authority")
    if signing.get("team_identifier") != EXPECTED_TEAM_ID:
        raise SystemExit("signing evidence does not record the pinned ctx Apple Team ID")
    if signing.get("hardened_runtime") is not True:
        raise SystemExit("signing evidence does not record hardened runtime")
    if signing.get("secure_timestamp") is not True:
        raise SystemExit("signing evidence does not record a secure timestamp")
    if not isinstance(notarization, dict) or notarization.get("status") != "Accepted":
        raise SystemExit("signing evidence does not record accepted notarization")
    if not notarization.get("submission_id"):
        raise SystemExit("signing evidence is missing the notarization submission id")
    if not re.fullmatch(r"[0-9a-f]{64}", str(notarization.get("submit_sha256", ""))):
        raise SystemExit("signing evidence is missing the exact notary response hash")
    expected_method = {
        "cli": "signed-exact-byte-version-execution",
        "runtime": "accepted-notary-strict-codesign-attestation",
    }[kind]
    if allow_pending_cli and kind == "cli":
        if verification != {
            "method": "signed-exact-byte-version-execution",
            "status": "pending",
        }:
            raise SystemExit("CLI signing evidence has invalid pending verification state")
    elif not isinstance(verification, dict) or verification != {
        "method": expected_method,
        "status": "passed",
    }:
        raise SystemExit(
            f"signing evidence does not record passed {expected_method} verification"
        )
    return document


def accepted_notary_fields(path: Path) -> dict[str, str]:
    submit = read_json(path)
    if submit.get("status") != "Accepted":
        raise SystemExit(
            f"notarization status is not Accepted: {submit.get('status', 'missing')}"
        )
    submission_id = submit.get("id")
    if not isinstance(submission_id, str) or not submission_id:
        raise SystemExit("accepted notarization response is missing an id")
    return {
        "notarization_status": "Accepted",
        "notarization_submission_id": submission_id,
        "notary_submit_sha256": sha256(path),
    }


def command_write(args: argparse.Namespace) -> None:
    details = args.codesign_details.read_text(encoding="utf-8")
    authority = detail_value(details, "Authority")
    identifier = detail_value(details, "Identifier")
    team_identifier = detail_value(details, "TeamIdentifier")
    if not is_expected_authority(authority):
        raise SystemExit(f"unexpected codesign authority: {authority}")
    if team_identifier != EXPECTED_TEAM_ID:
        raise SystemExit(f"unexpected codesign TeamIdentifier: {team_identifier}")
    if "runtime" not in code_directory_flags(details):
        raise SystemExit(
            "codesign details do not contain runtime in CodeDirectory flags"
        )
    if not re.search(r"^Timestamp=.+$", details, re.MULTILINE):
        raise SystemExit("codesign details do not contain a secure timestamp")

    notary = accepted_notary_fields(args.notary_submit)

    document = {
        "artifact_kind": args.kind,
        "artifact_name": args.artifact.name,
        "artifact_sha256": sha256(args.artifact),
        "codesign": {
            "authority": authority,
            "hardened_runtime": True,
            "identifier": identifier,
            "secure_timestamp": True,
            "team_identifier": team_identifier,
            "verified": True,
        },
        "artifact_verification": {
            "method": (
                "signed-exact-byte-version-execution"
                if args.kind == "cli"
                else "accepted-notary-strict-codesign-attestation"
            ),
            "status": "pending" if args.kind == "cli" else "passed",
        },
        "notarization": {
            "status": notary["notarization_status"],
            "submission_id": notary["notarization_submission_id"],
            "submit_sha256": notary["notary_submit_sha256"],
        },
        "packages": [],
        "platform": args.platform,
        "schema_version": 2,
    }
    write_json(args.output, document)


def command_record_cli_execution_verification(args: argparse.Namespace) -> None:
    document = require_base_document(
        read_json(args.evidence), args.platform, "cli", allow_pending_cli=True
    )
    if document.get("artifact_sha256") != sha256(args.artifact):
        raise SystemExit("executed CLI bytes do not match signing evidence")
    if not re.fullmatch(r"ctx [^\r\n]+", args.version_output):
        raise SystemExit("executed CLI version output is invalid")
    document["artifact_verification"] = {
        "method": "signed-exact-byte-version-execution",
        "status": "passed",
    }
    write_json(args.evidence, document)


def command_verify_artifact(args: argparse.Namespace) -> None:
    document = require_base_document(read_json(args.evidence), args.platform, args.kind)
    actual = sha256(args.artifact)
    if document.get("artifact_sha256") != actual:
        raise SystemExit(
            f"signed artifact does not match evidence: expected "
            f"{document.get('artifact_sha256')}, got {actual}"
        )
    if args.checksum:
        expected = read_single_line(args.checksum)
        if expected != actual:
            raise SystemExit(
                f"signed artifact checksum mismatch: expected {expected}, got {actual}"
            )


def command_bind_archive(args: argparse.Namespace) -> None:
    document = require_base_document(read_json(args.evidence), args.platform, "runtime")
    nested_sha = sha256(args.nested_artifact)
    if document.get("artifact_sha256") != nested_sha:
        raise SystemExit(
            "packaged runtime bytes do not match the signed/notarized dylib evidence"
        )
    archive_sha = sha256(args.archive)
    expected_archive_sha = read_single_line(args.checksum)
    if archive_sha != expected_archive_sha:
        raise SystemExit(
            f"runtime archive checksum was not generated from final bytes: "
            f"expected {expected_archive_sha}, got {archive_sha}"
        )
    package = {
        "archive_name": args.archive.name,
        "archive_sha256": archive_sha,
        "nested_artifact_sha256": nested_sha,
        "role": args.role,
    }
    packages = document.setdefault("packages", [])
    if not isinstance(packages, list):
        raise SystemExit("signing evidence packages field is not a list")
    packages[:] = [
        item
        for item in packages
        if not (isinstance(item, dict) and item.get("role") == args.role)
    ]
    packages.append(package)
    packages.sort(key=lambda item: str(item.get("role", "")))
    write_json(args.evidence, document)


def command_verify_archive(args: argparse.Namespace) -> None:
    document = require_base_document(read_json(args.evidence), args.platform, "runtime")
    archive_sha = sha256(args.archive)
    expected_archive_sha = read_single_line(args.checksum)
    if archive_sha != expected_archive_sha:
        raise SystemExit(
            f"runtime archive checksum mismatch: expected {expected_archive_sha}, got {archive_sha}"
        )
    nested_sha = sha256(args.nested_artifact)
    if document.get("artifact_sha256") != nested_sha:
        raise SystemExit("nested runtime dylib does not match signing evidence")
    packages = document.get("packages")
    expected = {
        "archive_name": args.archive.name,
        "archive_sha256": archive_sha,
        "nested_artifact_sha256": nested_sha,
        "role": args.role,
    }
    if not isinstance(packages, list) or expected not in packages:
        raise SystemExit(
            f"signing evidence does not bind the {args.role} runtime archive bytes"
        )


def command_create_attestation(args: argparse.Namespace) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_commit):
        raise SystemExit("attestation source commit must be a lowercase 40-character git SHA")
    if not is_expected_authority(args.codesign_authority):
        raise SystemExit("attestation codesign authority is not a ctx Developer ID identity")
    document = {
        "artifact_kind": args.kind,
        "artifact_name": args.artifact.name,
        "artifact_sha256": sha256(args.artifact),
        "codesign_authority": args.codesign_authority,
        "platform": args.platform,
        "schema_version": 2,
        "source_commit": args.source_commit,
        "team_identifier": EXPECTED_TEAM_ID,
    }
    document.update(accepted_notary_fields(args.notary_submit))
    write_json(args.output, document)


def command_verify_attestation(args: argparse.Namespace) -> None:
    if not is_expected_authority(args.codesign_authority):
        raise SystemExit("attestation codesign authority is not a ctx Developer ID identity")
    document = read_json(args.attestation)
    expected = {
        "artifact_kind": args.kind,
        "artifact_name": args.artifact.name,
        "artifact_sha256": sha256(args.artifact),
        "codesign_authority": args.codesign_authority,
        "platform": args.platform,
        "schema_version": 2,
        "source_commit": args.source_commit,
        "team_identifier": EXPECTED_TEAM_ID,
    }
    expected.update(accepted_notary_fields(args.notary_submit))
    if document != expected:
        raise SystemExit("signed macOS attestation does not bind the exact pinned artifact")


def runtime_archive_attestation_document(args: argparse.Namespace) -> dict[str, Any]:
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_commit):
        raise SystemExit("attestation source commit must be a lowercase 40-character git SHA")
    if not is_expected_authority(args.codesign_authority):
        raise SystemExit("attestation codesign authority is not a ctx Developer ID identity")
    document = {
        "archive_name": args.archive.name,
        "archive_sha256": sha256(args.archive),
        "artifact_kind": "runtime-release-archive",
        "codesign_authority": args.codesign_authority,
        "nested_artifact_name": "libonnxruntime.dylib",
        "nested_artifact_sha256": sha256(args.nested_artifact),
        "platform": args.platform,
        "provenance": "native-post-transcode",
        "role": "release",
        "schema_version": 2,
        "source_commit": args.source_commit,
        "team_identifier": EXPECTED_TEAM_ID,
    }
    document.update(accepted_notary_fields(args.notary_submit))
    return document


def command_create_runtime_archive_attestation(args: argparse.Namespace) -> None:
    if args.nested_artifact.name != "libonnxruntime.dylib":
        raise SystemExit("runtime archive attestation requires libonnxruntime.dylib")
    write_json(args.output, runtime_archive_attestation_document(args))


def command_verify_runtime_archive_attestation(args: argparse.Namespace) -> None:
    expected = runtime_archive_attestation_document(args)
    if read_json(args.attestation) != expected:
        raise SystemExit(
            "signed macOS runtime archive attestation does not bind the exact release archive"
        )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    write = subparsers.add_parser("write")
    write.add_argument("--output", required=True, type=Path)
    write.add_argument("--platform", required=True)
    write.add_argument("--kind", required=True, choices=("cli", "runtime"))
    write.add_argument("--artifact", required=True, type=Path)
    write.add_argument("--codesign-details", required=True, type=Path)
    write.add_argument("--notary-submit", required=True, type=Path)
    write.set_defaults(handler=command_write)

    record_cli = subparsers.add_parser("record-cli-execution-verification")
    record_cli.add_argument("--evidence", required=True, type=Path)
    record_cli.add_argument("--platform", required=True)
    record_cli.add_argument("--artifact", required=True, type=Path)
    record_cli.add_argument("--version-output", required=True)
    record_cli.set_defaults(handler=command_record_cli_execution_verification)

    verify = subparsers.add_parser("verify-artifact")
    verify.add_argument("--evidence", required=True, type=Path)
    verify.add_argument("--platform", required=True)
    verify.add_argument("--kind", required=True, choices=("cli", "runtime"))
    verify.add_argument("--artifact", required=True, type=Path)
    verify.add_argument("--checksum", type=Path)
    verify.set_defaults(handler=command_verify_artifact)

    bind = subparsers.add_parser("bind-archive")
    bind.add_argument("--evidence", required=True, type=Path)
    bind.add_argument("--platform", required=True)
    bind.add_argument("--archive", required=True, type=Path)
    bind.add_argument("--checksum", required=True, type=Path)
    bind.add_argument("--nested-artifact", required=True, type=Path)
    bind.add_argument("--role", required=True, choices=("builder", "release"))
    bind.set_defaults(handler=command_bind_archive)

    verify_archive = subparsers.add_parser("verify-archive")
    verify_archive.add_argument("--evidence", required=True, type=Path)
    verify_archive.add_argument("--platform", required=True)
    verify_archive.add_argument("--archive", required=True, type=Path)
    verify_archive.add_argument("--checksum", required=True, type=Path)
    verify_archive.add_argument("--nested-artifact", required=True, type=Path)
    verify_archive.add_argument(
        "--role", required=True, choices=("builder", "release")
    )
    verify_archive.set_defaults(handler=command_verify_archive)

    create_attestation = subparsers.add_parser("create-attestation")
    create_attestation.add_argument("--output", required=True, type=Path)
    create_attestation.add_argument("--platform", required=True)
    create_attestation.add_argument(
        "--kind", required=True, choices=("cli", "runtime")
    )
    create_attestation.add_argument("--artifact", required=True, type=Path)
    create_attestation.add_argument("--notary-submit", required=True, type=Path)
    create_attestation.add_argument("--source-commit", required=True)
    create_attestation.add_argument("--codesign-authority", required=True)
    create_attestation.set_defaults(handler=command_create_attestation)

    verify_attestation = subparsers.add_parser("verify-attestation")
    verify_attestation.add_argument("--attestation", required=True, type=Path)
    verify_attestation.add_argument("--platform", required=True)
    verify_attestation.add_argument(
        "--kind", required=True, choices=("cli", "runtime")
    )
    verify_attestation.add_argument("--artifact", required=True, type=Path)
    verify_attestation.add_argument("--notary-submit", required=True, type=Path)
    verify_attestation.add_argument("--source-commit", required=True)
    verify_attestation.add_argument("--codesign-authority", required=True)
    verify_attestation.set_defaults(handler=command_verify_attestation)

    create_archive_attestation = subparsers.add_parser(
        "create-runtime-archive-attestation"
    )
    create_archive_attestation.add_argument("--output", required=True, type=Path)
    create_archive_attestation.add_argument("--platform", required=True)
    create_archive_attestation.add_argument("--archive", required=True, type=Path)
    create_archive_attestation.add_argument(
        "--nested-artifact", required=True, type=Path
    )
    create_archive_attestation.add_argument(
        "--notary-submit", required=True, type=Path
    )
    create_archive_attestation.add_argument("--source-commit", required=True)
    create_archive_attestation.add_argument("--codesign-authority", required=True)
    create_archive_attestation.set_defaults(
        handler=command_create_runtime_archive_attestation
    )

    verify_archive_attestation = subparsers.add_parser(
        "verify-runtime-archive-attestation"
    )
    verify_archive_attestation.add_argument(
        "--attestation", required=True, type=Path
    )
    verify_archive_attestation.add_argument("--platform", required=True)
    verify_archive_attestation.add_argument("--archive", required=True, type=Path)
    verify_archive_attestation.add_argument(
        "--nested-artifact", required=True, type=Path
    )
    verify_archive_attestation.add_argument(
        "--notary-submit", required=True, type=Path
    )
    verify_archive_attestation.add_argument("--source-commit", required=True)
    verify_archive_attestation.add_argument("--codesign-authority", required=True)
    verify_archive_attestation.set_defaults(
        handler=command_verify_runtime_archive_attestation
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    args.handler(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
