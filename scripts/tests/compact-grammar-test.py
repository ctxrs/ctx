"""Synthetic release-only grammar staging, equivalence and inventory checks."""

import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/release"))
import compact_grammar as compact
import compact_grammar_source as staging
import compact_grammar_verify as verifier

spec = importlib.util.spec_from_file_location("inventory", ROOT / "scripts/release/cargo-release-inventory.py")
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)

PARSER = """#define STATE_COUNT 4
#define LARGE_STATE_COUNT 3
#define SYMBOL_COUNT 4
#define TOKEN_COUNT 3
enum ts_symbol_identifiers {
  sym_a = 1,
  sym_b = 2,
  sym_n = 3,
};
static const char protected_alias[] = "unchanged";
static const uint16_t ts_parse_table[LARGE_STATE_COUNT][SYMBOL_COUNT] = {
  [0] = {
    [sym_a] = ACTIONS(1),
  },
  [STATE(1)] = {
    [sym_n] = STATE(2),
  },
  [2] = {
    [sym_n] = STATE(5),
    [sym_b] = ACTIONS(5),
    [sym_a] = ACTIONS(7),
    [ts_builtin_sym_end] = ACTIONS(0),
  },
};
static const uint16_t ts_small_parse_table[] = {
  [0] = 1,
    ACTIONS(9), 1,
      sym_a,
};
static const uint32_t ts_small_parse_table_map[] = {
  [SMALL_STATE(3)] = 0,
};
"""


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


