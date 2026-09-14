#!/usr/bin/env python3
"""Adversarial mutations for the history CLI static boundary."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_history_cli_boundary import (
    EVALUATED_REVERSE_BAZEL_CONSUMERS,
    HISTORY_LABEL,
    HISTORY_TEST_SUPPORT_LABEL,
    BoundaryError,
    validate,
    validate_evaluated_reverse_bazel_consumers,
)


REPOSITORY = (
    Path(sys.argv[1]).resolve().parent
    if len(sys.argv) == 2
    else Path(__file__).resolve().parents[2]
)
if len(sys.argv) == 2:
    sys.argv.pop()
CHECKER = REPOSITORY / "tools/bazel/check_history_cli_boundary.py"


class HistoryCliBoundaryMutations(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        shutil.copy2(REPOSITORY / "Cargo.toml", self.root / "Cargo.toml")
        shutil.copy2(REPOSITORY / "BUILD.bazel", self.root / "BUILD.bazel")
        workspace_inputs = list((REPOSITORY / "crates").glob("*/Cargo.toml"))
        workspace_inputs.extend((REPOSITORY / "crates").glob("*/BUILD.bazel"))
        for source in workspace_inputs:
            destination = self.root / source.relative_to(REPOSITORY)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        for relative in (
            "crates/ctx-cli/src/provider_args.rs",
            "crates/ctx-cli/src/provider_sources.rs",
        ):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPOSITORY / relative, destination)
        shutil.copytree(
            REPOSITORY / "crates/ctx-history-cli/src",
            self.root / "crates/ctx-history-cli/src",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def reset(self) -> None:
        self.tearDown()
        self.setUp()

    @property
    def history_cargo(self) -> Path:
        return self.root / "crates/ctx-history-cli/Cargo.toml"

    @property
    def history_build(self) -> Path:
        return self.root / "crates/ctx-history-cli/BUILD.bazel"

    @property
    def final_build(self) -> Path:
        return self.root / "crates/ctx-cli/BUILD.bazel"

    def fixed_arguments(self) -> tuple[Path, ...]:
        return (
            self.root / "Cargo.toml",
            self.history_cargo,
            self.history_build,
            self.root / "crates/ctx-cli/Cargo.toml",
            self.final_build,
            self.root / "crates/ctx-history-cli/src",
            self.root / "crates/ctx-cli/src/provider_args.rs",
            self.root / "crates/ctx-cli/src/provider_sources.rs",
        )

    def member_cargos(self) -> tuple[Path, ...]:
        return tuple(sorted((self.root / "crates").glob("*/Cargo.toml")))

    def member_builds(self) -> tuple[Path, ...]:
        return (
            self.root / "BUILD.bazel",
            *sorted((self.root / "crates").glob("*/BUILD.bazel")),
        )

    def cli_arguments(self) -> tuple[str, ...]:
        return (
            *(str(path) for path in self.fixed_arguments()),
            *(str(path) for path in self.member_cargos()),
            "--member-builds",
            *(str(path) for path in self.member_builds()),
        )

    def validate(self) -> None:
        validate(
            *self.fixed_arguments(),
            self.member_cargos(),
            self.member_builds(),
        )

    def replace(self, path: Path, before: str, after: str) -> None:
        source = path.read_text(encoding="utf-8")
        self.assertIn(before, source)
        path.write_text(source.replace(before, after, 1), encoding="utf-8")

    def test_clean_head_passes(self) -> None:
        self.validate()

    def test_semantic_cargo_dependency_resolves_in_dev_scope(self) -> None:
        for kind in ("renamed", "workspace"):
            with self.subTest(kind=kind):
                self.reset()
                dependency = 'semantic_alias = { package = "ctx-semantic-index", path = "../ctx-semantic-index" }\n'
                if kind == "workspace":
                    self.replace(self.root / "Cargo.toml", "[patch.crates-io]", dependency + "[patch.crates-io]")
                    dependency = "semantic_alias.workspace = true\n"
                self.replace(
                    self.history_cargo,
                    'ctx-semantic-index = { path = "../ctx-semantic-index" }\n',
                    dependency,
                )
                self.validate()

    def test_semantic_cargo_dependency_cannot_move_out_of_dev_scope(self) -> None:
        for kind in ("direct", "renamed", "workspace"):
            for table in (
                "dependencies",
                "build-dependencies",
                "target.'cfg(unix)'.dependencies",
                "target.'cfg(unix)'.build-dependencies",
                "target.'cfg(unix)'.dev-dependencies",
            ):
                with self.subTest(kind=kind, table=table):
                    self.reset()
                    original = 'ctx-semantic-index = { path = "../ctx-semantic-index" }\n'
                    dependency = original
                    if kind != "direct":
                        dependency = 'semantic_alias = { package = "ctx-semantic-index", path = "../ctx-semantic-index" }\n'
                    if kind == "workspace":
                        self.replace(self.root / "Cargo.toml", "[patch.crates-io]", dependency + "[patch.crates-io]")
                        dependency = "semantic_alias.workspace = true\n"
                    self.replace(self.history_cargo, original, "")
                    if table == "dependencies":
                        self.replace(self.history_cargo, "[dependencies]\n", "[dependencies]\n" + dependency)
                    else:
                        self.history_cargo.write_text(
                            self.history_cargo.read_text(encoding="utf-8") + f"\n[{table}]\n" + dependency,
                            encoding="utf-8",
                        )
                    with self.assertRaisesRegex(BoundaryError, "ctx-semantic-index must appear exactly once in dev-dependencies"):
                        self.validate()

    def test_semantic_cargo_dependency_is_required_and_unique(self) -> None:
        original = 'ctx-semantic-index = { path = "../ctx-semantic-index" }\n'
        for replacement in (
            "",
            original + 'semantic_alias = { package = "ctx-semantic-index", path = "../ctx-semantic-index" }\n',
        ):
            with self.subTest(replacement=replacement):
                self.reset()
                self.replace(self.history_cargo, original, replacement)
                with self.assertRaisesRegex(BoundaryError, "ctx-semantic-index must appear exactly once in dev-dependencies"):
                    self.validate()

    def test_unit_test_dependency_inventory_is_exact(self) -> None:
        original = 'HISTORY_CLI_UNIT_TEST_DEPS = [\n    "//crates/ctx-semantic-index:lib",\n]'
        for value, error in (
            ("[]", "inventory drifted"),
            ('["//crates/ctx-terminal:lib"]', "inventory drifted"),
            ('["//crates/ctx-semantic-index:lib", "//crates/ctx-agent-application:lib"]', "inventory drifted"),
            ('["//crates/ctx-semantic-index:lib", "//crates/ctx-semantic-index:lib"]', "duplicate labels"),
            ('["//crates/ctx-semantic-index:lib"] + ["//crates/ctx-agent-application:lib"]', "standalone literal string list"),
            ('["//crates/ctx-semantic-index:lib"] if True else ["//crates/ctx-agent-application:lib"]', "standalone literal string list"),
        ):
            with self.subTest(value=value):
                self.reset()
                self.replace(self.history_build, original, "HISTORY_CLI_UNIT_TEST_DEPS = " + value)
                with self.assertRaisesRegex(BoundaryError, "HISTORY_CLI_UNIT_TEST_DEPS.*" + error):
                    self.validate()

    def test_unit_test_dependency_inventory_cannot_be_loaded_rebound_or_copied(self) -> None:
        original = 'HISTORY_CLI_UNIT_TEST_DEPS = [\n    "//crates/ctx-semantic-index:lib",\n]'
        cases = (
            (
                '"aliases", "all_crate_deps", "crate_edition")',
                '"aliases", "all_crate_deps", "crate_edition", "HISTORY_CLI_UNIT_TEST_DEPS")',
                "must be a local literal",
            ),
            (original, original + '\nHISTORY_CLI_UNIT_TEST_DEPS = []', "inventory drifted"),
            (original, 'HISTORY_CLI_UNIT_TEST_DEPS = OTHER_TEST_DEPS\nOTHER_TEST_DEPS = ["//crates/ctx-semantic-index:lib"]', "must be a literal string list"),
            (original, original + '\nCOPIED_DEPS = HISTORY_CLI_UNIT_TEST_DEPS', "may only be assigned"),
            (original, original + '\nHISTORY_CLI_UNIT_TEST_DEPS.append("//crates/ctx-agent-application:lib")', "unsupported rule or macro"),
        )
        for before, after, error in cases:
            with self.subTest(after=after):
                self.reset()
                self.replace(self.history_build, before, after)
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()

    def test_unit_test_dependency_cannot_feed_other_targets(self) -> None:
        unit_deps = "deps = all_crate_deps(normal = True, normal_dev = True) + HISTORY_CLI_DEPS"
        for before, after, error, move in (
            (
                "deps = all_crate_deps(normal = True) + HISTORY_CLI_DEPS,",
                "deps = all_crate_deps(normal = True) + HISTORY_CLI_DEPS + HISTORY_CLI_UNIT_TEST_DEPS,",
                "rust_library deps must be exactly",
                True,
            ),
            (
                "deps = all_crate_deps(normal = True) + HISTORY_CLI_TEST_SUPPORT_DEPS,",
                "deps = all_crate_deps(normal = True) + HISTORY_CLI_TEST_SUPPORT_DEPS + HISTORY_CLI_UNIT_TEST_DEPS,",
                "rust_library deps must be exactly",
                True,
            ),
            (
                unit_deps + " + [",
                unit_deps + " + HISTORY_CLI_UNIT_TEST_DEPS + [",
                "may only be assigned and used by its reviewed targets",
                False,
            ),
        ):
            with self.subTest(after=after):
                self.reset()
                if move:
                    # Preserve the use count so the library's exact expression
                    # must reject the otherwise valid inventory.
                    self.replace(self.history_build, unit_deps + " + HISTORY_CLI_UNIT_TEST_DEPS,", unit_deps + ",")
                self.replace(self.history_build, before, after)
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()

    def test_unit_test_dependency_cannot_enter_proc_macros_or_arbitrary_suffixes(self) -> None:
        for before, after, error in (
            (
                "proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),",
                'proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True) + ["//crates/ctx-semantic-index:lib"],',
                "proc_macro_deps must be exactly",
            ),
            (
                "+ HISTORY_CLI_DEPS + HISTORY_CLI_UNIT_TEST_DEPS,",
                '+ HISTORY_CLI_DEPS + HISTORY_CLI_UNIT_TEST_DEPS + ["//crates/ctx-agent-application:lib"],',
                "ctx_rust_test deps must be exactly",
            ),
            (
                "+ HISTORY_CLI_DEPS + HISTORY_CLI_UNIT_TEST_DEPS,",
                "+ HISTORY_CLI_UNIT_TEST_DEPS + HISTORY_CLI_DEPS,",
                "ctx_rust_test deps must be exactly",
            ),
            (
                "+ HISTORY_CLI_DEPS + [\n",
                '+ HISTORY_CLI_DEPS + ["//crates/ctx-semantic-index:lib"] + [\n',
                "request_parity_tests deps must be exactly",
            ),
        ):
            with self.subTest(after=after):
                self.reset()
                self.replace(self.history_build, before, after)
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()

    def test_renamed_workspace_and_target_cargo_dependencies_fail_closed(self) -> None:
        cases = (
            (
                "renamed",
                "forbidden Cargo dependencies",
            ),
            (
                "workspace",
                "forbidden Cargo dependencies",
            ),
            (
                "target",
                "forbidden Cargo dependencies",
            ),
        )
        for kind, error in cases:
            with self.subTest(kind=kind, error=error):
                if kind == "renamed":
                    self.replace(self.history_cargo, "[dev-dependencies]\n", "[dev-dependencies]\ncli_alias = { package = \"ctx-cli\", path = \"../ctx-cli\" }\n")
                elif kind == "workspace":
                    self.replace(self.root / "Cargo.toml", "[patch.crates-io]", "cli_alias = { package = \"ctx-cli\", version = \"1\" }\n[patch.crates-io]")
                    self.replace(self.history_cargo, "[dev-dependencies]\n", "[dev-dependencies]\ncli_alias.workspace = true\n")
                else:
                    self.history_cargo.write_text(self.history_cargo.read_text(encoding="utf-8") + "\n[target.'cfg(unix)'.build-dependencies]\ncli_alias = { package = \"ctx-cli\", path = \"../ctx-cli\" }\n", encoding="utf-8")
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()
                self.reset()

    def test_malformed_and_ambiguous_cargo_entries_fail_closed(self) -> None:
        for kind, error in (
            ("malformed", "must be a string or inline table"),
            ("ambiguous", "ambiguous workspace"),
            ("\n[target.'cfg(unix)']\nunsupported = {}\n", "unsupported tables"),
        ):
            with self.subTest(kind=kind):
                if kind == "malformed":
                    self.replace(self.history_cargo, "[dev-dependencies]\n", "[dev-dependencies]\ncli_alias = 1\n")
                elif kind == "ambiguous":
                    self.replace(self.history_cargo, "[dev-dependencies]\n", "[dev-dependencies]\ncli_alias = { workspace = false }\n")
                else:
                    self.history_cargo.write_text(self.history_cargo.read_text(encoding="utf-8") + kind, encoding="utf-8")
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()
                self.reset()

    def test_agent_identifier_family_fails_closed(self) -> None:
        source = self.root / "crates/ctx-history-cli/src/lib.rs"
        for identifier in ("ctx_agent_application::authority", "ctx_agent_integrations::authority"):
            with self.subTest(identifier=identifier):
                source.write_text(source.read_text(encoding="utf-8") + f"\nfn forbidden() {{ {identifier}(); }}\n", encoding="utf-8")
                with self.assertRaisesRegex(BoundaryError, "prohibited identifiers"):
                    self.validate()
                self.reset()
                source = self.root / "crates/ctx-history-cli/src/lib.rs"

    def test_identity_and_unknown_provider_authority_fail_closed(self) -> None:
        source = self.root / "crates/ctx-history-cli/src/lib.rs"
        for identifier in (
            "identity::home_dir()",
            "CaptureProvider::Unknown",
        ):
            with self.subTest(identifier=identifier):
                source.write_text(
                    source.read_text(encoding="utf-8")
                    + f"\nfn forbidden() {{ let _ = {identifier}; }}\n",
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(BoundaryError, "prohibited identifiers"):
                    self.validate()
                self.reset()
                source = self.root / "crates/ctx-history-cli/src/lib.rs"

    def test_comments_are_not_false_positives(self) -> None:
        source = self.root / "crates/ctx-history-cli/src/lib.rs"
        source.write_text(
            source.read_text(encoding="utf-8")
            + """
