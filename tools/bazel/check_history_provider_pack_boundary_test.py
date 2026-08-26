#!/usr/bin/env python3
from __future__ import annotations

import shutil
import tempfile
import unittest
from pathlib import Path

from check_history_provider_pack_boundary import (
    BoundaryError,
    EVALUATED_REVERSE_BAZEL_CONSUMERS,
    LIVE_BOUNDARY_TARGET,
    MUTATION_BOUNDARY_TARGET,
    PACK_LABEL,
    validate,
    validate_evaluated_reverse_bazel_consumers,
)


REPOSITORY = Path(__file__).resolve().parents[2]


class ProviderPackBoundaryMutations(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copy2(REPOSITORY / "Cargo.toml", self.root / "Cargo.toml")
        shutil.copy2(REPOSITORY / "BUILD.bazel", self.root / "BUILD.bazel")
        tools_build = self.root / "tools/bazel/BUILD.bazel"
        tools_build.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(
            REPOSITORY / "tools/bazel/BUILD.bazel", tools_build
        )
        workspace_inputs = list((REPOSITORY / "crates").glob("*/Cargo.toml"))
        workspace_inputs.extend((REPOSITORY / "crates").glob("*/BUILD.bazel"))
        for source in workspace_inputs:
            destination = self.root / source.relative_to(REPOSITORY)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @property
    def pack_cargo(self) -> Path:
        return self.root / "crates/ctx-history-providers-task-docs/Cargo.toml"

    @property
    def pack_build(self) -> Path:
        return self.root / "crates/ctx-history-providers-task-docs/BUILD.bazel"

    @property
    def capture_cargo(self) -> Path:
        return self.root / "crates/ctx-history-capture-composition/Cargo.toml"

    @property
    def capture_build(self) -> Path:
        return self.root / "crates/ctx-history-capture-composition/BUILD.bazel"

    def member_cargos(self) -> tuple[Path, ...]:
        return tuple(sorted((self.root / "crates").glob("*/Cargo.toml")))

    def member_builds(self) -> tuple[Path, ...]:
        return (
            self.root / "BUILD.bazel",
            *sorted((self.root / "crates").glob("*/BUILD.bazel")),
        )

    def validate(self) -> None:
        validate(
            self.root / "Cargo.toml",
            self.pack_cargo,
            self.pack_build,
            self.capture_cargo,
            self.capture_build,
            self.member_cargos(),
            self.member_builds(),
        )

    def replace(self, path: Path, before: str, after: str) -> None:
        source = path.read_text(encoding="utf-8")
        self.assertIn(before, source)
        path.write_text(source.replace(before, after, 1), encoding="utf-8")

    def test_clean_head_passes(self) -> None:
        self.validate()

    def test_live_registration_and_default_ci_routes_cannot_disappear(self) -> None:
        cases = (
            (
                self.root / "BUILD.bazel",
                f'name = "{LIVE_BOUNDARY_TARGET.removeprefix("//:")}",',
                'name = "removed_provider_pack_boundary_check",',
                "must define exactly one",
            ),
            (
                self.root / "BUILD.bazel",
                '    data = [\n        ":history_provider_pack_boundary_root_build",\n        ":history_provider_pack_boundary_inputs",\n    ],\n',
                "    data = [],\n",
                "complete Cargo/BUILD data registration drifted",
            ),
            (
                self.root / "BUILD.bazel",
                '    srcs = ["scripts/bazelw"] + CARGO_WORKSPACE_PACKAGE_DATA,\n',
                '    srcs = ["scripts/bazelw"],\n',
                "input filegroup srcs",
            ),
            (
                self.root / "tools/bazel/BUILD.bazel",
                '    data = ["//:history_provider_pack_boundary_inputs"],\n',
                "    data = [],\n",
                "complete Cargo/BUILD data drifted",
            ),
            (
                self.root / "tools/bazel/BUILD.bazel",
                '    main = "check_history_provider_pack_boundary_test.py",\n',
                '    main = "check_history_provider_pack_boundary_test.py",\n    tags = ["manual"],\n',
                "must remain in default CI",
            ),
        )
        for path, before, after, error in cases:
            with self.subTest(path=path, before=before):
                self.replace(path, before, after)
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()
                shutil.copy2(REPOSITORY / "BUILD.bazel", self.root / "BUILD.bazel")
                shutil.copy2(
                    REPOSITORY / "tools/bazel/BUILD.bazel",
                    self.root / "tools/bazel/BUILD.bazel",
                )

    def test_cargo_workspace_package_data_must_match_members(self) -> None:
        self.replace(
            self.root / "BUILD.bazel",
            '    "//crates/ctx-cli-qualification:cargo_package_data",\n',
            "",
        )
        with self.assertRaisesRegex(
            BoundaryError, "Cargo workspace package-data closure drifted"
        ):
            self.validate()

    def test_capture_index_and_cross_pack_backedges_are_rejected(self) -> None:
        cases = (
            (
                "[dev-dependencies]\n",
                '[dev-dependencies]\nindex_alias = { package = "ctx-history-index", path = "../ctx-history-index" }\n',
                "forbidden Cargo dependencies|Cargo build dependencies drifted",
            ),
            (
                "\n[dev-dependencies]\ntempfile.workspace = true\n",
                '\n[dev-dependencies]\ntempfile.workspace = true\n\n[build-dependencies]\ncapture_alias = { package = "ctx-history-capture", path = "../ctx-history-capture" }\n',
                "Cargo build dependencies drifted",
            ),
            (
                "\n[dev-dependencies]\ntempfile.workspace = true\n",
                "\n[dev-dependencies]\ntempfile.workspace = true\n\n[target.'cfg(unix)'.dependencies]\nother_pack = { package = \"ctx-history-providers-other\", path = \"../ctx-history-providers-other\" }\n",
                "forbidden Cargo dependencies",
            ),
        )
        for before, after, error in cases:
            with self.subTest(after=after):
                self.replace(self.pack_cargo, before, after)
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()
                shutil.copy2(
                    REPOSITORY / "crates/ctx-history-providers-task-docs/Cargo.toml",
                    self.pack_cargo,
                )

    def test_tempfile_must_remain_dev_only(self) -> None:
        self.replace(
            self.pack_cargo,
            "[dev-dependencies]\ntempfile.workspace = true\n",
            "",
        )
        self.replace(
            self.pack_cargo,
            "sha2.workspace = true\n",
            "sha2.workspace = true\ntempfile.workspace = true\n",
        )
        with self.assertRaisesRegex(BoundaryError, "production dependencies drifted"):
            self.validate()

    def test_upward_reverse_cargo_consumer_fails_closed(self) -> None:
        terminal = self.root / "crates/ctx-terminal/Cargo.toml"
        terminal.write_text(
            terminal.read_text(encoding="utf-8")
            + '\n[dev-dependencies]\npack_alias = { package = "ctx-history-providers-task-docs", path = "../ctx-history-providers-task-docs" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "reverse Cargo consumers"):
            self.validate()

    def test_pack_build_dependency_drift_is_rejected(self) -> None:
        self.replace(
            self.pack_build,
            '"//crates/ctx-history-core:lib",\n',
            '"//crates/ctx-history-core:lib",\n    "//crates/ctx-history-index:lib",\n',
        )
        with self.assertRaisesRegex(BoundaryError, "TASK_DOCS_DEPS drifted"):
            self.validate()

    def test_raw_reverse_bazel_consumer_is_rejected(self) -> None:
        terminal_build = self.root / "crates/ctx-terminal/BUILD.bazel"
        self.replace(
            terminal_build,
            "deps = all_crate_deps(normal = True),",
            'deps = all_crate_deps(normal = True) + ["//crates/ctx-history-providers-task-docs:lib"],',
        )
        with self.assertRaisesRegex(BoundaryError, "unexpected reverse"):
            self.validate()

    def test_evaluated_reverse_bazel_consumer_drift_is_rejected(self) -> None:
        expected = EVALUATED_REVERSE_BAZEL_CONSUMERS[PACK_LABEL]
        cases = (
            expected[1:],
            (*expected, "//crates/ctx-terminal:lib"),
        )
        for consumers in cases:
            with self.subTest(consumers=consumers):
                def query(_expression: str) -> tuple[str, ...]:
                    return consumers

                with self.assertRaisesRegex(
                    BoundaryError, "evaluated reverse Bazel consumers"
                ):
                    validate_evaluated_reverse_bazel_consumers(query)


if __name__ == "__main__":
    unittest.main()
