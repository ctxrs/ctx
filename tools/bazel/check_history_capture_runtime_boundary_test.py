#!/usr/bin/env python3
from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_history_capture_runtime_boundary import validate


WORKSPACE_CARGO = """\
[workspace]

[workspace.dependencies]
chrono = "0.4"
tempfile = "3"
uuid = "1"
thiserror = "1"
serde = "1"
serde_json = "1"
sha2 = "1"
"""

RUNTIME_CARGO = """\
[dependencies]
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-core = { path = "../ctx-history-core" }
thiserror.workspace = true
uuid.workspace = true
"""

JSONL_CARGO = """\
[dependencies]
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-capture-runtime = { path = "../ctx-history-capture-runtime" }
ctx-history-core = { path = "../ctx-history-core" }
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
"""

RUNTIME_BUILD = """\
load("@crates//:defs.bzl", "aliases", "all_crate_deps", "crate_edition")
load("@rules_rust//rust:defs.bzl", "rust_library")
load("//:rust_sources.bzl", "RUST_PROD_SRC_EXCLUDES")
load("//tools/bazel:ctx_rust.bzl", "ctx_rust_test")

RUNTIME_DEPS = [
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-core:lib",
]

rust_library(
    name = "lib",
    deps = all_crate_deps(normal = True) + RUNTIME_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True),
)

ctx_rust_test(
    name = "unit_tests",
    deps = all_crate_deps(normal = True, normal_dev = True) + RUNTIME_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
)
"""

JSONL_BUILD = """\
load("@crates//:defs.bzl", "aliases", "all_crate_deps", "crate_edition")
load("@rules_rust//rust:defs.bzl", "rust_library")
load("//:rust_sources.bzl", "RUST_PROD_SRC_EXCLUDES")
load("//tools/bazel:ctx_rust.bzl", "ctx_rust_test")

JSONL_DEPS = [
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-io:lib",
]

JSONL_TEST_DEPS = [
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-io:test_support_lib",
]

rust_library(
    name = "lib",
    deps = all_crate_deps(normal = True) + JSONL_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True),
)

rust_library(
    name = "test_support_lib",
    testonly = True,
    deps = all_crate_deps(normal = True) + JSONL_TEST_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True),
    rustc_flags = ['--cfg=feature="test-support"'],
)

ctx_rust_test(
    name = "unit_tests",
    deps = all_crate_deps(normal = True, normal_dev = True) + JSONL_TEST_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
)
"""

PROVIDER_CARGO = """\
[dependencies]
chrono.workspace = true
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-capture-runtime = { path = "../ctx-history-capture-runtime" }
ctx-history-core = { path = "../ctx-history-core" }
ctx-history-jsonl = { path = "../ctx-history-jsonl" }
ctx-history-native-jsonl-parsers = { path = "../ctx-history-native-jsonl-parsers" }
ctx-history-source-io = { path = "../ctx-history-source-io" }
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[dev-dependencies]
ctx-history-jsonl = { path = "../ctx-history-jsonl", features = ["test-support"] }
ctx-history-source-io = { path = "../ctx-history-source-io", features = ["test-support"] }
tempfile.workspace = true
uuid.workspace = true
"""