// ctx_agent_application::comment_only()
/* outer ctx_agent_application::comment_only()
   /* nested identity::home_dir() and CaptureProvider::Unknown */
   ctx_agent_integrations::comment_only()
*/
const RAW_BOUNDARY_TEXT: &str = r####"ctx_agent_application::inside_raw()
identity::home_dir(); CaptureProvider::Unknown; "quoted""####;
""",
            encoding="utf-8",
        )
        self.validate()

    def test_real_identifier_after_raw_string_is_rejected(self) -> None:
        source = self.root / "crates/ctx-history-cli/src/lib.rs"
        source.write_text(
            source.read_text(encoding="utf-8")
            + 'const RAW: &str = r######"ctx_agent_application::text()"######;\n'
            + "fn forbidden_after_raw() { identity::home_dir(); }\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "identity::home_dir"):
            self.validate()

    def test_reverse_cargo_consumer_fails_closed(self) -> None:
        consumer = self.root / "crates/ctx-terminal/Cargo.toml"
        consumer.write_text(consumer.read_text(encoding="utf-8") + "\n[dev-dependencies]\nhistory_alias = { package = \"ctx-history-cli\", path = \"../ctx-history-cli\" }\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "reverse Cargo consumers"):
            self.validate()

    def test_bazel_raw_composed_aliased_and_proc_macro_edges_fail_closed(self) -> None:
        cases = (
            ("history", "\ncustom_rust_target(name = \"concealed\", deps = [\"//crates/ctx-agent-application:lib\"])\n", "unsupported rule or macro"),
            ("history", "\nCOPIED_DEPS = HISTORY_CLI_DEPS\n", "may only be assigned"),
            ("history", "\nHISTORY_CLI_DEPS = [\"//crates/ctx-history-capture:lib\"] + [\"//crates/ctx-history-core:lib\"]\n", "inventory drifted"),
            ("history", "\nrust_library(name = \"raw\", deps = [\"//crates/ctx-agent-application:lib\"], proc_macro_deps = [\"//crates/ctx-agent-integrations:lib\"])\n", "exactly two rust_library"),
            ("final", "\nCTX_CLI_DEPS.append(\"//crates/ctx-history-cli:lib\")\n", "unsupported rule or macro"),
        )
        for target, addition, error in cases:
            with self.subTest(target=target, error=error):
                path = self.history_build if target == "history" else self.final_build
                path.write_text(path.read_text(encoding="utf-8") + addition, encoding="utf-8")
                with self.assertRaisesRegex(BoundaryError, error):
                    self.validate()
                self.reset()

    def test_unexpected_reverse_bazel_label_fails_closed(self) -> None:
        self.replace(
            self.final_build,
            '"//crates/ctx-history-cli:lib",\n    "//crates/ctx-history-core:lib",',
            '"//crates/ctx-history-cli:lib",\n    "//crates/ctx-history-cli:unexpected",\n    "//crates/ctx-history-core:lib",',
        )
        with self.assertRaisesRegex(BoundaryError, "reverse history-cli labels"):
            self.validate()

    def test_unreviewed_workspace_build_consumer_fails_closed(self) -> None:
        for label in (HISTORY_LABEL, HISTORY_TEST_SUPPORT_LABEL):
            with self.subTest(label=label):
                terminal_build = self.root / "crates/ctx-terminal/BUILD.bazel"
                self.replace(
                    terminal_build,
                    "deps = all_crate_deps(normal = True),",
                    f'deps = all_crate_deps(normal = True) + ["{label}"],',
                )
                with self.assertRaisesRegex(BoundaryError, "unexpected reverse"):
                    self.validate()
                self.reset()

    def test_concatenated_reverse_bazel_label_fails_closed_after_evaluation(self) -> None:
        terminal_build = self.root / "crates/ctx-terminal/BUILD.bazel"
        self.replace(
            terminal_build,
            "deps = all_crate_deps(normal = True),",
            'deps = all_crate_deps(normal = True) + ["//crates/ctx-history-" + "cli:lib"],',
        )
        # The static inventory cannot resolve Starlark expressions in an
        # unreviewed consumer. The live Bazel query below must reject its edge.
        self.validate()
        actual = {
            HISTORY_LABEL: (*EVALUATED_REVERSE_BAZEL_CONSUMERS[HISTORY_LABEL], "//crates/ctx-terminal:lib"),
            HISTORY_TEST_SUPPORT_LABEL: EVALUATED_REVERSE_BAZEL_CONSUMERS[HISTORY_TEST_SUPPORT_LABEL],
        }
        with self.assertRaisesRegex(BoundaryError, "evaluated reverse Bazel consumers"):
            validate_evaluated_reverse_bazel_consumers(
                lambda expression: actual[HISTORY_LABEL if HISTORY_LABEL in expression else HISTORY_TEST_SUPPORT_LABEL]
            )

    def test_loaded_macro_reverse_bazel_dependency_fails_closed_after_evaluation(self) -> None:
        terminal_build = self.root / "crates/ctx-terminal/BUILD.bazel"
        terminal_macro = self.root / "crates/ctx-terminal/history_cli_consumer.bzl"
        terminal_macro.write_text(
            """def add_history_cli_consumer():
    native.filegroup(
        name = "unexpected_history_consumer",
        srcs = ["//crates/ctx-history-cli:lib"],
    )