class CompactGrammarTest(unittest.TestCase):
    def test_order_zero_defaults_category_collision_and_protected_source(self):
        original = PARSER + '\n#pragma GCC optimize ("O3")\n'
        transformed, stats = compact.compact(original)
        self.assertEqual(verifier.compare(original, transformed)["cells_checked"], 16)
        self.assertEqual(stats["converted_rows"], 1)
        self.assertIn("#define LARGE_STATE_COUNT 2", transformed)
        # Action 5 and goto 5 remain distinct groups, ordered by symbol ID.
        self.assertIn("[4] = 3, 7, 1, 1, 5, 1, 2, 5, 1, 3,", transformed)
        self.assertIn("[SMALL_STATE(3)] = 0,", transformed)
        self.assertIn("[SMALL_STATE(2)] = 4,", transformed)
        for wrong in (
            transformed.replace("[4] = 3, 7,", "[4] = 3, 8,"),
            transformed.replace('"unchanged"', '"modified"'),
            transformed.replace('#pragma GCC optimize ("O3")', ''),
            transformed.replace("[sym_a] = ACTIONS(1)", "[sym_a] = ACTIONS(2)"),
            transformed.replace("[sym_n] = STATE(2)", "[sym_n] = STATE(3)"),
        ):
            with self.subTest(wrong=wrong), self.assertRaises(ValueError):
                verifier.compare(original, wrong)

    def test_rejects_unknown_duplicate_misclassified_and_overflow_entries(self):
        for old, new in (
            ("ACTIONS(7)", "ACTIONS(0x7)"),
            ("[sym_a] = ACTIONS(7)", "[sym_b] = ACTIONS(7)"),
            ("[sym_n] = STATE(5)", "[sym_n] = ACTIONS(5)"),
            ("ACTIONS(7)", "ACTIONS(65536)"),
            ("[0] = 1,", "[1] = 1,"),
        ):
            with self.subTest(new=new), self.assertRaises(ValueError):
                compact.compact(PARSER.replace(old, new))

    def fixture(self, root):
        archives = root / "archives"
        archives.mkdir()
        crate = "tree-sitter-fixture-1.0.0"
        files = {
            "Cargo.toml": b'[package]\nname="tree-sitter-fixture"\nversion="1.0.0"\n',
            "one/parser.c": PARSER.encode(), "two/parser.c": PARSER.encode(),
            "bindings/rust/build.rs": b'fn main() {}\n',
            "src/scanner.c": b'/* scanner unchanged */\n',
            "src/tree_sitter/parser.h": b'/* header unchanged */\n',
            "LICENSE": b'Synthetic fixture license\n',
        }
        archive = archives / f"{crate}.crate"
        with tarfile.open(archive, "w:gz") as bundle:
            for relative, raw in files.items():
                member = tarfile.TarInfo(f"{crate}/{relative}")
                member.size = len(raw)
                bundle.addfile(member, io.BytesIO(raw))
        runtime = root / "runtime"
        runtime.mkdir()
        (runtime / "Cargo.toml").write_text('[package]\nname="tree-sitter"\nversion="0.27.0"\n')
        (runtime / "language.c").write_text("/* pinned runtime */\n")
        runtime_hash = "a" * 64
        manifest = {"format": 1, "runtime": {
            "crate": "tree-sitter-0.27.0", "archive_sha256": runtime_hash,
            "files": {"language.c": staging.digest(runtime / "language.c")},
        }, "parsers": [{
            "id": language, "crate": crate, "archive_sha256": staging.digest(archive),
            "parser": f"{language}/parser.c", "symbol": f"tree_sitter_{language}",
            "files": {relative: sha(raw) for relative, raw in files.items()},
        } for language in ("one", "two")]}
        pin_file = root / "pins.json"
        pin_file.write_text(json.dumps(manifest))
        lock = root / "Cargo.lock"
        lock.write_text('version = 4\n' + ''.join(
            f'[[package]]\nname="{name}"\nversion="{version}"\n'
            f'source="{staging.REGISTRY}"\nchecksum="{checksum}"\n'
            for name, version, checksum in (
                ("tree-sitter-fixture", "1.0.0", staging.digest(archive)),
                ("tree-sitter", "0.27.0", runtime_hash),
            )
        ))
        stage = root / "stage"
        metadata = {"packages": [{
            "id": "grammar", "name": "tree-sitter-fixture", "version": "1.0.0",
            "source": None, "manifest_path": str(stage / crate / "Cargo.toml"),
        }, {
            "id": "runtime", "name": "tree-sitter", "version": "0.27.0",
            "source": staging.REGISTRY, "manifest_path": str(runtime / "Cargo.toml"),
        }]}
        return archives, lock, stage, pin_file, metadata

    def test_verified_archive_staging_preserves_identity_notices_and_all_other_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archives, lock, stage, pins, metadata = self.fixture(root)
            original_archive = next(archives.iterdir()).read_bytes()
            with patch.object(staging, "PINS", pins):
                staging.prepare(archives, lock, stage)
                staging.bind_metadata(metadata, stage)
                package = metadata["packages"][0]
                self.assertEqual(package["source"], staging.REGISTRY)
                self.assertEqual(inventory.package_label(package, root),
                                 "@@rules_rust++crate+crates__tree-sitter-fixture-1.0.0//:tree-sitter-fixture")
                config = tomllib.loads(staging.config_path(stage).read_text())
                source = Path(package["manifest_path"]).parent
                self.assertEqual(config["paths"], [str(source)])
                evidence = metadata["source_transforms"]
                self.assertNotIn(directory, json.dumps(evidence))
                self.assertEqual(evidence["transform"]["release_profile"],
                                 {"opt_level": "s", "lto": "thin", "codegen_units": 1})
                self.assertEqual(len(evidence["packages"][0]["parsers"]), 2)
                materials = inventory.safe_materials(package, root)
                portable = inventory.stage_materials(materials, root / "materials")
                notice = next(m for m in portable if m["logical"].endswith(staging.NOTICE))
                text = (root / "materials" / notice["logical"]).read_text()
                self.assertNotIn(directory, text)
                self.assertIn(evidence["packages"][0]["source_sha256"], text)
                self.assertIn(staging.digest(next(archives.iterdir())), text)
                self.assertEqual(next(archives.iterdir()).read_bytes(), original_archive)
                with tarfile.open(fileobj=io.BytesIO(original_archive), mode="r:gz") as archive:
                    for member in archive:
                        path = stage / member.name
                        before = archive.extractfile(member).read()
                        if not path.name == "parser.c":
                            self.assertEqual(path.read_bytes(), before)
                with self.assertRaises(FileExistsError):
                    staging.prepare(archives, lock, stage)

    def test_rejects_archive_lock_drift_and_unsafe_members(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archives, lock, stage, pins, _ = self.fixture(root)
            archive = next(archives.iterdir())
            before = archive.read_bytes()
            with patch.object(staging, "PINS", pins):
                archive.write_bytes(b"wrong archive")
                with self.assertRaisesRegex(ValueError, "archive checksum mismatch"):
                    staging.prepare(archives, lock, stage)
                self.assertFalse(stage.exists())
                archive.write_bytes(before)
                locked = lock.read_text()
                lock.write_text(locked.replace('version="1.0.0"', 'version="2.0.0"'))
                with self.assertRaisesRegex(ValueError, "lock pin mismatch"):
                    staging.prepare(archives, lock, stage)
                self.assertFalse(stage.exists())
                lock.write_text(locked)
                # Even a checksum-pinned archive must remain inside fresh staging.
                with tarfile.open(archive, "w:gz") as bundle:
                    member = tarfile.TarInfo("tree-sitter-fixture-1.0.0/../../escape")
                    member.size = 1
                    bundle.addfile(member, io.BytesIO(b"x"))
                manifest = json.loads(pins.read_text())
                for parser in manifest["parsers"]:
                    parser["archive_sha256"] = staging.digest(archive)
                pins.write_text(json.dumps(manifest))
                lock.write_text(locked.replace(sha(before), staging.digest(archive)))
                with self.assertRaisesRegex(ValueError, "unexpected.*member"):
                    staging.prepare(archives, lock, stage)
                self.assertFalse((root / "escape").exists())

    def test_inventory_rejects_wrong_selection_source_notice_and_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archives, lock, stage, pins, metadata = self.fixture(root)
            with patch.object(staging, "PINS", pins):
                staging.prepare(archives, lock, stage)
                wrong = copy.deepcopy(metadata)
                wrong["packages"][0]["manifest_path"] = str(root / "registry/Cargo.toml")
                with self.assertRaisesRegex(ValueError, "did not select"):
                    staging.bind_metadata(wrong, stage)
                source = Path(metadata["packages"][0]["manifest_path"]).parent
                for path in (source / "src/scanner.c", source / "one/parser.c",
                             source / staging.NOTICE, root / "runtime/language.c"):
                    before = path.read_bytes()
                    path.write_bytes(before + b" changed")
                    with self.subTest(path=path), self.assertRaises(ValueError):
                        staging.bind_metadata(copy.deepcopy(metadata), stage)
                    path.write_bytes(before)
                extra = source / "unexpected.rs"
                extra.write_text("unexpected source")
                with self.assertRaisesRegex(ValueError, "staged source mismatch"):
                    staging.bind_metadata(copy.deepcopy(metadata), stage)

    def test_inventory_composes_notify_and_grammar_paths_without_losing_patch_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archives, lock, stage, pins, metadata = self.fixture(root)
            notify = root / "notify"
            notify.mkdir()
            def bind_notify(value, source):
                self.assertEqual(source, notify)
                value["source_patches"] = [{"name": "notify", "patch_sha256": "b" * 64}]
            with patch.object(staging, "PINS", pins):
                staging.prepare(archives, lock, stage, notify)
                paths = tomllib.loads(staging.config_path(stage, notify).read_text())["paths"]
                self.assertEqual(paths, [str(Path(metadata["packages"][0]["manifest_path"]).parent), str(notify)])
                with patch.object(inventory.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], 0, json.dumps(metadata), "")) as run, patch.object(
                        inventory.notify_source, "bind_metadata", side_effect=bind_notify):
                    value = inventory.run_metadata(root, "fixture-target", notify, stage,
                                                   ["ctx/compact-tokenizer", "fixture/second"])
                command = run.call_args.args[0]
                feature_start = command.index("--features")
                self.assertEqual(command[feature_start:feature_start + 4],
                                 ["--features", "ctx/compact-tokenizer", "--features", "fixture/second"])
                self.assertEqual(command[-2:], ["--config", str(stage / "config-notify.toml")])
                self.assertEqual(value["source_patches"][0]["name"], "notify")
                self.assertIn("source_transforms", value)
                # Ordinary inventory does not load, transform or override grammar sources.
                with patch.object(inventory.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], 0, '{"packages":[]}', "")) as run:
                    self.assertEqual(inventory.run_metadata(root, "fixture-target"), {"packages": []})
                self.assertNotIn("--config", run.call_args.args[0])
                self.assertNotIn("--features", run.call_args.args[0])

    def test_public_pin_set_matches_locked_original_sixteen_parsers(self):
        manifest = staging.pins()
        self.assertEqual(len(manifest["parsers"]), 16)
        self.assertEqual(len(staging.crates(manifest)), 14)
        self.assertEqual({p["id"] for p in manifest["parsers"]}, {
            "verilog", "cuda", "cpp", "c_sharp", "objc", "swift", "fortran",
            "ocaml", "ocaml_interface", "julia", "kotlin", "scala", "apex", "sql", "typescript", "tsx",
        })
        staging.verify_lock(ROOT / "Cargo.lock", manifest)

    def test_factory_default_and_bridge_profiles_configs_remaps_for_all_targets(self):
        factory = (ROOT / "scripts/release/build-public-candidate-on-linux.sh").read_text()
        start = factory.index("build_target() {")
        call = factory.index('  env "${build_env[@]}"', start)
        # Execute the actual build invocation against an argument recorder.
        body = factory[start:factory.index('\n  if [[ "${target_id}" == macos-*', call)]
        feature_start = factory.index("\ncargo_feature_args=()")
        selection = factory[feature_start:factory.index('[[ "${cargo_jobs}"', feature_start)]
        inventory_start = factory.index("    python3 scripts/release/cargo-release-inventory.py")
        inventory_call = factory[inventory_start:factory.index("\n  fi", inventory_start)]
        observe = '''
}
build_target "$3"
notify_inventory_args=()
CTX_PUBLIC_TARGET_TRIPLE=fixture-target
inventory=fixture-inventory
materials=fixture-materials
material_root=fixture-material-root
python3() { command python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "$@"; }
'''
        script = '''set -euo pipefail
repo_root="$1"
work_dir="$2/work with spaces"
stage_dir="$work_dir/stage"
notify_source="$stage_dir/notify/source"
compact_grammar_source="$stage_dir/compact-grammars"
legacy_compatible="$4"
cargo_zigbuild_bin="$2/capture"
cargo_jobs=1
source_commit=fixture
cargo_lock_sha256=fixture
macos_sdk_root="$work_dir/sdk"
die() { exit 1; }
''' + selection + body + observe + inventory_call
        with tempfile.TemporaryDirectory() as directory:
            recorder = Path(directory) / "capture"
            recorder.write_text('''#!/usr/bin/env python3
import json,os,sys
print(json.dumps({"argv":sys.argv[1:], "env":{k:os.environ.get(k) for k in ("CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "CFLAGS")}}))
''')
            recorder.chmod(0o755)
            for target in ("linux-x64", "linux-arm64", "macos-x64", "macos-arm64", "windows-x64"):
                for legacy in (0, 1):
                    with self.subTest(target=target, legacy=legacy):
                        env = {k: v for k, v in os.environ.items() if k not in (
                            "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_PROFILE_RELEASE_OPT_LEVEL",
                            "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                        )}
                        env.update(CFLAGS="-DFIXTURE=1", CARGO_ENCODED_RUSTFLAGS="--cfg\x1ffixture")
                        result = subprocess.run(
                            ["bash", "-c", script, "fixture", str(ROOT), directory, target, str(legacy)],
                            env=env, capture_output=True, text=True, timeout=10,
                        )
                        self.assertEqual(result.returncode, 0, result.stderr)
                        encoded, inventory_encoded = result.stdout.splitlines()
                        captured = json.loads(encoded)
                        observed = captured["env"]
                        arguments = captured["argv"]
                        inventory_arguments = json.loads(inventory_encoded)
                        config = "--config " + arguments[arguments.index("--config") + 1] if "--config" in arguments else ""
                        flags = observed["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
                        self.assertEqual(flags[:2], ["--cfg", "fixture"])
                        self.assertEqual(observed["CFLAGS"], "-DFIXTURE=1")
                        stage = Path(directory) / "work with spaces/stage"
                        if legacy:
                            for actual in (arguments, inventory_arguments):
                                self.assertEqual(actual[actual.index("--features") + 1], "ctx/compact-tokenizer")
                            config_name = "config-notify.toml" if target.startswith("macos-") else "config.toml"
                            self.assertEqual(config, f"--config {stage}/compact-grammars/{config_name}")
                            self.assertEqual(observed["CARGO_PROFILE_RELEASE_OPT_LEVEL"], "s")
                            self.assertEqual(observed["CARGO_PROFILE_RELEASE_LTO"], "thin")
                            self.assertEqual(observed["CARGO_PROFILE_RELEASE_CODEGEN_UNITS"], "1")
                            self.assertIn(f"--remap-path-prefix={stage}/compact-grammars=/ctx/deps", flags)
                        else:
                            self.assertNotIn("--features", arguments)
                            self.assertNotIn("--features", inventory_arguments)
                            self.assertEqual(config, f"--config {stage}/notify/config.toml" if target.startswith("macos-") else "")
                            for key in ("OPT_LEVEL", "LTO", "CODEGEN_UNITS"):
                                self.assertIsNone(observed[f"CARGO_PROFILE_RELEASE_{key}"])

    def test_inventory_cli_records_resolved_feature_presence_and_default_absence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            packages = []
            for name, version, relative, source in (
                ("ctx", "1.0.0", "crates/ctx-cli", None),
                ("ctx-sift-codec", "1.0.0", "crates/ctx-sift-codec", None),
                ("tantivy", "0.26.1", "registry/tantivy", staging.REGISTRY),
            ):
                manifest = root / relative / "Cargo.toml"
                manifest.parent.mkdir(parents=True)
                manifest.write_text(f'[package]\nname="{name}"\nversion="{version}"\n')
                packages.append({"id": name, "name": name, "version": version,
                                 "manifest_path": str(manifest), "source": source})
            (root / "Cargo.toml").write_text("[workspace]\n")
            for enabled in (False, True):
                features = ["compact-tokenizer"] if enabled else []
                metadata = {"packages": packages, "resolve": {"nodes": [
                    {"id": "ctx", "deps": [{"pkg": "ctx-sift-codec"}, {"pkg": "tantivy"}], "features": features},
                    {"id": "ctx-sift-codec", "deps": [], "features": features},
                    {"id": "tantivy", "deps": [], "features": sorted(inventory.REQUIRED_TANTIVY_FEATURES)},
                ]}}
                target, materials = root / "inventory.json", root / "materials.json"
                argv = ["inventory", "--repo", str(root), "--target", "fixture-target",
                        "--target-output", str(target), "--materials-output", str(materials),
                        "--material-root", str(root / f"materials-{enabled}")]
                if enabled:
                    argv += ["--features", "ctx/compact-tokenizer", "--features", "ctx/compact-tokenizer"]
                with patch.object(sys, "argv", argv), patch.object(
                        inventory.subprocess, "run", return_value=subprocess.CompletedProcess(
                            [], 0, json.dumps(metadata), "")) as cargo, patch("sys.stdout", new=io.StringIO()):
                    self.assertEqual(inventory.main(), 0)
                self.assertEqual(cargo.call_args.args[0].count("--features"), 2 if enabled else 0)
                for document in (target, materials):
                    actual = [f for f in json.loads(document.read_text())["features"]
                              if f["feature"] == "compact-tokenizer"]
                    expected = [{"label": label, "feature": "compact-tokenizer"} for label in (
                        "@@//crates/ctx-cli:ctx", "@@//crates/ctx-sift-codec:lib",
                    )] if enabled else []
                    self.assertEqual(actual, expected)

    def test_bridge_size_limit_checks_final_signed_bytes_and_exact_boundary(self):
        factory = (ROOT / "scripts/release/build-public-candidate-on-linux.sh").read_text()
        size_start = factory.index('  if [[ "${legacy_compatible}" == "1" ]]; then',
                                   factory.index('  artifact="${artifact_stage}/${binary}"'))
        guard = factory[size_start:factory.index('  sha256_file "${artifact}"', size_start)]
        self.assertGreater(size_start, factory.index("scripts/run-windows-release-signing.sh cli"))
        self.assertGreater(size_start, factory.index("scripts/run-macos-release-signing.sh"))
        script = 'set -euo pipefail\nartifact="$1"\nlegacy_compatible="$2"\ntarget_id=fixture\ndie() { exit 23; }\n' + guard
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "fixture"
            for size in (128 * 1024 * 1024 - 1, 128 * 1024 * 1024, 128 * 1024 * 1024 + 1):
                with artifact.open("wb") as output:
                    output.truncate(size)  # Sparse file: no large allocation or compilation.
                for legacy in (0, 1):
                    result = subprocess.run(["bash", "-c", script, "fixture", str(artifact), str(legacy)],
                                            capture_output=True, timeout=5)
                    self.assertEqual(result.returncode, 23 if legacy and size >= 134217728 else 0)


if __name__ == "__main__":
    unittest.main()