PROVIDER_BUILD = """\
load("@crates//:defs.bzl", "aliases", "all_crate_deps", "crate_edition")
load("@rules_rust//rust:defs.bzl", "rust_library")
load("//:rust_sources.bzl", "RUST_PROD_SRC_EXCLUDES")
load("//tools/bazel:ctx_rust.bzl", "ctx_rust_test")

NATIVE_JSONL_DEPS = [
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-jsonl:lib",
    "//crates/ctx-history-native-jsonl-parsers:lib",
    "//crates/ctx-history-source-io:lib",
]

NATIVE_JSONL_TEST_DEPS = [
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-jsonl:test_support_lib",
    "//crates/ctx-history-native-jsonl-parsers:lib",
    "//crates/ctx-history-source-io:test_support_lib",
]

rust_library(
    name = "lib",
    deps = all_crate_deps(normal = True) + NATIVE_JSONL_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True),
)

rust_library(
    name = "test_support_lib",
    testonly = True,
    deps = all_crate_deps(normal = True, normal_dev = True) + NATIVE_JSONL_TEST_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
)

ctx_rust_test(
    name = "unit_tests",
    deps = all_crate_deps(normal = True, normal_dev = True) + NATIVE_JSONL_TEST_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
)
"""


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.workspace_manifest = root / "Cargo.toml"
        self.runtime_manifest = root / "runtime-Cargo.toml"
        self.runtime_build = root / "runtime-BUILD.bazel"
        self.jsonl_manifest = root / "jsonl-Cargo.toml"
        self.jsonl_build = root / "jsonl-BUILD.bazel"
        self.provider_manifest = root / "provider-Cargo.toml"
        self.provider_build = root / "provider-BUILD.bazel"
        self.workspace_manifest.write_text(WORKSPACE_CARGO, encoding="utf-8")
        self.runtime_manifest.write_text(RUNTIME_CARGO, encoding="utf-8")
        self.runtime_build.write_text(RUNTIME_BUILD, encoding="utf-8")
        self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")
        self.jsonl_build.write_text(JSONL_BUILD, encoding="utf-8")
        self.provider_manifest.write_text(PROVIDER_CARGO, encoding="utf-8")
        self.provider_build.write_text(PROVIDER_BUILD, encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate(
            self.workspace_manifest,
            self.runtime_manifest,
            self.runtime_build,
            self.jsonl_manifest,
            self.jsonl_build,
            self.provider_manifest,
            self.provider_build,
        )

    def test_minimal_runtime_boundary_passes(self) -> None:
        self.validate()

    def test_package_rename_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO
            + '\n[dev-dependencies]\nindex_alias = { package = "ctx-history-index", path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_jsonl_source_sqlite_dependency_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO
            + '\n[dev-dependencies]\nctx-history-source-sqlite = { path = "../ctx-history-source-sqlite" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_workspace_inherited_package_rename_is_rejected(self) -> None:
        self.workspace_manifest.write_text(
            WORKSPACE_CARGO
            + '\nindex_alias = { package = "ctx-history-index", version = "1" }\n',
            encoding="utf-8",
        )
        self.jsonl_manifest.write_text(
            JSONL_CARGO + "\n[dev-dependencies]\nindex_alias.workspace = true\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_normal_dev_and_build_dependency_variants_are_rejected(self) -> None:
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            with self.subTest(table=table):
                addition = (
                    'ctx-history-index = { path = "../ctx-history-index" }\n'
                    if table == "dependencies"
                    else f'\n[{table}]\nctx-history-index = {{ path = "../ctx-history-index" }}\n'
                )
                self.jsonl_manifest.write_text(
                    JSONL_CARGO + addition,
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
                    self.validate()
                self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")

    def test_target_specific_normal_dev_and_build_variants_are_rejected(self) -> None:
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            with self.subTest(table=table):
                self.jsonl_manifest.write_text(
                    JSONL_CARGO
                    + f"\n[target.'cfg(unix)'.{table}]\n"
                    + 'index_alias = { package = "ctx-history-index-format", path = "../ctx-history-index-format" }\n',
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
                    self.validate()
                self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")

    def test_ambiguous_workspace_dependency_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO
            + '\n[dev-dependencies]\nindex_alias = { package = "ctx-history-index", workspace = true }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "cannot combine workspace inheritance"):
            self.validate()

    def test_malformed_dependency_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO + "\n[dev-dependencies]\nindex_alias = 1\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "must be a string or inline table"):
            self.validate()

    def test_malformed_workspace_branches_are_rejected(self) -> None:
        cases = (
            (
                "missing workspace",
                '[package]\nname = "fixture"\n',
                "root Cargo manifest must define a workspace table",
            ),
            (
                "non-table dependencies",
                "workspace = { dependencies = 1 }\n",
                "workspace.dependencies must be a table",
            ),
            (
                "missing inherited dependency",
                WORKSPACE_CARGO.replace('sha2 = "1"\n', ""),
                "absent from root workspace.dependencies",
            ),
            (
                "malformed inherited dependency",
                WORKSPACE_CARGO.replace('sha2 = "1"', "sha2 = 1"),
                "must be a string or inline table",
            ),
            (
                "recursive workspace inheritance",
                WORKSPACE_CARGO.replace(
                    'sha2 = "1"', "sha2 = { workspace = true }"
                ),
                "cannot inherit from workspace.dependencies",
            ),
            (
                "invalid workspace package rename",
                WORKSPACE_CARGO.replace('sha2 = "1"', 'sha2 = { package = "" }'),
                "invalid package rename",
            ),
        )
        for name, workspace, error in cases:
            with self.subTest(name=name):
                self.workspace_manifest.write_text(workspace, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, error):
                    self.validate()
                self.workspace_manifest.write_text(WORKSPACE_CARGO, encoding="utf-8")

    def test_malformed_workspace_inheritance_flags_are_rejected(self) -> None:
        for flag, error in (
            ('"yes"', "non-boolean workspace inheritance flag"),
            ("false", "ambiguous workspace = false entry"),
        ):
            with self.subTest(flag=flag):
                self.jsonl_manifest.write_text(
                    JSONL_CARGO.replace(
                        "sha2.workspace = true", f"sha2 = {{ workspace = {flag} }}"
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, error):
                    self.validate()
                self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")

    def test_runtime_build_dependency_outside_allowlist_is_rejected(self) -> None:
        self.runtime_manifest.write_text(
            RUNTIME_CARGO + '\n[build-dependencies]\ncc = "1"\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "Cargo build dependencies drifted"):
            self.validate()

    def test_runtime_target_dependency_evasion_is_rejected(self) -> None:
        self.runtime_manifest.write_text(
            RUNTIME_CARGO
            + "\n[target.'cfg(unix)'.dependencies]\n"
            + 'ctx-history-index = { path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_runtime_forbidden_authorities_are_rejected(self) -> None:
        for package in (
            "ctx-history-index-format",
            "ctx-history-index",
            "ctx-history-capture",
            "ctx-history-jsonl",
        ):
            with self.subTest(package=package):
                self.runtime_manifest.write_text(
                    RUNTIME_CARGO
                    + f'\n[build-dependencies]\nforbidden = {{ package = "{package}", path = "../forbidden" }}\n',
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
                    self.validate()
                self.runtime_manifest.write_text(RUNTIME_CARGO, encoding="utf-8")

    def test_runtime_direct_bazel_dependencies_cannot_be_augmented_or_reassigned(self) -> None:
        for mutation in (
            'RUNTIME_DEPS += ["//crates/ctx-history-index:lib"]\n',
            'RUNTIME_DEPS = RUNTIME_DEPS + ["//crates/ctx-history-index:lib"]\n',
            'COPIED_DEPS = RUNTIME_DEPS\n',
        ):
            with self.subTest(mutation=mutation):
                self.runtime_build.write_text(
                    RUNTIME_BUILD.replace(
                        "\nrust_library(", f"\n{mutation}\nrust_library("
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "RUNTIME_DEPS"):
                    self.validate()
                self.runtime_build.write_text(RUNTIME_BUILD, encoding="utf-8")

    def test_composed_bazel_label_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            JSONL_BUILD.replace(
                "all_crate_deps(normal = True) + JSONL_DEPS",
                'all_crate_deps(normal = True) + ["//crates/ctx-history-index:lib"]',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "Bazel rust_library deps must be exactly"):
            self.validate()

    def test_runtime_production_dependencies_are_enforced(self) -> None:
        self.runtime_build.write_text(
            RUNTIME_BUILD.replace(
                "all_crate_deps(normal = True) + RUNTIME_DEPS,",
                'all_crate_deps(normal = True) + ["//crates/ctx-history-capture:lib"],',
                1,
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "Bazel rust_library deps must be exactly"):
            self.validate()

    def test_loaded_bazel_label_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            'load("//tools:deps.bzl", "JSONL_DEPS")\n' + JSONL_BUILD,
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "local literal"):
            self.validate()

    def test_jsonl_deps_augmentation_and_reassignment_are_rejected(self) -> None:
        for name, mutation in (
            (
                "augmented assignment",
                'JSONL_DEPS += ["//crates/ctx-history-index:lib"]\n',
            ),
            (
                "reassignment",
                'JSONL_DEPS = JSONL_DEPS + ["//crates/ctx-history-index:lib"]\n',
            ),
            ("alias reference", "COPIED_DEPS = JSONL_DEPS\n"),
        ):
            with self.subTest(name=name):
                self.jsonl_build.write_text(
                    JSONL_BUILD.replace(
                        "\nrust_library(", f"\n{mutation}\nrust_library("
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "JSONL_DEPS"):
                    self.validate()

    def test_jsonl_test_support_target_is_enforced(self) -> None:
        cases = (
            (
                "name",
                'name = "test_support_lib"',
                'name = "concealed_support"',
                "test-support rust_library must be named",
            ),
            (
                "testonly",
                "testonly = True",
                "testonly = False",
                "test-support rust_library must be testonly",
            ),
            (
                "feature",
                '''rustc_flags = ['--cfg=feature="test-support"']''',
                '''rustc_flags = ['--cfg=feature="concealed"']''',
                "must enable only the test-support feature",
            ),
            (
                "dependencies",
                "all_crate_deps(normal = True) + JSONL_TEST_DEPS",
                'all_crate_deps(normal = True) + ["//crates/ctx-history-index:lib"]',
                "test-support rust_library deps",
            ),
        )
        for name, allowed, forbidden, error in cases:
            with self.subTest(name=name):
                self.jsonl_build.write_text(
                    JSONL_BUILD.replace(allowed, forbidden, 1),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, error):
                    self.validate()
                self.jsonl_build.write_text(JSONL_BUILD, encoding="utf-8")

    def test_trusted_symbols_require_canonical_load_sources(self) -> None:
        cases = (
            ("all_crate_deps", "@crates//:defs.bzl"),
            ("rust_library", "@rules_rust//rust:defs.bzl"),
            ("ctx_rust_test", "//tools/bazel:ctx_rust.bzl"),
        )
        for symbol, source in cases:
            with self.subTest(symbol=symbol):
                self.runtime_build.write_text(
                    RUNTIME_BUILD.replace(
                        f'load("{source}"',
                        'load("//untrusted:defs.bzl"',
                        1,
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "canonical source"):
                    self.validate()
                self.runtime_build.write_text(RUNTIME_BUILD, encoding="utf-8")

    def test_trusted_symbols_cannot_be_rebound(self) -> None:
        for symbol in ("all_crate_deps", "rust_library", "ctx_rust_test"):
            with self.subTest(symbol=symbol):
                self.runtime_build.write_text(
                    RUNTIME_BUILD + f"\n{symbol} = concealed_symbol\n",
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "may not be rebound"):
                    self.validate()
                self.runtime_build.write_text(RUNTIME_BUILD, encoding="utf-8")

    def test_trusted_symbols_cannot_be_loaded_through_aliases(self) -> None:
        for symbol, source in (
            ("all_crate_deps", "@crates//:defs.bzl"),
            ("rust_library", "@rules_rust//rust:defs.bzl"),
            ("ctx_rust_test", "//tools/bazel:ctx_rust.bzl"),
        ):
            with self.subTest(symbol=symbol):
                self.runtime_build.write_text(
                    RUNTIME_BUILD.replace(
                        f'"{symbol}"',
                        f'concealed = "{symbol}"',
                        1,
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "without aliasing"):
                    self.validate()
                self.runtime_build.write_text(RUNTIME_BUILD, encoding="utf-8")

    def test_raw_rust_test_and_binary_calls_are_rejected_in_both_packages(self) -> None:
        for build_path, source, forbidden_label in (
            (
                self.runtime_build,
                RUNTIME_BUILD,
                "//crates/ctx-history-index:lib",
            ),
            (
                self.jsonl_build,
                JSONL_BUILD,
                "//crates/ctx-history-index-format:lib",
            ),
        ):
            for rule in ("rust_test", "rust_binary"):
                with self.subTest(build=build_path.name, rule=rule):
                    build_path.write_text(
                        source
                        + f"""\
{rule}(
    name = "concealed_target",
    deps = ["{forbidden_label}"],
    proc_macro_deps = ["{forbidden_label}"],
)
""",
                        encoding="utf-8",
                    )
                    with self.assertRaisesRegex(
                        ValueError, f"unsupported rule or macro call: {rule}"
                    ):
                        self.validate()
                    build_path.write_text(source, encoding="utf-8")

    def test_unsupported_rules_rust_load_bindings_are_rejected_in_both_packages(
        self,
    ) -> None:
        for build_path, source in (
            (self.runtime_build, RUNTIME_BUILD),
            (self.jsonl_build, JSONL_BUILD),
        ):
            with self.subTest(build=build_path.name):
                build_path.write_text(
                    source.replace(
                        'load("@rules_rust//rust:defs.bzl", "rust_library")',
                        'load("@rules_rust//rust:defs.bzl", "rust_binary", "rust_library", "rust_test")',
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "load bindings drifted"):
                    self.validate()
                build_path.write_text(source, encoding="utf-8")

    def test_custom_rust_macro_calls_are_rejected_in_both_packages(self) -> None:
        for build_path, source, forbidden_label in (
            (
                self.runtime_build,
                RUNTIME_BUILD,
                "//crates/ctx-history-index:lib",
            ),
            (
                self.jsonl_build,
                JSONL_BUILD,
                "//crates/ctx-history-index-format:lib",
            ),
        ):
            with self.subTest(build=build_path.name):
                build_path.write_text(
                    source
                    + f"""\
custom_rust_target(
    name = "concealed_target",
    deps = ["{forbidden_label}"],
    proc_macro_deps = ["{forbidden_label}"],
)
""",
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(
                    ValueError, "unsupported rule or macro call: custom_rust_target"
                ):
                    self.validate()
                build_path.write_text(source, encoding="utf-8")

    def test_custom_macro_loads_are_rejected_in_both_packages(self) -> None:
        for build_path, source in (
            (self.runtime_build, RUNTIME_BUILD),
            (self.jsonl_build, JSONL_BUILD),
        ):
            with self.subTest(build=build_path.name):
                build_path.write_text(
                    'load("//untrusted:rust.bzl", "custom_rust_target")\n'
                    + source,
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "unsupported load source"):
                    self.validate()
                build_path.write_text(source, encoding="utf-8")

    def test_ctx_rust_test_dependencies_are_enforced_for_both_packages(self) -> None:
        cases = (
            (
                self.runtime_build,
                RUNTIME_BUILD,
                "all_crate_deps(normal = True, normal_dev = True)",
                'all_crate_deps(normal = True, normal_dev = True) + ["//crates/ctx-history-index:lib"]',
            ),
            (
                self.jsonl_build,
                JSONL_BUILD,
                "all_crate_deps(normal = True, normal_dev = True) + JSONL_TEST_DEPS",
                'all_crate_deps(normal = True, normal_dev = True) + JSONL_TEST_DEPS + ["//crates/ctx-history-index-format:lib"]',
            ),
        )
        for build_path, source, allowed, forbidden in cases:
            with self.subTest(build=build_path.name):
                build_path.write_text(
                    source.replace(allowed, forbidden),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "ctx_rust_test #1 deps"):
                    self.validate()
                build_path.write_text(source, encoding="utf-8")

    def test_every_ctx_rust_test_is_enforced(self) -> None:
        self.runtime_build.write_text(
            RUNTIME_BUILD
            + """\
ctx_rust_test(
    name = "extra_tests",
    deps = all_crate_deps(normal = True, normal_dev = True) + ["//crates/ctx-history-index:lib"],
    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
)
""",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "ctx_rust_test #2 deps"):
            self.validate()

    def test_production_proc_macro_dependencies_are_enforced(self) -> None:
        for build_path, source, forbidden_label in (
            (
                self.runtime_build,
                RUNTIME_BUILD,
                "//crates/ctx-history-index:lib",
            ),
            (
                self.jsonl_build,
                JSONL_BUILD,
                "//crates/ctx-history-index-format:lib",
            ),
        ):
            with self.subTest(build=build_path.name):
                build_path.write_text(
                    source.replace(
                        "all_crate_deps(proc_macro = True),",
                        f'all_crate_deps(proc_macro = True) + ["{forbidden_label}"],',
                        1,
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "rust_library proc_macro_deps"):
                    self.validate()
                build_path.write_text(source, encoding="utf-8")

    def test_ctx_rust_test_proc_macro_dependencies_are_enforced(self) -> None:
        for build_path, source, forbidden_label in (
            (
                self.runtime_build,
                RUNTIME_BUILD,
                "//crates/ctx-history-index:lib",
            ),
            (
                self.jsonl_build,
                JSONL_BUILD,
                "//crates/ctx-history-index-format:lib",
            ),
        ):
            with self.subTest(build=build_path.name):
                build_path.write_text(
                    source.replace(
                        "all_crate_deps(proc_macro = True, proc_macro_dev = True)",
                        "all_crate_deps(proc_macro = True, proc_macro_dev = True) "
                        f'+ ["{forbidden_label}"]',
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(
                    ValueError, "ctx_rust_test #1 proc_macro_deps"
                ):
                    self.validate()
                build_path.write_text(source, encoding="utf-8")

    def test_bazel_comments_do_not_create_false_positive(self) -> None:
        self.jsonl_build.write_text(
            JSONL_BUILD
            + '# "//crates/ctx-history-index:lib" must remain forbidden\n',
            encoding="utf-8",
        )
        self.validate()

    def test_jsonl_index_format_direct_label_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            JSONL_BUILD.replace(
                '"//crates/ctx-history-core:lib",',
                '"//crates/ctx-history-index-format:lib",',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "direct dependency inventory drifted"):
            self.validate()

    def test_provider_forbidden_authorities_are_rejected(self) -> None:
        for package in (
            "ctx-history-capture",
            "ctx-history-index",
            "ctx-history-index-format",
            "ctx-history-index-query",
        ):
            with self.subTest(package=package):
                self.provider_manifest.write_text(
                    PROVIDER_CARGO
                    + f'\n[build-dependencies]\nforbidden = {{ package = "{package}", path = "../forbidden" }}\n',
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
                    self.validate()
                self.provider_manifest.write_text(PROVIDER_CARGO, encoding="utf-8")

    def test_provider_direct_bazel_dependencies_cannot_be_augmented_or_reassigned(self) -> None:
        for mutation in (
            'NATIVE_JSONL_DEPS += ["//crates/ctx-history-index:lib"]\n',
            'NATIVE_JSONL_TEST_DEPS = NATIVE_JSONL_TEST_DEPS + ["//crates/ctx-history-capture:lib"]\n',
            'COPIED_DEPS = NATIVE_JSONL_DEPS\n',
        ):
            with self.subTest(mutation=mutation):
                self.provider_build.write_text(
                    PROVIDER_BUILD.replace(
                        "\nrust_library(", f"\n{mutation}\nrust_library("
                    ),
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "NATIVE_JSONL_"):
                    self.validate()
                self.provider_build.write_text(PROVIDER_BUILD, encoding="utf-8")

    def test_cli_returns_nonzero_for_invalid_input(self) -> None:
        self.jsonl_manifest.write_text("[dependencies\n", encoding="utf-8")
        completed = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).with_name("check_history_capture_runtime_boundary.py")),
                str(self.workspace_manifest),
                str(self.runtime_manifest),
                str(self.runtime_build),
                str(self.jsonl_manifest),
                str(self.jsonl_build),
                str(self.provider_manifest),
                str(self.provider_build),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("Expected ']'", completed.stderr)


if __name__ == "__main__":
    unittest.main()
