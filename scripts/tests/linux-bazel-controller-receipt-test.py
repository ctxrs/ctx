#!/usr/bin/env python3
"""Regressions for descriptor-pinned Linux controller receipts."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):  # type: ignore[no-untyped-def]
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PUBLISHER = load(
    "ctx_linux_publisher_receipt_test",
    ROOT / "scripts/release/publish-linux-bazel-release.py",
)
RECEIPT = load(
    "ctx_linux_controller_receipt_test",
    ROOT / "scripts/release/write-linux-bazel-controller-receipt.py",
)


def sha(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )


def write_candidate(root: Path, platform: str, commit: str) -> None:
    root.mkdir()
    binary = "ctx-linux-aarch64" if platform == "linux-aarch64" else "ctx"
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    triple = (
        "aarch64-unknown-linux-gnu"
        if platform == "linux-aarch64"
        else "x86_64-unknown-linux-gnu"
    )
    runtime = f"ctx-onnxruntime-{platform}"
    artifact = root / binary
    artifact.write_bytes(f"fixture {platform} artifact\n".encode())
    artifact.chmod(0o755)
    artifact_sha = sha(artifact.read_bytes())
    artifact_size = artifact.stat().st_size
    source = {"clean": True, "commit": commit}
    build_name = f"{binary}.build-info.json"
    write_json(
        root / build_name,
        {
            "artifact_sha256": artifact_sha,
            "platform": platform,
            "release_version": "1.2.3",
            "source": source,
            "target": triple,
        },
    )
    size_name = f"{binary}.size.json"
    write_json(
        root / size_name,
        {
            "artifact": {
                "file": binary,
                "sha256": artifact_sha,
                "size_bytes": artifact_size,
            },
            "target": {"id": target_id, "platform": platform, "rust_triple": triple},
            "version": "1.2.3",
        },
    )
    notices_name = f"{binary}.third-party-notices.txt"
    (root / notices_name).write_text(
        f"artifact_sha256: {artifact_sha}\nfixture notices\n", encoding="utf-8"
    )
    sbom_name = f"{binary}.cdx.json"
    properties = {
        "ctx:build-info:sha256": sha((root / build_name).read_bytes()),
        "ctx:construction:authority": "bazel-release-route-v1",
        "ctx:construction:label": f"//:ctx_release_{target_id.replace('-', '_')}",
        "ctx:platform": platform,
        "ctx:source:public-commit": commit,
        "ctx:target": triple,
        "ctx:target-id": target_id,
    }
    write_json(
        root / sbom_name,
        {
            "metadata": {
                "component": {
                    "bom-ref": f"urn:ctx:artifact:sha256:{artifact_sha}",
                    "hashes": [{"alg": "SHA-256", "content": artifact_sha}],
                    "version": "1.2.3",
                },
                "properties": [
                    {"name": key, "value": value}
                    for key, value in sorted(properties.items())
                ],
            }
        },
    )
    write_json(
        root / f"{binary}.candidate.json",
        {
            "artifact": {
                "file": binary,
                "sha256": artifact_sha,
                "size_bytes": artifact_size,
            },
            "construction": {
                "authority": "bazel-release-route-v1",
                "label": f"//:ctx_release_{target_id.replace('-', '_')}",
            },
            "evidence": {
                "binary_size_report": {
                    "file": size_name,
                    "sha256": sha((root / size_name).read_bytes()),
                },
                "build_info": {
                    "file": build_name,
                    "sha256": sha((root / build_name).read_bytes()),
                },
                "cyclonedx_sbom": {
                    "file": sbom_name,
                    "sha256": sha((root / sbom_name).read_bytes()),
                },
                "third_party_notices": {
                    "file": notices_name,
                    "sha256": sha((root / notices_name).read_bytes()),
                },
            },
            "source": source,
        },
    )
    write_json(
        root / f"{binary}.dependency-advisory.json",
        {
            "source": {"commit": commit, "dirty": False},
            "status": "clean",
            "target_id": target_id,
        },
    )
    (root / f"{binary}.version").write_text("ctx 1.2.3\n", encoding="utf-8")
    for target in (binary, sbom_name, notices_name):
        (root / f"{target}.sha256").write_text(
            f"{sha((root / target).read_bytes())}\n", encoding="ascii"
        )
    for extension in ("tar.gz", "tar.zst"):
        name = f"{runtime}.{extension}"
        (root / name).write_bytes(f"fixture {name}\n".encode())
        (root / f"{name}.sha256").write_text(
            f"{sha((root / name).read_bytes())}\n", encoding="ascii"
        )
    write_json(
        root / f"{runtime}.tar.zst.asset.json",
        {
            "asset": {
                "archive_sha256": sha((root / f"{runtime}.tar.zst").read_bytes()),
                "artifact": f"{runtime}.tar.zst",
                "platform": platform,
            },
            "id": "linux_aarch64_cpu" if platform == "linux-aarch64" else "linux_x64_cpu",
        },
    )
    PUBLISHER.seal_candidate(root, platform, commit)


def receipt_args(candidate: Path, output: Path, platform: str, commit: str) -> argparse.Namespace:
    arch = "aarch64" if platform == "linux-aarch64" else "x86_64"
    evidence = f"Linux\t{arch}\t{arch}\t0\tuname\tgeneric\tnone\tpresent\t1"
    daemon = f"{arch}\t29.1.3\tfixture-daemon"
    return argparse.Namespace(
        buildx_sha256="b" * 64,
        buildx_version="v0.20.1",
        candidate_dir=candidate,
        controller_authority="authoritative",
        controller_base_image="ubuntu@sha256:" + "a" * 64,
        controller_evidence=evidence,
        controller_image_id="sha256:" + "c" * 64,
        controller_os="ubuntu\t22.04\tunknown",
        controller_recipe=ROOT / "scripts/release/linux-bazel-release-controller.Dockerfile",
        controller_socket_device_after="33",
        controller_socket_device_before="33",
        controller_socket_inode_after="44",
        controller_socket_inode_before="44",
        controller_socket_mode_after="c1ed",
        controller_socket_mode_before="c1ed",
        daemon_after=daemon,
        daemon_before=daemon,
        docker_client_sha256="d" * 64,
        docker_client_version="Docker version 27.5.1, fixture",
        launcher_authority="non_authoritative",
        launcher_evidence=f"Linux\t{arch}\t{arch}\t0\tuname\tgeneric\tnone\tpresent\t1",
        launcher_os="ubuntu\t24.04\tunknown",
        output=output,
        platform=platform,
        socket_device_after="11",
        socket_device_before="11",
        socket_inode_after="22",
        socket_inode_before="22",
        socket_mode_after="c1ed",
        socket_mode_before="c1ed",
        source_commit=commit,
        source_tree="e" * 40,
        zstd_sha256="f" * 64,
        zstd_version="zstd v1.4.8",
    )


class ReceiptTests(unittest.TestCase):
    commit = "1" * 40

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def candidate(self, platform: str = "linux-x64") -> Path:
        value = self.root / f"candidate-{platform}-{len(list(self.root.iterdir()))}"
        write_candidate(value, platform, self.commit)
        return value

    def test_x64_and_arm64_receipts_use_canonical_platforms(self) -> None:
        for platform, binary in (
            ("linux-x64", "ctx"),
            ("linux-aarch64", "ctx-linux-aarch64"),
        ):
            with self.subTest(platform=platform):
                candidate = self.candidate(platform)
                output = self.root / f"{platform}.receipt.json"
                RECEIPT.write_receipt(receipt_args(candidate, output, platform, self.commit))
                value = json.loads(output.read_bytes())
                self.assertEqual(value["platform"], platform)
                self.assertEqual(value["artifact"]["path"], binary)
                self.assertEqual(len(value["candidate_receipts"]["leaves"]), 17)

    def test_completed_artifact_substitution_with_usr_bin_true_fails(self) -> None:
        candidate = self.candidate()
        shutil.copy2("/usr/bin/true", candidate / "ctx")
        with self.assertRaisesRegex(RECEIPT.PublicationError, "does not match marker"):
            RECEIPT.write_receipt(
                receipt_args(candidate, self.root / "substituted.json", "linux-x64", self.commit)
            )

    def test_transitive_relationship_fails_even_when_completion_is_resealed(self) -> None:
        candidate = self.candidate()
        (candidate / "ctx-linux-x64.release-complete.json").unlink()
        build = json.loads((candidate / "ctx.build-info.json").read_bytes())
        build["artifact_sha256"] = "0" * 64
        write_json(candidate / "ctx.build-info.json", build)
        PUBLISHER.seal_candidate(candidate, "linux-x64", self.commit)
        with self.assertRaisesRegex(RECEIPT.ReceiptError, "build-info"):
            RECEIPT.write_receipt(
                receipt_args(candidate, self.root / "transitive.json", "linux-x64", self.commit)
            )

    def test_rename_before_publish_fails_without_receipt(self) -> None:
        candidate = self.candidate()
        output = self.root / "race-before.json"

        def race(phase: str, _snapshot: object) -> None:
            if phase == "before_publish":
                (candidate / "ctx").rename(candidate / "ctx.original")
                shutil.copy2("/usr/bin/true", candidate / "ctx")

        with self.assertRaisesRegex(RECEIPT.PublicationError, "names changed"):
            RECEIPT.write_receipt(
                receipt_args(candidate, output, "linux-x64", self.commit), race
            )
        self.assertFalse(output.exists())

    def test_rename_after_publish_leaves_only_original_verified_bytes(self) -> None:
        candidate = self.candidate()
        original_sha = sha((candidate / "ctx").read_bytes())
        output = self.root / "race-after.json"

        def race(phase: str, _snapshot: object) -> None:
            if phase == "after_publish":
                (candidate / "ctx").rename(candidate / "ctx.original")
                shutil.copy2("/usr/bin/true", candidate / "ctx")

        with self.assertRaisesRegex(RECEIPT.PublicationError, "names changed"):
            RECEIPT.write_receipt(
                receipt_args(candidate, output, "linux-x64", self.commit), race
            )
        self.assertEqual(json.loads(output.read_bytes())["artifact"]["sha256"], original_sha)

    def test_daemon_and_socket_mutations_fail(self) -> None:
        for field in ("daemon_after", "socket_inode_after"):
            with self.subTest(field=field):
                candidate = self.candidate()
                args = receipt_args(
                    candidate, self.root / f"{field}.json", "linux-x64", self.commit
                )
                setattr(args, field, "x86_64\t29.1.3\tother" if field == "daemon_after" else "99")
                with self.assertRaisesRegex(RECEIPT.ReceiptError, "authority changed"):
                    RECEIPT.write_receipt(args)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", nargs=3, metavar=("DIR", "PLATFORM", "COMMIT"))
    args, remaining = parser.parse_known_args()
    if args.fixture:
        directory, platform, commit = args.fixture
        write_candidate(Path(directory), platform, commit)
        return 0
    unittest.main(argv=[__file__, *remaining])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
