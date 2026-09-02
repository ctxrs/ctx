#!/usr/bin/env python3

import ast
from pathlib import Path
import subprocess
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


class RustTargetInventoryTest(unittest.TestCase):
    def protected_edge_fixture(
        self,
        *,
        direct_dependencies: str = "",
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
        consumer.joinpath("BUILD.bazel").write_text(
            "rust_library(\n"
            '    name = "lib",\n'
            '    crate_root = "src/lib.rs",\n'
            "    deps = all_crate_deps(normal = True)"
            + (" + [" + ", ".join(repr(label) for label in labels) + "]" if labels else "")
            + ",\n)\n",
            encoding="utf-8",
        )
        consumer_data: dict[str, object] = {"package": {"name": "consumer"}}
        if direct_dependencies:
            consumer_data["dependencies"] = tomllib.loads(direct_dependencies)["dependencies"]

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

    def test_direct_renamed_protected_edge_needs_an_explicit_bazel_label(self) -> None:
        root, packages = self.protected_edge_fixture(
            direct_dependencies=(
                "[dependencies]\n"
                'history_jsonl = { package = "ctx-history-jsonl", path = "../crates/ctx-history-jsonl" }\n'
            ),
        )
        with self.assertRaisesRegex(InventoryError, "explicitly declare protected.*ctx-history-jsonl"):
            local_graph(root, packages)

    def test_workspace_inherited_renamed_protected_edge_needs_an_explicit_bazel_label(self) -> None:
        root, packages = self.protected_edge_fixture(
            direct_dependencies="[dependencies]\nhistory_sqlite.workspace = true\n",
            workspace_dependencies=(
                "[workspace.dependencies]\n"
                'history_sqlite = { package = "ctx-history-source-sqlite", path = "crates/ctx-history-source-sqlite" }\n'
            ),
        )
        with self.assertRaisesRegex(InventoryError, "explicitly declare protected.*ctx-history-source-sqlite"):
            local_graph(root, packages)

    def test_renamed_protected_edges_accept_explicit_bazel_labels(self) -> None:
        root, packages = self.protected_edge_fixture(
            direct_dependencies=(
                "[dependencies]\n"
                'history_jsonl = { package = "ctx-history-jsonl", path = "../crates/ctx-history-jsonl" }\n'
                "history_sqlite.workspace = true\n"
            ),
            workspace_dependencies=(
                "[workspace.dependencies]\n"
                'history_sqlite = { package = "ctx-history-source-sqlite", path = "crates/ctx-history-source-sqlite" }\n'
            ),
            labels=(
                "//crates/ctx-history-jsonl:lib",
                "//crates/ctx-history-source-sqlite:lib",
            ),
        )
        self.assertEqual(
            local_graph(root, packages)["consumer"],
            {"ctx-history-jsonl", "ctx-history-source-sqlite"},
        )

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
