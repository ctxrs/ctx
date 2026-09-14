#!/usr/bin/env python3

import ast
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

try:
    from tools.bazel.check_rust_target_inventory import (
        InventoryError,
        bazel_path_declared,
        cargo_targets,
        dependency_ownership,
        live_package_manifests,
        local_graph,
        rust_source_owned,
    )
except ModuleNotFoundError:
    from check_rust_target_inventory import (
        InventoryError,
        bazel_path_declared,
        cargo_targets,
        dependency_ownership,
        live_package_manifests,
        local_graph,
        rust_source_owned,
    )


def module(source: str) -> ast.Module:
    return ast.parse(source)


class ExecutableOwnershipTest(unittest.TestCase):
    """Exercise Git discovery, Cargo parsing and BUILD ownership via the CLI."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        directory = Path(temporary.name)
        self.root = directory / "repo"
        self.root.mkdir()
        home = directory / "home"
        home.mkdir()
        self.environment = {
            key: value for key, value in os.environ.items()
            if not key.startswith("GIT_") and key not in {"PYTHONPATH", "PYTHONHOME"}
        }
        self.environment.update(
            HOME=str(home),
            XDG_CONFIG_HOME=str(home),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(directory),
        )
        subprocess.run(
            ["git", "init", "-q", str(self.root)],
            env=self.environment, check=True,
        )
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["fixture"]\n', encoding="utf-8"
        )
        (self.root / "BUILD.bazel").write_text("# workspace\n", encoding="utf-8")
        self.package = self.root / "fixture"
        self.package.mkdir()

    def check_target(
        self, kind: str, build: str, *, harness: bool | None = None,
        implicit: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        for previous in ("tests/entry.rs", "src/bin/entry.rs"):
            (self.package / previous).unlink(missing_ok=True)
        path = {"test": "tests/entry.rs", "bin": "src/bin/entry.rs"}[kind]
        source = self.package / path
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text(
            "fn main() {}\n" if kind == "bin" or harness is False
            else "#[test] fn entry() {}\n", encoding="utf-8",
        )
        manifest = '[package]\nname = "fixture"\nversion = "0.1.0"\n'
        if not implicit:
            manifest += f'[[{kind}]]\nname = "entry"\npath = "{path}"\n'
            if harness is not None:
                manifest += f"harness = {str(harness).lower()}\n"
        (self.package / "Cargo.toml").write_text(manifest, encoding="utf-8")
        (self.package / "BUILD.bazel").write_text(
            'filegroup(name = "cargo_package_data", srcs = glob(["**"]))\n'
            + build.replace("ENTRY", path), encoding="utf-8",
        )
        return subprocess.run(
            [sys.executable, str(Path(__file__).with_name("check_rust_target_inventory.py"))],
            cwd=self.root, env=self.environment, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, timeout=10,
        )

    def assert_owned(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "live Cargo/Bazel ownership covers 1 Cargo targets and "
            "0 local edges across 1 discovered packages\n",
        )

    def assert_unowned(self, result: subprocess.CompletedProcess[str], kind: str) -> None:
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(f"Cargo target is not owned by Bazel: {kind}:entry", result.stderr)

    def test_compile_only_rules_cannot_own_executables(self) -> None:
        for kind in ("test", "bin"):
            for implicit in (False, True):
                for rule in ("rust_library", "rust_proc_macro"):
                    with self.subTest(kind=kind, implicit=implicit, rule=rule):
                        self.assert_unowned(self.check_target(
                            kind, f'{rule}(name = "entry", crate_root = "ENTRY")',
                            implicit=implicit,
                        ), kind)

    def test_binary_rules_own_bins(self) -> None:
        for rule in ("rust_binary", "ctx_rust_binary"):
            for implicit in (False, True):
                with self.subTest(rule=rule, implicit=implicit):
                    self.assert_owned(self.check_target(
                        "bin", f'{rule}(name = "entry", crate_root = "ENTRY")',
                        implicit=implicit,
                    ))

    def test_tests_require_test_execution_even_without_libtest(self) -> None:
        for rule in ("rust_binary", "ctx_rust_binary"):
            for harness in (True, False):
                with self.subTest(rule=rule, harness=harness):
                    self.assert_unowned(self.check_target(
                        "test", f'{rule}(name = "entry", crate_root = "ENTRY")',
                        harness=harness,
                    ), "test")

    def test_test_rules_preserve_cargo_harness_mode(self) -> None:
        for rule in ("rust_test", "ctx_rust_test"):
            for harness in (True, False):
                for bazel_harness in (True, False):
                    with self.subTest(rule=rule, harness=harness, bazel_harness=bazel_harness):
                        result = self.check_target(
                            "test", f'{rule}(name = "entry", crate_root = "ENTRY", '
                            f'use_libtest_harness = {bazel_harness})', harness=harness,
                        )
                        if harness == bazel_harness:
                            self.assert_owned(result)
                        else:
                            self.assert_unowned(result, "test")

    def test_default_harness_and_differently_named_test_owner(self) -> None:
        for rule in ("rust_test", "ctx_rust_test"):
            for implicit in (False, True):
                with self.subTest(rule=rule, implicit=implicit):
                    self.assert_owned(self.check_target(
                        "test", 'rust_library(name = "entry", srcs = ["ENTRY"])\n'
                        f'{rule}(name = "entry_test", crate_root = "ENTRY")',
                        implicit=implicit,
                    ))

    def test_existing_binary_contract_macros_are_tests(self) -> None:
        # Each macro forwards src to ctx_rust_test through binary_contracts.bzl.
        for rule in (
            "ctx_binary_contract_test", "ctx_cli_contract_test", "ctx_cli_integration_test",
            "agent_application_binary_contract", "daemon_cli_binary_contract",
            "history_ingest_binary_contract", "history_read_binary_contract",
            "observability_binary_contract",
        ):
            for kind, harness in (("test", True), ("test", False), ("bin", None)):
                with self.subTest(rule=rule, kind=kind, harness=harness):
                    result = self.check_target(
                        kind, f'{rule}(name = "entry", src = "ENTRY")', harness=harness,
                    )
                    if kind == "test" and harness:
                        self.assert_owned(result)
                    else:
                        self.assert_unowned(result, kind)

    def test_rule_name_shape_does_not_establish_executable_ownership(self) -> None:
        for rule in ("rust_unrecognized", "unknown_contract_test", "unknown_binary_contract"):
            with self.subTest(rule=rule):
                self.assert_unowned(self.check_target(
                    "test", f'{rule}(name = "entry", srcs = ["ENTRY"])',
                ), "test")

    def test_test_rule_cannot_own_a_binary(self) -> None:
        for rule in ("rust_test", "ctx_rust_test"):
            with self.subTest(rule=rule):
                self.assert_unowned(self.check_target(
                    "bin", f'{rule}(name = "entry", crate_root = "ENTRY")',
                ), "bin")

    def test_executable_rule_still_needs_the_source(self) -> None:
        for kind, rule in (("test", "ctx_rust_test"), ("bin", "ctx_rust_binary")):
            for attributes in ('crate_root = "other.rs"', 'data = ["ENTRY"]',
                               'srcs = glob(["**/*.rs"], exclude = ["ENTRY"])'):
                with self.subTest(kind=kind, attributes=attributes):
                    self.assert_unowned(self.check_target(
                        kind, f'{rule}(name = "entry", {attributes})',
                    ), kind)


class RustTargetInventoryTest(unittest.TestCase):
    def local_edge_fixture(
        self,
        *,
        dependency_definition: str,
        workspace_dependencies: str = "",
        labels: tuple[str, ...] = (),
    ) -> tuple[Path, dict[str, tuple[Path, dict[str, object]]]]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "Cargo.toml").write_text(
            "[workspace]\n" + workspace_dependencies,
            encoding="utf-8",
        )
        consumer = root / "consumer"
        consumer.joinpath("src").mkdir(parents=True)
        consumer.joinpath("src/lib.rs").write_text("", encoding="utf-8")
        label_list = (
            " + [" + ", ".join(repr(label) for label in labels) + "]"
            if labels
            else ""
        )
        consumer.joinpath("BUILD.bazel").write_text(
            "rust_library(\n"
            '    name = "lib",\n'
            '    crate_root = "src/lib.rs",\n'
            "    deps = all_crate_deps(normal = True, build = True)"
            + label_list
            + ",\n)\n"
            "ctx_rust_test(\n"
            '    name = "unit_tests",\n'
            '    crate_root = "src/lib.rs",\n'
            "    deps = all_crate_deps(normal_dev = True)"
            + label_list
            + ",\n)\n",
            encoding="utf-8",
        )
        consumer_data: dict[str, object] = {
            "package": {"name": "consumer"},
            **tomllib.loads(dependency_definition),
        }

        packages: dict[str, tuple[Path, dict[str, object]]] = {"consumer": (consumer, consumer_data)}
        for name in ("ctx-history-jsonl", "ctx-history-source-sqlite"):
            directory = root / "crates" / name
            directory.mkdir(parents=True)
            directory.joinpath("Cargo.toml").write_text(
                f'[package]\nname = "{name}"\n', encoding="utf-8"
            )
            directory.joinpath("BUILD.bazel").write_text("rust_library(name = \"lib\")\n", encoding="utf-8")
            packages[name] = (directory, {"package": {"name": name}})
        return root, packages

    def test_source_family_edges_require_their_explicit_library_labels(self) -> None:
        cases = (
            (
                "normal direct JSONL",
                "ctx-history-jsonl",
                "[dependencies]\n"
                'ctx-history-jsonl = { path = "../crates/ctx-history-jsonl" }\n',
                "",
                "//crates/ctx-history-jsonl:lib",
                {"ctx-history-jsonl"},
            ),
            (
                "normal inherited renamed SQLite",
                "ctx-history-source-sqlite",
                "[dependencies]\nhistory_sqlite.workspace = true\n",
                "[workspace.dependencies]\n"
                'history_sqlite = { package = "ctx-history-source-sqlite", path = "crates/ctx-history-source-sqlite" }\n',
                "//crates/ctx-history-source-sqlite:lib",
                {"ctx-history-source-sqlite"},
            ),
            (
                "dev direct SQLite",
                "ctx-history-source-sqlite",
                "[dev-dependencies]\n"
                'ctx-history-source-sqlite = { path = "../crates/ctx-history-source-sqlite" }\n',
                "",
                "//crates/ctx-history-source-sqlite:test_support_lib",
                set(),
            ),
            (
                "dev renamed JSONL",
                "ctx-history-jsonl",
                "[dev-dependencies]\n"
                'history_jsonl = { package = "ctx-history-jsonl", path = "../crates/ctx-history-jsonl" }\n',
                "",
                "//crates/ctx-history-jsonl:test_support_lib",
                set(),
            ),
            (
                "build direct SQLite",
                "ctx-history-source-sqlite",
                "[build-dependencies]\n"
                'ctx-history-source-sqlite = { path = "../crates/ctx-history-source-sqlite" }\n',
                "",
                "//crates/ctx-history-source-sqlite:lib",
                {"ctx-history-source-sqlite"},
            ),
            (
                "build inherited renamed JSONL",
                "ctx-history-jsonl",
                "[build-dependencies]\nhistory_jsonl.workspace = true\n",
                "[workspace.dependencies]\n"
                'history_jsonl = { package = "ctx-history-jsonl", path = "crates/ctx-history-jsonl" }\n',
                "//crates/ctx-history-jsonl:lib",
                {"ctx-history-jsonl"},
            ),
            (
                "target-specific direct JSONL",
                "ctx-history-jsonl",
                "[target.'cfg(unix)'.dependencies]\n"
                'ctx-history-jsonl = { path = "../crates/ctx-history-jsonl" }\n',
                "",
                "//crates/ctx-history-jsonl:lib",
                {"ctx-history-jsonl"},
            ),
            (
                "target-specific inherited renamed SQLite",
                "ctx-history-source-sqlite",
                "[target.'cfg(unix)'.dependencies]\nhistory_sqlite.workspace = true\n",
                "[workspace.dependencies]\n"
                'history_sqlite = { package = "ctx-history-source-sqlite", path = "crates/ctx-history-source-sqlite" }\n',
                "//crates/ctx-history-source-sqlite:lib",
                {"ctx-history-source-sqlite"},
            ),
        )
        for (
            edge,
            package,
            dependency_definition,
            workspace_dependencies,
            label,
            expected_graph,
        ) in cases:
            with self.subTest(edge=edge):
                root, packages = self.local_edge_fixture(
                    dependency_definition=dependency_definition,
                    workspace_dependencies=workspace_dependencies,
                )
                with self.assertRaisesRegex(InventoryError, f"explicitly declare.*{package}"):
                    local_graph(root, packages)

                root, packages = self.local_edge_fixture(
                    dependency_definition=dependency_definition,
                    workspace_dependencies=workspace_dependencies,
                    labels=(label,),
                )
                self.assertEqual(local_graph(root, packages)["consumer"], expected_graph)

        root, packages = self.local_edge_fixture(
            dependency_definition=(
                "[dependencies]\n"
                'ctx-history-jsonl = { path = "../crates/ctx-history-jsonl" }\n'
            ),
            labels=("//crates/ctx-history-jsonl:other",),
        )
        with self.assertRaisesRegex(InventoryError, "explicitly declare.*ctx-history-jsonl"):
            local_graph(root, packages)

    def test_explicit_target_does_not_hide_implicit_targets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            (package / "src/bin").mkdir(parents=True)
            (package / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
            (package / "src/bin/implicit.rs").write_text("fn main() {}\n", encoding="utf-8")
            (package / "explicit.rs").write_text("fn main() {}\n", encoding="utf-8")
            targets = cargo_targets(
                package,
                {
                    "package": {"name": "fixture"},
                    "bin": [{"name": "explicit", "path": "explicit.rs"}],
                },
            )
        self.assertEqual(
            targets,
            {
                "bin:explicit": Path("explicit.rs"),
                "bin:fixture": Path("src/main.rs"),
                "bin:implicit": Path("src/bin/implicit.rs"),
            },
        )

    def test_source_ownership_requires_a_structural_rule_attribute(self) -> None:
        misleading = module(
            '''
# crate_root = "src/main.rs"
notice = "src/main.rs"
filegroup(name = "cargo_package_data", data = ["src/main.rs"])
'''
        )
        self.assertFalse(rust_source_owned([misleading], "src/main.rs"))
        broad_filegroup = module(
            'filegroup(name = "cargo_package_data", srcs = glob(["**"]))'
        )
        self.assertFalse(rust_source_owned([broad_filegroup], "src/main.rs"))
        owned = module('rust_binary(name = "app", crate_root = "src/main.rs")')
        self.assertTrue(rust_source_owned([owned], "src/main.rs"))

    def test_glob_source_ownership_honors_excludes(self) -> None:
        metadata = module(
            '''
SOURCES = glob(["src/**/*.rs"], exclude = ["src/private/**"])
rust_library(name = "lib", srcs = SOURCES)
'''
        )
        self.assertTrue(rust_source_owned([metadata], "src/nested/lib.rs"))
        self.assertFalse(rust_source_owned([metadata], "src/private/secret.rs"))

    def test_build_script_must_be_structurally_declared(self) -> None:
        misleading = module('notice = "build.rs"')
        self.assertFalse(bazel_path_declared([misleading], "build.rs"))
        exported = module('exports_files(["Cargo.toml", "build.rs"])')
        self.assertTrue(bazel_path_declared([exported], "build.rs"))

    def test_all_crate_deps_only_covers_requested_dependency_class(self) -> None:
        metadata = module(
            '''
rust_library(
    name = "lib",
    deps = all_crate_deps(normal = True),
)
'''
        )
        labels, flags = dependency_ownership([metadata])
        self.assertEqual(labels, set())
        self.assertEqual(flags, {"normal"})
        self.assertNotIn("normal_dev", flags)
        self.assertNotIn("build", flags)

    def test_dependency_labels_come_only_from_dependency_attributes(self) -> None:
        misleading = module('notice = "//crates/unowned:lib"')
        labels, _ = dependency_ownership([misleading])
        self.assertEqual(labels, set())
        owned = module(
            'rust_library(name = "lib", deps = ["//crates/owned:lib"])'
        )
        labels, _ = dependency_ownership([owned])
        self.assertEqual(labels, {"//crates/owned:lib"})

    def test_dependency_ownership_is_scoped_to_the_named_rust_target(self) -> None:
        metadata = module(
            '''
rust_library(name = "lib", crate_root = "src/lib.rs", deps = [])
ctx_rust_test(
    name = "unit_tests",
    crate_root = "src/lib.rs",
    deps = all_crate_deps(normal = True, normal_dev = True),
)
'''
        )
        labels, flags = dependency_ownership(
            [metadata],
            target_name="lib",
            target_path="src/lib.rs",
        )
        self.assertEqual(labels, set())
        self.assertEqual(flags, set())
        _, test_flags = dependency_ownership([metadata], tests_only=True)
        self.assertEqual(test_flags, {"normal", "normal_dev"})

    def test_live_manifest_discovery_includes_untracked_and_ignores_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "-q", root], check=True)
            for relative, name in (
                ("crates/tracked/Cargo.toml", "tracked"),
                ("crates/untracked/Cargo.toml", "untracked"),
                ("ignored/Cargo.toml", "ignored"),
            ):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(f'[package]\nname = "{name}"\n', encoding="utf-8")
            (root / ".gitignore").write_text("ignored/\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", root, "add", ".gitignore", "crates/tracked/Cargo.toml"],
                check=True,
            )
            manifests = live_package_manifests(root)
        self.assertEqual(
            manifests,
            {
                Path("crates/tracked/Cargo.toml"),
                Path("crates/untracked/Cargo.toml"),
            },
        )


if __name__ == "__main__":
    unittest.main()
