#!/usr/bin/env python3
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INVENTORY = ROOT / "scripts" / "release" / "cargo-release-inventory.py"
BUILD_INFO = ROOT / "scripts" / "release" / "linux-factory-build-info.py"
BUILD_INFO_CHECK = ROOT / "scripts" / "check-public-cli-build-info.py"
RELEASE_SBOM = ROOT / "scripts" / "release-sbom.py"
SCHEMA = ROOT / "contracts" / "release-candidate-manifest-v1.schema.json"
SEALER = ROOT / "scripts" / "release" / "seal-linux-factory-candidate.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


inventory = load(INVENTORY, "cargo_release_inventory")
build_info = load(BUILD_INFO, "linux_factory_build_info")
build_info_check = load(BUILD_INFO_CHECK, "check_public_cli_build_info")
release_sbom = load(RELEASE_SBOM, "release_sbom")
sealer = load(SEALER, "seal_linux_factory_candidate")


class LinuxReleaseFactoryTest(unittest.TestCase):
    def test_selected_graph_uses_only_reachable_packages(self) -> None:
        metadata = {
            "packages": [
                {"id": "ctx", "name": "ctx", "source": None},
                {"id": "reachable", "name": "reachable", "source": "registry"},
                {"id": "foreign", "name": "foreign", "source": "registry"},
            ],
            "resolve": {
                "nodes": [
                    {"id": "ctx", "deps": [{"pkg": "reachable"}]},
                    {"id": "reachable", "deps": []},
                    {"id": "foreign", "deps": []},
                ]
            },
        }
        self.assertEqual(inventory.selected_package_ids(metadata), {"ctx", "reachable"})

    def test_material_inventory_is_portable_and_complete(self) -> None:
        with tempfile.TemporaryDirectory() as source_directory, tempfile.TemporaryDirectory() as directory:
            source = Path(source_directory) / "Cargo.toml"
            source.write_text("[package]\nname='fixture'\nversion='1.0.0'\n")
            records = [{"kind": "main", "logical": "crates/fixture/Cargo.toml", "path": str(source)}]
            portable = inventory.stage_materials(records, Path(directory))
            self.assertNotIn(str(ROOT), json.dumps(portable))
            self.assertEqual(portable, [{"kind": "main", "logical": "crates/fixture/Cargo.toml"}])
            self.assertTrue((Path(directory) / "crates/fixture/Cargo.toml").is_file())

    def test_material_inventory_deduplicates_root_manifest_aliases(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "Cargo.toml"
            manifest.write_text("[workspace]\n", encoding="utf-8")
            records = [
                {"kind": "main", "logical": "./Cargo.toml", "path": str(manifest)},
                {"kind": "main", "logical": "Cargo.toml", "path": str(manifest)},
            ]
            portable = inventory.stage_materials(records, root / "staged")
            self.assertEqual(portable, [{"kind": "main", "logical": "Cargo.toml"}])

    def test_build_info_requires_sdk_exactly_for_macos(self) -> None:
        matrix = ROOT / "contracts" / "release-targets-v1.json"
        self.assertEqual(build_info.target(matrix, "linux-x64")["os"], "linux")
        self.assertEqual(build_info.target(matrix, "macos-arm64")["os"], "macos")
        with self.assertRaisesRegex(ValueError, "exact platform"):
            build_info.target(matrix, "freebsd-x64")

    def test_factory_candidate_uses_real_schema_construction_branch(self) -> None:
        schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
        construction = schema["properties"]["construction"]
        factory_branch = construction["allOf"][1]
        self.assertEqual(
            factory_branch["if"]["properties"]["authority"]["const"],
            "linux-cross-cargo-zigbuild-v1",
        )
        matrix = json.dumps(
            {
                "schema_version": 1,
                "targets": [
                    {
                        "id": "linux-x64",
                        "public_rust_target": "x86_64-unknown-linux-gnu",
                        "public_construction_authority": "linux-cross-cargo-zigbuild-v1",
                        "public_construction_label": "scripts/release/build-public-candidate-on-linux.sh",
                    }
                ],
            },
            separators=(",", ":"),
        ).encode()
        target = release_sbom.target_contract(
            matrix, "linux-x64", "linux-x64", "x86_64-unknown-linux-gnu"
        )
        candidate_construction = {
            "authority": target["public_construction_authority"],
            "label": target["public_construction_label"],
        }
        self.assertEqual(
            candidate_construction["label"],
            factory_branch["then"]["properties"]["label"]["const"],
        )

    def test_factory_script_pins_all_external_tools(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        factory_inputs = json.loads(
            (ROOT / "contracts" / "release-factory-inputs-v1.json").read_text()
        )
        for value in ("1.97.1", "0.15.2", "0.23.0", "0.29.0", "2.0.1"):
            self.assertIn(value, source)
        self.assertIn("--diagnostic-unsigned", source)
        self.assertIn("official release requires", source)
        self.assertIn("Ubuntu ${factory_host_os_version}", source)
        self.assertIn('"linux_host"', source)
        self.assertIn('--builder-os "${factory_host_os}"', source)
        self.assertEqual(
            factory_inputs["linux_host"],
            {
                "arch": "x86_64",
                "authority": "ctx-release-factory-ubuntu24-x86_64-v1",
                "os_id": "ubuntu",
                "os_version": "24.04",
            },
        )
        self.assertIn("llvm-strip -S -x", source)
        self.assertIn("/usr/bin/llvm-readobj", source)
        self.assertIn("ctx-release-factory.json", source)
        self.assertIn('"${cargo_zigbuild_bin}" zigbuild', source)
        self.assertNotIn("cargo zigbuild", source)
        self.assertIn('python_with_tomli=(env "PYTHONPATH=${repo_root}/${tomli_dir}" python3 -S)', source)
        self.assertNotIn('export PYTHONPATH=', source)

    def test_release_matrix_has_one_narrow_linux_abi_authority(self) -> None:
        value = json.loads(
            (ROOT / "contracts" / "release-targets-v1.json").read_text(
                encoding="utf-8"
            )
        )
        for target_value in value["targets"]:
            self.assertNotIn("bazel_platform", target_value)
            if target_value["os"] == "linux":
                self.assertEqual(
                    target_value["linux_build"], {"glibc_max": "2.35"}
                )
            else:
                self.assertIsNone(target_value["linux_build"])
        serialized = json.dumps(value)
        for stale_field in (
            "builder_image",
            "rust_commit",
            "rust_sysroot",
            "rust_toolchain",
            "ubuntu_snapshot",
        ):
            self.assertNotIn(stale_field, serialized)

    def test_factory_sealer_loads_its_sibling_under_isolated_python(self) -> None:
        subprocess.run(
            [sys.executable, "-I", os.fspath(SEALER), "--help"],
            check=True,
            capture_output=True,
            text=True,
        )

    def test_core_only_seal_binds_all_five_artifacts_and_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "candidate"
            candidate.mkdir()
            for leaf in sealer.expected_core_release_leaves():
                if leaf == sealer.FACTORY_MANIFEST:
                    continue
                (candidate / leaf).write_text(f"fixture {leaf}\n", encoding="utf-8")
            source_commit = "a" * 40
            files = []
            for path in sorted(candidate.iterdir(), key=lambda item: item.name):
                raw = path.read_bytes()
                files.append(
                    {
                        "file": path.name,
                        "sha256": hashlib.sha256(raw).hexdigest(),
                        "size_bytes": len(raw),
                    }
                )
            manifest = {
                "files": files,
                "kind": "ctx-linux-release-factory",
                "releasable": True,
                "runtime_sidecars_included": False,
                "schema_version": 1,
                "selected_targets": [
                    "linux-arm64",
                    "linux-x64",
                    "macos-arm64",
                    "macos-x64",
                    "windows-x64",
                ],
                "source_commit": source_commit,
                "version": "1.0.0",
            }
            (candidate / sealer.FACTORY_MANIFEST).write_text(
                json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n",
                encoding="utf-8",
            )
            subprocess.run(
                [
                    sys.executable,
                    "-I",
                    os.fspath(SEALER),
                    "--core-only",
                    "--candidate-dir",
                    os.fspath(candidate),
                    "--source-commit",
                    source_commit,
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            marker = candidate / sealer.CORE_COMPLETION_LEAF
            payload = json.loads(marker.read_text(encoding="utf-8"))
            self.assertEqual(
                payload["targets"],
                ["linux-arm64", "linux-x64", "macos-arm64", "macos-x64", "windows-x64"],
            )
            self.assertEqual(
                {record["name"] for record in payload["files"]},
                set(sealer.expected_core_release_leaves()),
            )
            (candidate / "ctx-macos-x64").chmod(0o600)
            subprocess.run(
                [
                    sys.executable,
                    "-I",
                    os.fspath(SEALER),
                    "--verify-core-only",
                    "--candidate-dir",
                    os.fspath(candidate),
                    "--source-commit",
                    source_commit,
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            (candidate / "ctx-macos-x64").write_text(
                "substituted bytes\n", encoding="utf-8"
            )
            with self.assertRaises(subprocess.CalledProcessError):
                subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        os.fspath(SEALER),
                        "--verify-core-only",
                        "--candidate-dir",
                        os.fspath(candidate),
                        "--source-commit",
                        source_commit,
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                )

    def test_factory_target_selection_defaults_to_all_and_accepts_known_ids(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        self.assertIn("--targets ID[,ID...]", source)
        self.assertIn("target_specs=()", source)
        self.assertIn(
            "public-cli-release-targets.py targets --field id",
            source,
        )
        self.assertIn('target_ids=("${all_target_ids[@]}")', source)
        self.assertIn('[[ "${target_spec}" == "all" ]]', source)

    def test_factory_target_selection_rejects_invalid_empty_and_duplicate_ids(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        self.assertIn("unsupported release target: ${target_id}", source)
        self.assertIn("--targets contains an empty target ID", source)
        self.assertIn("duplicate release target: ${target_id}", source)
        self.assertIn("--targets all cannot be combined with target IDs", source)

    def test_factory_skips_unselected_targets_and_their_signing_work(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        self.assertIn('for target_id in "${target_ids[@]}"; do', source)
        self.assertIn(
            'scripts/run-macos-release-signing.sh "${target_id}" cli',
            source,
        )
        self.assertIn('case "${target_id}" in\n      macos-*)', source)
        self.assertNotIn(
            "scripts/run-macos-release-signing.sh macos-arm64",
            source,
        )
        self.assertNotIn(
            "scripts/run-macos-release-signing.sh macos-x64",
            source,
        )

    def test_factory_macos_sdk_and_signing_requirements_are_target_dependent(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        self.assertIn('if [[ "${needs_macos}" == "1" ]]; then', source)
        self.assertIn(
            'if [[ "${official}" == "1" && "${needs_macos}" == "1" ]]; then',
            source,
        )
        self.assertIn('build_env+=("SDKROOT=${macos_sdk_root}"', source)
        self.assertNotIn('export SDKROOT="${macos_sdk_root}"', source)

    def test_factory_windows_signing_is_linux_native_and_precedes_sealing(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        self.assertIn("needs_windows=0", source)
        self.assertIn('[[ "${target_id}" == windows-x64 ]] && needs_windows=1', source)
        self.assertIn(
            'if [[ "${official}" == "1" && "${needs_windows}" == "1" ]]; then',
            source,
        )
        signing = source.index("scripts/run-windows-release-signing.sh cli \\")
        checksum_loop = source.index('for target_id in "${target_ids[@]}"; do', signing)
        self.assertLess(signing, checksum_loop)
        self.assertLess(signing, source.index('sha256_file "${artifact}"', signing))
        self.assertNotIn("signtool", source.lower())

    def test_windows_signing_contract_and_secret_boundary_are_pinned(self) -> None:
        contract = json.loads(
            (ROOT / "contracts" / "windows-authenticode-v1.json").read_text()
        )
        launcher = (ROOT / "scripts" / "run-windows-release-signing.sh").read_text()
        factory = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        validator = (ROOT / "scripts" / "validate-public-cli-factory-artifact.sh").read_text()
        self.assertEqual(contract["account"], "ctxsignkimmy")
        self.assertEqual(contract["certificate_profile"], "ctx-public-release")
        self.assertEqual(contract["jsign"]["version"], "7.5")
        self.assertEqual(len(contract["jsign"]["sha256"]), 64)
        self.assertIn(contract["jsign"]["sha256"], factory)
        self.assertIn('--storepass "file:${access_token}"', launcher)
        self.assertIn('--data-urlencode "client_secret@${client_secret_file}"', launcher)
        self.assertIn('env -i PATH="/usr/bin:/bin"', launcher)
        self.assertIn('java --source 11 --class-path "${jsign_jar}"', launcher)
        self.assertIn("verify-windows-authenticode.ps1", validator)
        self.assertNotIn("powershell.exe", launcher.lower())

    def test_factory_requires_complete_core_but_not_runtimes_for_promotion(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        self.assertIn("selection_complete=1", source)
        self.assertIn("runtimes_built=0", source)
        self.assertIn(
            'if [[ "${official}" == "1" && "${selection_complete}" == "1" && "${build_runtimes}" == "1" ]]; then',
            source,
        )
        self.assertIn(
            'if [[ "${official}" == "1" && "${selection_complete}" == "1" ]]; then',
            source,
        )
        self.assertIn('"releasable": official and selection_complete', source)
        self.assertIn('"runtime_sidecars_included": runtimes_built', source)
        self.assertIn("--core-only --candidate-dir", source)
        self.assertIn('"version": version', source)
        self.assertIn('"selected_targets": selected_targets', source)
        self.assertIn('factory_status="non-promotable"', source)

    def test_cargo_zigbuild_resolution_rejects_shadow_and_returns_absolute_path(self) -> None:
        helper = ROOT / "scripts" / "release" / "resolve-cargo-zigbuild.py"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            trusted = root / "trusted"
            shadow = root / "shadow"
            trusted.mkdir()
            shadow.mkdir()
            trusted_tool = trusted / "cargo-zigbuild"
            shadow_tool = shadow / "cargo-zigbuild"
            trusted_tool.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = --version ]; then printf '%s\\n' 'cargo-zigbuild 0.23.0'; else printf trusted; fi\n",
                encoding="utf-8",
            )
            shadow_tool.write_text(
                "#!/bin/sh\nprintf '%s\\n' 'cargo-zigbuild 0.22.0'\n",
                encoding="utf-8",
            )
            trusted_tool.chmod(stat.S_IRWXU)
            shadow_tool.chmod(stat.S_IRWXU)
            rejected = subprocess.run(
                [sys.executable, os.fspath(helper), "--expected-version", "0.23.0"],
                env={"PATH": os.fspath(shadow) + os.pathsep + os.fspath(trusted)},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(rejected.returncode, 0)
            selected = subprocess.run(
                [sys.executable, os.fspath(helper), "--expected-version", "0.23.0"],
                env={"PATH": os.fspath(trusted) + os.pathsep + os.fspath(shadow)},
                check=True,
                capture_output=True,
                text=True,
            )
            resolved = json.loads(selected.stdout)
            self.assertEqual(resolved["path"], os.path.realpath(trusted_tool))
            self.assertEqual(resolved["observed_version"], "0.23.0")
            self.assertEqual(
                subprocess.check_output(
                    [resolved["path"], "zigbuild"],
                    env={"PATH": os.fspath(shadow) + os.pathsep + os.environ.get("PATH", "")},
                    text=True,
                ),
                "trusted",
            )

    def _write_factory_build_info(
        self,
        root: Path,
        platform: str,
        source_repo: Path,
        source_commit: str,
    ) -> tuple[Path, Path, dict[str, object]]:
        matrix = ROOT / "contracts" / "release-targets-v1.json"
        factory_inputs_path = ROOT / "contracts" / "release-factory-inputs-v1.json"
        factory_inputs = json.loads(factory_inputs_path.read_text(encoding="utf-8"))
        selected = build_info.target(matrix, platform)
        host = factory_inputs["linux_host"]
        recipe = ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh"
        pins = build_info.recipe_pins(recipe)
        artifact = root / f"{platform}.artifact"
        output = root / f"{platform}.build-info.json"
        artifact.write_bytes(f"factory artifact: {platform}\n".encode())
        arguments = [
            sys.executable,
            os.fspath(BUILD_INFO),
            "--artifact",
            os.fspath(artifact),
            "--cargo-lock",
            os.fspath(ROOT / "Cargo.lock"),
            "--matrix",
            os.fspath(matrix),
            "--output",
            os.fspath(output),
            "--platform",
            platform,
            "--recipe",
            os.fspath(recipe),
            "--rust-version",
            f"rustc {pins['RUST_VERSION']} ({pins['RUST_COMMIT'][:9]} 2026-07-01)",
            "--source-commit",
            source_commit,
            "--source-repo",
            os.fspath(source_repo),
            "--static-status",
            "passed",
            "--local-runtime-status",
            "not_run",
            "--local-runtime-authority",
            "not_run",
            "--zig-version",
            pins["ZIG_VERSION"],
            "--cargo-zigbuild-version",
            pins["CARGO_ZIGBUILD_VERSION"],
            "--builder-authority",
            host["authority"],
            "--builder-os",
            f"{host['os_id']}-{host['os_version']}-{host['arch']}",
            "--inspector-authority",
            "ctx-release-static-llvm-v1",
            "--inspector-tool",
            "llvm",
        ]
        if selected["os"] == "macos":
            arguments.extend(
                (
                    "--macos-sdk-sha256",
                    factory_inputs["macos_sdk"]["archive_sha256"],
                    "--macos-sdk-authority",
                    factory_inputs["macos_sdk"]["authority"],
                )
            )
        subprocess.run(arguments, check=True, capture_output=True, text=True)
        return artifact, output, selected

    def _factory_source(self, root: Path) -> tuple[Path, str]:
        source_repo = root / "source"
        subprocess.run(
            ["git", "init", "--quiet", os.fspath(source_repo)], check=True
        )
        (source_repo / "source.txt").write_text("clean factory source\n")
        subprocess.run(
            ["git", "-C", os.fspath(source_repo), "add", "source.txt"], check=True
        )
        subprocess.run(
            [
                "git",
                "-C",
                os.fspath(source_repo),
                "-c",
                "user.name=Factory Test",
                "-c",
                "user.email=factory-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
            check=True,
        )
        source_commit = subprocess.check_output(
            ["git", "-C", os.fspath(source_repo), "rev-parse", "HEAD"], text=True
        ).strip()
        return source_repo, source_commit

    def test_factory_build_info_binds_matrix_abi_for_all_targets(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_repo, source_commit = self._factory_source(root)
            for platform in (
                "linux-x64",
                "linux-aarch64",
                "macos-arm64",
                "macos-x64",
                "windows-x64",
            ):
                with self.subTest(platform=platform):
                    artifact, output, selected = self._write_factory_build_info(
                        root, platform, source_repo, source_commit
                    )
                    value = json.loads(output.read_text(encoding="utf-8"))
                    self.assertEqual(value["linux_build"], selected["linux_build"])
                    self.assertEqual(
                        value["builder"]["os"], "ubuntu-24.04-x86_64"
                    )
                    self.assertNotIn("glibc_max", value["release_factory"])
                    build_info_check.validate(
                        artifact,
                        output,
                        ROOT / "contracts" / "release-targets-v1.json",
                        platform,
                        source_commit,
                        ROOT / "Cargo.lock",
                        ROOT / "contracts" / "release-factory-inputs-v1.json",
                    )

    def test_factory_build_info_rejects_stale_builder_toolchain_and_recipe(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_repo, source_commit = self._factory_source(root)
            artifact, output, _ = self._write_factory_build_info(
                root, "linux-x64", source_repo, source_commit
            )
            valid = json.loads(output.read_text(encoding="utf-8"))

            mutations = {
                "builder authority": lambda value: value["builder"].__setitem__(
                    "authority", "ctx-release-factory-ubuntu22-x86_64-v1"
                ),
                "builder OS": lambda value: value["builder"].__setitem__(
                    "os", "ubuntu-22.04-x86_64"
                ),
                "recipe": lambda value: value["builder"].__setitem__(
                    "recipe_sha256", "0" * 64
                ),
                "Rust": lambda value: value.__setitem__(
                    "rust_version", "rustc 1.96.0 (000000000 2026-01-01)"
                ),
                "Zig": lambda value: value["release_factory"].__setitem__(
                    "zig_version", "0.14.1"
                ),
                "cargo-zigbuild": lambda value: value[
                    "release_factory"
                ].__setitem__("cargo_zigbuild_version", "0.22.1"),
                "stale Linux build": lambda value: value["linux_build"].__setitem__(
                    "builder_image", "stale"
                ),
            }
            for label, mutate in mutations.items():
                with self.subTest(mutation=label):
                    value = copy.deepcopy(valid)
                    mutate(value)
                    output.write_text(
                        json.dumps(value, sort_keys=True, separators=(",", ":"))
                        + "\n",
                        encoding="utf-8",
                    )
                    with self.assertRaises(ValueError):
                        build_info_check.validate(
                            artifact,
                            output,
                            ROOT / "contracts" / "release-targets-v1.json",
                            "linux-x64",
                            source_commit,
                            ROOT / "Cargo.lock",
                            ROOT / "contracts" / "release-factory-inputs-v1.json",
                        )

    def test_factory_does_not_expose_apple_credentials_to_builds(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        build_body = source[source.index("build_target()") : source.index("for target_id in")]
        for name in (
            "APPLE_CODESIGN_CERT_P12_B64",
            "APPLE_CODESIGN_CERT_PASSWORD",
            "NOTARY_ISSUER",
            "NOTARY_KEY_ID",
            "NOTARY_KEY_P8_B64",
        ):
            self.assertNotIn(name, source)
            self.assertNotIn(name, build_body)
        self.assertNotIn("CTX_MACOS_SIGNING_SECRET_SOURCE=injected", source)

    def test_factory_reserves_macos_developer_id_signature_header_space(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        build_start = source.index("build_target()")
        build_body = source[build_start : source.index("for target_id in", build_start)]
        self.assertIn('if [[ "${target_id}" == macos-* ]]; then', build_body)
        self.assertIn("-Clink-arg=-Wl,-headerpad,0x1000", build_body)
        self.assertIn('"CARGO_ENCODED_RUSTFLAGS=${encoded_flags}"', build_body)

    def test_native_linux_validator_requires_ubuntu_24(self) -> None:
        source = (
            ROOT / "scripts" / "validate-public-cli-factory-artifact.sh"
        ).read_text()
        self.assertIn("native Linux validation requires authoritative Ubuntu 24.04 execution", source)
        self.assertIn("ubuntu-24.04", source)

    def test_native_validator_uses_immutable_version_evidence_without_cargo(self) -> None:
        source = (
            ROOT / "scripts" / "validate-public-cli-factory-artifact.sh"
        ).read_text()
        self.assertNotIn("cargo metadata", source)
        self.assertIn("check-public-cli-build-info.py", source)
        self.assertIn('--candidate-manifest "${artifact}.candidate.json"', source)
        self.assertIn('--version-file "${artifact}.version"', source)

    def test_native_validator_is_exactly_three_argument_and_core_only(self) -> None:
        source = (
            ROOT / "scripts" / "validate-public-cli-factory-artifact.sh"
        ).read_text()
        self.assertIn(
            "Usage: scripts/validate-public-cli-factory-artifact.sh "
            "PLATFORM ARTIFACT_DIR OUTPUT_DIR\n",
            source,
        )
        self.assertIn('[[ $# -eq 3 ]]', source)
        for paired_artifact_surface in (
            "COMPANION",
            "PAIR_ENVELOPE",
            '"${companion}"',
            '"${pair_envelope}"',
            "-Companion",
            "-PairEnvelope",
            "install-managed-pair.py",
        ):
            self.assertNotIn(paired_artifact_surface, source)

    def test_candidate_version_binds_artifact_build_info_and_sidecar(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "ctx-linux-aarch64"
            build_info_path = root / "ctx-linux-aarch64.build-info.json"
            candidate_path = root / "ctx-linux-aarch64.candidate.json"
            version_path = root / "ctx-linux-aarch64.version"
            artifact.write_bytes(b"exact factory artifact\n")
            build_info_document = {
                "source": {"clean": True, "commit": "a" * 40},
                "target": "aarch64-unknown-linux-gnu",
            }
            build_info_path.write_text(
                json.dumps(build_info_document, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            build_info_sha256 = hashlib.sha256(build_info_path.read_bytes()).hexdigest()
            candidate_document = {
                "schema_version": 1,
                "kind": "ctx-public-cli-candidate",
                "construction": {},
                "product": "core",
                "version": "1.0.0",
                "target": {
                    "id": "linux-arm64",
                    "platform": "linux-aarch64",
                    "rust_triple": "aarch64-unknown-linux-gnu",
                },
                "source": build_info_document["source"],
                "artifact": {
                    "file": artifact.name,
                    "sha256": hashlib.sha256(artifact.read_bytes()).hexdigest(),
                    "size_bytes": artifact.stat().st_size,
                },
                "evidence": {
                    "build_info": {
                        "file": build_info_path.name,
                        "sha256": build_info_sha256,
                    }
                },
                "tantivy": {},
            }
            candidate_path.write_text(
                json.dumps(candidate_document, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            version_path.write_text(
                "not run on this host: linux-aarch64\n", encoding="utf-8"
            )

            self.assertEqual(
                build_info_check.candidate_version(
                    artifact,
                    build_info_path,
                    candidate_path,
                    version_path,
                    "linux-aarch64",
                    build_info_sha256,
                ),
                "1.0.0",
            )

            version_path.write_text("ctx 1.0.1\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "version sidecar does not match"):
                build_info_check.candidate_version(
                    artifact,
                    build_info_path,
                    candidate_path,
                    version_path,
                    "linux-aarch64",
                    build_info_sha256,
                )

            version_path.write_text("ctx 1.0.0\n", encoding="utf-8")
            build_info_path.write_text(
                json.dumps({**build_info_document, "mutated": True}) + "\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "exact artifact and build-info"):
                build_info_check.candidate_version(
                    artifact,
                    build_info_path,
                    candidate_path,
                    version_path,
                    "linux-aarch64",
                    build_info_sha256,
                )

    def test_native_validator_restores_unix_execute_mode_after_identity_checks(
        self,
    ) -> None:
        source = (
            ROOT / "scripts" / "validate-public-cli-factory-artifact.sh"
        ).read_text()
        checksum = source.index('[[ "${before}" == "${expected_sha256}" ]]')
        identity = source.index("check-public-cli-build-info.py")
        chmod = source.index('chmod u+x "${artifact}"')
        smoke = source.index("scripts/run-native-candidate-smoke.sh")
        self.assertLess(checksum, identity)
        self.assertLess(identity, chmod)
        self.assertLess(chmod, smoke)
        self.assertNotIn('chmod u+x -- "${artifact}"', source)
        self.assertIn('if [[ "${platform}" != windows-x64 ]]; then', source)
        self.assertIn(
            '[[ -f "${artifact}" && ! -L "${artifact}" && -x "${artifact}" ]]',
            source,
        )
        self.assertIn('[[ "${after}" == "${before}" ]]', source)


if __name__ == "__main__":
    unittest.main()
