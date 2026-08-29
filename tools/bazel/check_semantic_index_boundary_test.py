#!/usr/bin/env python3

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_semantic_index_boundary import BoundaryError, validate


CARGO = """\
[features]
default = []
test-support = []

[dependencies]
anyhow.workspace = true
ctx-history-core = { path = "../ctx-history-core" }
ctx-history-index = { path = "../ctx-history-index" }
ctx-history-platform = { path = "../ctx-history-platform" }
ctx-semantic-model = { path = "../ctx-semantic-model" }
fs2 = "0.4.3"
memmap2.workspace = true
rusqlite.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true
url.workspace = true
uuid.workspace = true

[dev-dependencies]
tempfile.workspace = true
"""

BUILD = """\
filegroup(
    name = "cargo_package_data",
    srcs = glob(["**"], exclude = ["BUILD.bazel"]),
)

CTX_SEMANTIC_INDEX_DEPS = [
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-index:lib",
    "//crates/ctx-history-platform:lib",
    "//crates/ctx-semantic-model:lib",
]

rust_library(
    name = "lib",
    srcs = PROD_SRCS,
    deps = all_crate_deps(normal = True) + CTX_SEMANTIC_INDEX_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True),
    rustc_flags = CTX_SEMANTIC_INDEX_RUSTC_FLAGS,
)
rust_library(
    name = "test_support_lib",
    srcs = PROD_SRCS,
    deps = all_crate_deps(normal = True) + CTX_SEMANTIC_INDEX_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True),
    rustc_flags = CTX_SEMANTIC_INDEX_RUSTC_FLAGS + ['--cfg=feature="test-support"'],
)
ctx_rust_test(
    name = "unit_tests",
    srcs = RUST_SRCS,
    deps = all_crate_deps(normal = True, normal_dev = True) + CTX_SEMANTIC_INDEX_DEPS,
    proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
    rustc_flags = CTX_SEMANTIC_INDEX_RUSTC_FLAGS,
)
"""


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        package = self.root / "crates/ctx-semantic-index"
        package.mkdir(parents=True)
        (package / "Cargo.toml").write_text(CARGO, encoding="utf-8")
        (package / "BUILD.bazel").write_text(BUILD, encoding="utf-8")
        (self.root / "crates/ctx-cli/src/semantic").mkdir(parents=True)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_current_contract_passes(self) -> None:
        validate(self.root)

    def test_unmodeled_path_dependency_fails(self) -> None:
        cargo = self.root / "crates/ctx-semantic-index/Cargo.toml"
        cargo.write_text(
            cargo.read_text(encoding="utf-8")
            .replace("fs2 =", 'ctx-protocol = { path = "../ctx-protocol" }\nfs2 ='),
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_unreviewed_external_dependency_fails(self) -> None:
        cargo = self.root / "crates/ctx-semantic-index/Cargo.toml"
        cargo.write_text(
            cargo.read_text(encoding="utf-8").replace("fs2 =", 'rand = "0.9"\nfs2 ='),
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_build_dependency_bypass_fails(self) -> None:
        cargo = self.root / "crates/ctx-semantic-index/Cargo.toml"
        cargo.write_text(
            cargo.read_text(encoding="utf-8")
            + "\n[build-dependencies]\ncc = \"1\"\n",
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_target_dependency_bypass_fails(self) -> None:
        cargo = self.root / "crates/ctx-semantic-index/Cargo.toml"
        cargo.write_text(
            cargo.read_text(encoding="utf-8")
            + "\n[target.'cfg(unix)'.dependencies]\nnix = \"0.30\"\n",
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_cargo_feature_drift_fails(self) -> None:
        cargo = self.root / "crates/ctx-semantic-index/Cargo.toml"
        cargo.write_text(
            cargo.read_text(encoding="utf-8").replace("test-support = []", "test-support = []\nextra = []"),
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_bazel_feature_drift_fails(self) -> None:
        build = self.root / "crates/ctx-semantic-index/BUILD.bazel"
        build.write_text(
            build.read_text(encoding="utf-8").replace(
                "CTX_SEMANTIC_INDEX_RUSTC_FLAGS + ['--cfg=feature=\"test-support\"']",
                "CTX_SEMANTIC_INDEX_RUSTC_FLAGS",
            ),
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_nested_cli_duplicate_fails(self) -> None:
        duplicate = self.root / "crates/ctx-cli/src/semantic/vector_store/new.rs"
        duplicate.parent.mkdir(parents=True)
        duplicate.write_text("pub fn duplicate() {}\n", encoding="utf-8")
        with self.assertRaises(BoundaryError):
            validate(self.root)

    def test_renamed_cli_duplicate_fails(self) -> None:
        duplicate = self.root / "crates/ctx-cli/src/semantic/semantic_store.rs"
        duplicate.write_text("pub fn duplicate() {}\n", encoding="utf-8")
        with self.assertRaises(BoundaryError):
            validate(self.root)


if __name__ == "__main__":
    unittest.main()