""",
            encoding="utf-8",
        )
        terminal_build.write_text(
            'load(":history_cli_consumer.bzl", "add_history_cli_consumer")\n'
            + terminal_build.read_text(encoding="utf-8")
            + "\nadd_history_cli_consumer()\n",
            encoding="utf-8",
        )
        # The dependency is in a loaded file, outside the raw BUILD inventory.
        self.validate()
        actual = {
            HISTORY_LABEL: (*EVALUATED_REVERSE_BAZEL_CONSUMERS[HISTORY_LABEL], "//crates/ctx-terminal:unexpected_history_consumer"),
            HISTORY_TEST_SUPPORT_LABEL: EVALUATED_REVERSE_BAZEL_CONSUMERS[HISTORY_TEST_SUPPORT_LABEL],
        }
        with self.assertRaisesRegex(BoundaryError, "evaluated reverse Bazel consumers"):
            validate_evaluated_reverse_bazel_consumers(
                lambda expression: actual[HISTORY_LABEL if HISTORY_LABEL in expression else HISTORY_TEST_SUPPORT_LABEL]
            )

    def test_workspace_build_input_inventory_fails_closed(self) -> None:
        with self.assertRaisesRegex(BoundaryError, "BUILD input inventory"):
            validate(
                *self.fixed_arguments(),
                self.member_cargos(),
                self.member_builds()[:-1],
            )

    def test_ctx_cli_dependency_inventory_cannot_feed_integration_tests(self) -> None:
        self.replace(
            self.final_build,
            'extra_data = ["//:cloud_removed_build_inputs"],',
            "extra_data = CTX_CLI_DEPS,",
        )
        with self.assertRaisesRegex(BoundaryError, "reviewed Rust targets"):
            self.validate()

    def test_test_support_library_is_testonly_and_has_exact_uses(self) -> None:
        self.replace(
            self.history_build,
            'name = "test_support_lib",\n    testonly = True,',
            'name = "test_support_lib",',
        )
        with self.assertRaisesRegex(BoundaryError, "must set testonly"):
            self.validate()
        self.reset()
        self.replace(
            self.final_build,
            'extra_data = ["//:cloud_removed_build_inputs"],',
            "extra_data = CTX_CLI_TEST_DEPS,",
        )
        with self.assertRaisesRegex(BoundaryError, "reviewed Rust targets"):
            self.validate()

    def test_proc_macro_composition_fails_closed(self) -> None:
        self.replace(
            self.history_build,
            "proc_macro_deps = all_crate_deps(proc_macro = True),",
            'proc_macro_deps = all_crate_deps(proc_macro = True) + ["//crates/ctx-agent-integrations:lib"],',
        )
        with self.assertRaisesRegex(BoundaryError, "proc_macro_deps must be exactly"):
            self.validate()

    def test_malformed_toml_checker_cli_fails_closed(self) -> None:
        self.history_cargo.write_text("[dependencies\n", encoding="utf-8")
        result = subprocess.run([sys.executable, str(CHECKER), *self.cli_arguments()], check=False, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_checker_cli_returns_failure(self) -> None:
        self.history_cargo.write_text(self.history_cargo.read_text(encoding="utf-8") + "\n[build-dependencies]\nclap_alias = { package = \"clap\", version = \"4\" }\n", encoding="utf-8")
        result = subprocess.run([sys.executable, str(CHECKER), *self.cli_arguments()], check=False, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("forbidden Cargo dependencies", result.stderr)


if __name__ == "__main__":
    unittest.main()
