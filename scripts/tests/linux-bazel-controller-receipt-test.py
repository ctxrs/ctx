#!/usr/bin/env python3
"""Contracts for compact Linux controller receipts."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
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


BUNDLE = load(
    "ctx_linux_release_bundle_receipt_test",
    ROOT / "scripts/release/release_bundle.py",
)
RECEIPT = load(
    "ctx_linux_controller_receipt_test",
    ROOT / "scripts/release/write-linux-bazel-controller-receipt.py",
)


def sha(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def write_candidate(root: Path, platform: str, commit: str) -> None:
    root.mkdir()
    binary = "ctx-linux-aarch64" if platform == "linux-aarch64" else "ctx"
    for name in BUNDLE.expected_release_leaves(platform):
        path = root / name
        path.write_bytes(f"fixture {platform} {name}\n".encode())
        path.chmod(0o755 if name == binary else 0o644)
    BUNDLE.seal_bundle(root, platform, commit)


def receipt_args(
    candidate: Path, output: Path, platform: str, commit: str
) -> argparse.Namespace:
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
        controller_recipe=ROOT
        / "scripts/release/linux-bazel-release-controller.Dockerfile",
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
        launcher_evidence=evidence,
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

    def test_receipt_binds_each_canonical_bundle(self) -> None:
        for platform, binary in (
            ("linux-x64", "ctx"),
            ("linux-aarch64", "ctx-linux-aarch64"),
        ):
            with self.subTest(platform=platform):
                candidate = self.candidate(platform)
                output = self.root / f"{platform}.receipt.json"
                RECEIPT.write_receipt(
                    receipt_args(candidate, output, platform, self.commit)
                )
                value = json.loads(output.read_bytes())
                marker = candidate / BUNDLE.completion_leaf(platform)
                self.assertEqual(value["platform"], platform)
                self.assertEqual(value["artifact"]["path"], binary)
                self.assertEqual(len(value["candidate_receipts"]["leaves"]), 17)
                self.assertEqual(
                    value["candidate_receipts"]["completion_sha256"],
                    sha(marker.read_bytes()),
                )

    def test_modified_bundle_is_rejected(self) -> None:
        candidate = self.candidate()
        (candidate / "ctx").write_bytes(b"replacement\n")
        with self.assertRaisesRegex(RECEIPT.BundleError, "completion marker"):
            RECEIPT.write_receipt(
                receipt_args(
                    candidate, self.root / "modified.json", "linux-x64", self.commit
                )
            )

    def test_wrong_source_identity_is_rejected(self) -> None:
        candidate = self.candidate()
        with self.assertRaisesRegex(RECEIPT.BundleError, "completion identity"):
            RECEIPT.write_receipt(
                receipt_args(candidate, self.root / "wrong.json", "linux-x64", "2" * 40)
            )

    def test_existing_receipt_is_not_replaced(self) -> None:
        candidate = self.candidate()
        output = self.root / "existing.json"
        output.write_text("sentinel\n", encoding="utf-8")
        with self.assertRaisesRegex(RECEIPT.ReceiptError, "already exists"):
            RECEIPT.write_receipt(
                receipt_args(candidate, output, "linux-x64", self.commit)
            )
        self.assertEqual(output.read_text(encoding="utf-8"), "sentinel\n")

    def test_daemon_and_socket_mutations_fail(self) -> None:
        for field in ("daemon_after", "socket_inode_after"):
            with self.subTest(field=field):
                candidate = self.candidate()
                args = receipt_args(
                    candidate,
                    self.root / f"{field}.json",
                    "linux-x64",
                    self.commit,
                )
                setattr(
                    args,
                    field,
                    "x86_64\t29.1.3\tother" if field == "daemon_after" else "99",
                )
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
