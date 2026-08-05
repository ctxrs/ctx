#!/usr/bin/env python3
"""Focused tests for deterministic Core Bazel build information."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/release/public-cli-bazel-build-info.py"
SPEC = importlib.util.spec_from_file_location("public_cli_bazel_build_info", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load build-info producer")
PRODUCER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRODUCER)


def run(*command: str, cwd: Path) -> str:
    return subprocess.run(
        command,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


class BuildInfoProducerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = Path(self.temporary.name) / "repo"
        (self.repo / "contracts").mkdir(parents=True)
        (self.repo / "crates/ctx-cli").mkdir(parents=True)
        (self.repo / "scripts/release").mkdir(parents=True)
        shutil.copy2(ROOT / ".bazelversion", self.repo / ".bazelversion")
        shutil.copy2(ROOT / "MODULE.bazel", self.repo / "MODULE.bazel")
        shutil.copy2(ROOT / "MODULE.bazel.lock", self.repo / "MODULE.bazel.lock")
        shutil.copy2(
            ROOT / "contracts/release-targets-v1.json",
            self.repo / "contracts/release-targets-v1.json",
        )
        shutil.copy2(
            ROOT / "scripts/release/linux-bazel-release.Dockerfile",
            self.repo / "scripts/release/linux-bazel-release.Dockerfile",
        )
        (self.repo / "Cargo.lock").write_text("version = 4\n", encoding="utf-8")
        (self.repo / "crates/ctx-cli/Cargo.toml").write_text(
            '[package]\nname = "ctx"\nversion = "0.26.0"\n',
            encoding="utf-8",
        )
        (self.repo / ".gitignore").write_text("inputs/\n", encoding="utf-8")
        (self.repo / "tracked.txt").write_text("clean\n", encoding="utf-8")
        run("git", "init", "-q", cwd=self.repo)
        run("git", "config", "user.email", "ctx-release-test@example.invalid", cwd=self.repo)
        run("git", "config", "user.name", "ctx release test", cwd=self.repo)
        run("git", "add", ".", cwd=self.repo)
        run("git", "commit", "-qm", "fixture", cwd=self.repo)
        self.commit = run("git", "rev-parse", "HEAD", cwd=self.repo)

        inputs = self.repo / "inputs"
        inputs.mkdir()
        cargo_lock_sha256 = hashlib.sha256(
            (self.repo / "Cargo.lock").read_bytes()
        ).hexdigest()
        self.artifact = inputs / "ctx"
        self.write_artifact("x86_64-unknown-linux-gnu", cargo_lock_sha256)
        self.rustc = inputs / "rustc"
        self.rustc.write_text(
            "#!/usr/bin/env sh\n"
            "printf 'rustc 1.97.1 (8bab26f4f 2026-07-10)\\n'\n",
            encoding="utf-8",
        )
        self.rustc.chmod(0o755)
        self.common = {
            "artifact": self.artifact,
            "bazel_version_file": self.repo / ".bazelversion",
            "builder_recipe": self.repo
            / "scripts/release/linux-bazel-release.Dockerfile",
            "cargo_lock": self.repo / "Cargo.lock",
            "cargo_toml": self.repo / "crates/ctx-cli/Cargo.toml",
            "matrix": self.repo / "contracts/release-targets-v1.json",
            "module_file": self.repo / "MODULE.bazel",
            "module_lock": self.repo / "MODULE.bazel.lock",
            "platform": "linux-x64",
            "source_commit": self.commit,
            "source_repo": self.repo,
            "version": "0.26.0",
        }
        self.images = {
            "builder_image_id": "sha256:" + "a" * 64,
            "runtime_image_id": "sha256:" + "b" * 64,
            "inspector_image_id": "sha256:" + "c" * 64,
        }

    def write_artifact(self, target: str, cargo_lock_sha256: str) -> None:
        self.artifact.write_text(
            f"""#!/usr/bin/env bash
set -euo pipefail
case "${{1:-}}" in
  _release-build-identity)
    printf 'CTX_RELEASE_BUILD_SOURCE_COMMIT={self.commit}\\n'
    printf 'CTX_RELEASE_BUILD_CARGO_LOCK_SHA256={cargo_lock_sha256}\\n'
    printf 'CTX_RELEASE_BUILD_TARGET={target}\\n'
    ;;
  --version) printf 'ctx 0.26.0\\n' ;;
  *) exit 1 ;;
esac
""",
            encoding="utf-8",
        )
        self.artifact.chmod(0o755)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def create_args(self, output: Path, **overrides: object) -> argparse.Namespace:
        values: dict[str, object] = {
            **self.common,
            **self.images,
            "docker": "/usr/bin/docker",
            "output": output,
            "rust_version": "rustc 1.97.1 (8bab26f4f 2026-07-10)",
        }
        values.update(overrides)
        return argparse.Namespace(**values)

    def verify_args(self, build_info: Path, **overrides: object) -> argparse.Namespace:
        values: dict[str, object] = {
            **self.common,
            "build_info": build_info,
            "rustc": self.rustc,
        }
        values.update(overrides)
        return argparse.Namespace(**values)

    def create(self, output: Path, **overrides: object) -> None:
        with (
            mock.patch.object(PRODUCER, "verify_image"),
            mock.patch.object(PRODUCER, "run_container_gates"),
        ):
            PRODUCER.create(self.create_args(output, **overrides))

    def test_create_is_deterministic_and_verifies(self) -> None:
        first = self.repo / "inputs/first.json"
        second = self.repo / "inputs/second.json"
        self.create(first)
        self.create(second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        digest = PRODUCER.verify(self.verify_args(first))
        self.assertEqual(digest, hashlib.sha256(first.read_bytes()).hexdigest())

    def test_arm64_create_is_deterministic_and_verifies(self) -> None:
        cargo_lock_sha256 = hashlib.sha256(
            (self.repo / "Cargo.lock").read_bytes()
        ).hexdigest()
        self.write_artifact("aarch64-unknown-linux-gnu", cargo_lock_sha256)
        self.common["platform"] = "linux-aarch64"
        output = self.repo / "inputs/arm64.json"
        self.create(output)
        digest = PRODUCER.verify(self.verify_args(output))
        value = json.loads(output.read_bytes())
        self.assertEqual(digest, hashlib.sha256(output.read_bytes()).hexdigest())
        self.assertEqual(value["platform"], "linux-aarch64")
        self.assertEqual(value["target"], "aarch64-unknown-linux-gnu")
        self.assertEqual(
            value["linux_build"]["rust_sysroot"],
            "/opt/rustup/toolchains/1.97.1-aarch64-unknown-linux-gnu",
        )

    def test_dirty_tree_fails_closed(self) -> None:
        (self.repo / "tracked.txt").write_text("dirty\n", encoding="utf-8")
        with self.assertRaisesRegex(PRODUCER.BuildInfoError, "checkout is dirty"):
            self.create(self.repo / "inputs/dirty.json")

    def test_mismatched_source_version_target_and_toolchain_fail_closed(self) -> None:
        cases = (
            ({"source_commit": "f" * 40}, "source commit does not match"),
            ({"version": "0.26.3"}, "source version mismatch"),
            ({"platform": "linux-arm64"}, "owned native Linux target"),
            ({"rust_version": "rustc 1.98.0 (fffffffff 2026-08-01)"}, "rustc"),
        )
        for index, (overrides, message) in enumerate(cases):
            with self.subTest(overrides=overrides):
                with self.assertRaisesRegex(PRODUCER.BuildInfoError, message):
                    self.create(self.repo / f"inputs/rejected-{index}.json", **overrides)

    def test_python_310_cargo_version_fallback_is_strict(self) -> None:
        cargo_toml = self.repo / "crates/ctx-cli/Cargo.toml"
        with mock.patch.object(PRODUCER, "tomllib", None):
            self.assertEqual(PRODUCER.release_version(cargo_toml), "0.26.0")
            cargo_toml.write_text(
                '[package]\nversion = "0.26.0"\nversion = "0.26.3"\n',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(PRODUCER.BuildInfoError, "malformed"):
                PRODUCER.release_version(cargo_toml)

    def test_pinned_builder_provisions_python_toml_parser(self) -> None:
        recipe = (
            ROOT / "scripts/release/linux-bazel-release.Dockerfile"
        ).read_text(encoding="utf-8")
        self.assertIn("python3-tomli", recipe)
        self.assertIn("bazel-${BAZEL_VERSION}-linux-${BAZEL_ARCH}", recipe)
        self.assertIn('"${BAZEL_SHA256}"', recipe)

    def test_container_gates_select_target_architecture(self) -> None:
        for platform, docker_platform in (
            ("linux-x64", "linux/amd64"),
            ("linux-aarch64", "linux/arm64"),
        ):
            with (
                self.subTest(platform=platform),
                mock.patch.object(PRODUCER, "run_checked", return_value="") as checked,
            ):
                PRODUCER.run_container_gates(
                    docker="/usr/bin/docker",
                    source_repo=ROOT,
                    artifact=self.artifact,
                    version="0.26.0",
                    platform=platform,
                    runtime_image_id=self.images["runtime_image_id"],
                    inspector_image_id=self.images["inspector_image_id"],
                )
                self.assertEqual(len(checked.call_args_list), 2)
                for call in checked.call_args_list:
                    command = call.args[0]
                    self.assertEqual(
                        command[command.index("--platform") + 1],
                        docker_platform,
                    )

    def test_verify_rejects_changed_bazel_binding(self) -> None:
        accepted = self.repo / "inputs/accepted.json"
        changed = self.repo / "inputs/changed.json"
        self.create(accepted)
        value = json.loads(accepted.read_bytes())
        value["bazel"]["module_lock_sha256"] = "f" * 64
        changed.write_bytes(PRODUCER.canonical_json(value))
        with self.assertRaisesRegex(PRODUCER.BuildInfoError, "does not match the exact"):
            PRODUCER.verify(self.verify_args(changed))

    def test_create_never_replaces_existing_output(self) -> None:
        output = self.repo / "inputs/existing.json"
        output.write_text("sentinel\n", encoding="utf-8")
        with self.assertRaisesRegex(PRODUCER.BuildInfoError, "already exists"):
            self.create(output)
        self.assertEqual(output.read_text(encoding="utf-8"), "sentinel\n")

    def test_native_linux_builder_routes_both_architectures_through_bazel(self) -> None:
        builder = (
            ROOT / "scripts/release/build-linux-bazel-release.sh"
        ).read_text(encoding="utf-8")
        packager = (
            ROOT / "scripts/package-public-cli-bazel-release.sh"
        ).read_text(encoding="utf-8")
        permission_gates = (
            'chmod 0555 "${artifact}"',
            'chmod 0444 "${artifact}.sha256" "${artifact}.version"',
            'chmod 0755 "${task_root}/release-input"',
        )
        producer_call = (
            "python3 -I scripts/release/public-cli-bazel-build-info.py create"
        )
        for permission_gate in permission_gates:
            self.assertIn(permission_gate, builder)
            self.assertLess(builder.index(permission_gate), builder.index(producer_call))
        self.assertIn("route_target=//:ctx_release_linux_x64", builder)
        self.assertIn("route_target=//:ctx_release_linux_arm64", builder)
        self.assertIn("docker_platform=linux/arm64", builder)
        self.assertIn("requires a native ${expected_host_arch} host", builder)
        self.assertIn("host_authority", builder)
        self.assertIn("emulation is diagnostic only", builder)
        self.assertIn("requires a native ${expected_host_arch} Docker daemon", builder)
        self.assertIn(
            "d7aedc8565ed47b6231badb80b09f034"
            "e389c5f2b1c2ac2c55406f7c661d8b88",
            builder,
        )
        self.assertIn(
            'release_work_root="${CTX_LINUX_RELEASE_WORK_ROOT:-/tmp}"',
            builder,
        )
        self.assertIn(
            'task_root="$(mktemp -d "${task_prefix}XXXXXX")"',
            builder,
        )
        self.assertIn(
            'cache_root="${CTX_LINUX_RELEASE_CACHE_ROOT:-}"',
            builder,
        )
        self.assertIn(
            'docker_run_args+=(-v "${cache_root}:/build/cache:rw")',
            builder,
        )
        output_normalization = "print(os.path.abspath(sys.argv[1]))"
        default_symbols = (
            'private_symbols_dir="${output_dir}.private-debug-symbols"'
        )
        self.assertIn(output_normalization, builder)
        self.assertIn(default_symbols, builder)
        self.assertLess(
            builder.index(output_normalization), builder.index(default_symbols)
        )
        self.assertNotIn(
            'mktemp -d "/tmp/ctx-public-${platform}-bazel-release.',
            builder,
        )
        self.assertIn('BUILD_WORKSPACE_DIRECTORY="$PWD"', builder)
        self.assertIn('RUNFILES_DIR="$route.runfiles"', builder)
        self.assertIn("--network none", builder)
        for value in (
            "CTX_OSV_SCANNER=/release-advisory/osv-scanner",
            "CTX_OSV_DATABASE_DIR=/release-advisory/database",
            "CTX_OSV_DATABASE_METADATA=/release-advisory/database-metadata.json",
        ):
            self.assertIn(value, builder)
        self.assertNotIn("cargo build", builder)
        self.assertNotIn("cargo zigbuild", builder)
        self.assertNotIn("qemu-", builder)
        for release_script in (builder, packager):
            self.assertIn("cargo-version", release_script)
            self.assertNotIn("import tomllib", release_script)

    def test_container_gates_stage_only_explicit_world_readable_inputs(self) -> None:
        source = Path(self.temporary.name) / "private-source"
        source.mkdir(mode=0o700)
        for relative_path, _ in PRODUCER.CONTAINER_GATE_INPUTS:
            destination = source / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes((ROOT / relative_path).read_bytes())
            destination.chmod(0o600)
        source.chmod(0o700)

        with PRODUCER.staged_container_gate_source(source) as staged:
            staged_path = staged
            self.assertNotEqual(staged, source)
            self.assertEqual(staged.stat().st_mode & 0o777, 0o755)
            self.assertEqual(
                sorted(
                    path.relative_to(staged)
                    for path in staged.rglob("*")
                    if path.is_file()
                ),
                sorted(path for path, _ in PRODUCER.CONTAINER_GATE_INPUTS),
            )
            for directory in (
                path for path in staged.rglob("*") if path.is_dir()
            ):
                self.assertEqual(directory.stat().st_mode & 0o777, 0o755)
            for relative_path, expected_mode in PRODUCER.CONTAINER_GATE_INPUTS:
                staged_file = staged / relative_path
                self.assertEqual(
                    staged_file.read_bytes(),
                    (ROOT / relative_path).read_bytes(),
                )
                self.assertEqual(staged_file.stat().st_mode & 0o777, expected_mode)

        self.assertFalse(staged_path.exists())

    def test_container_gates_never_mount_the_private_source_checkout(self) -> None:
        calls: list[tuple[list[str], str]] = []

        def capture(command: list[str], label: str, **_: object) -> str:
            calls.append((command, label))
            self.assertNotIn(f"{ROOT}:/repo:ro", command)
            repo_mount = next(
                value
                for index, value in enumerate(command)
                if command[index - 1 : index] == ["-v"]
                and value.endswith(":/repo:ro")
            )
            staged = Path(repo_mount.removesuffix(":/repo:ro"))
            self.assertEqual(staged.stat().st_mode & 0o777, 0o755)
            for relative_path, expected_mode in PRODUCER.CONTAINER_GATE_INPUTS:
                staged_file = staged / relative_path
                self.assertEqual(staged_file.stat().st_mode & 0o777, expected_mode)
            return ""

        with mock.patch.object(PRODUCER, "run_checked", side_effect=capture):
            PRODUCER.run_container_gates(
                docker="/usr/bin/docker",
                source_repo=ROOT,
                artifact=self.artifact,
                version="0.26.0",
                platform="linux-x64",
                runtime_image_id=self.images["runtime_image_id"],
                inspector_image_id=self.images["inspector_image_id"],
            )

        self.assertEqual(
            [label for _, label in calls],
            ["pinned inspector static ABI gate", "pinned native runtime gate"],
        )
        runtime_call = calls[1][0]
        self.assertIn("/tmp:rw,nosuid,nodev,exec", runtime_call)
        runtime_command = runtime_call[runtime_call.index("-c") + 1]
        self.assertIn(
            "install -m 0755 /candidate/ctx /tmp/candidate/ctx",
            runtime_command,
        )
        self.assertIn(
            "scripts/run-native-candidate-smoke.sh /tmp/candidate/ctx",
            runtime_command,
        )


if __name__ == "__main__":
    unittest.main()
