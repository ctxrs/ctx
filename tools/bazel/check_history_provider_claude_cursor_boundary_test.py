#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

from check_history_provider_claude_cursor_boundary import (
    FORBIDDEN,
    REQUIRED,
    BoundaryError,
    validate_capture,
    validate_pack,
)


class ClaudeCursorBoundaryMutationTests(unittest.TestCase):
    def test_capture_dependency_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = root / "Cargo.toml"
            manifest.write_text('[package]\nname = "ctx-history-provider-claude-cursor"\n[dependencies]\nctx-history-capture = "1"\n', encoding="utf-8")
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("", encoding="utf-8")
            build = root / "BUILD.bazel"
            build.write_text("", encoding="utf-8")
            with self.assertRaisesRegex(BoundaryError, "dependency inventory|forbidden"):
                validate_pack(manifest, build)

    def test_bazel_only_forbidden_dependencies_are_rejected(self) -> None:
        for dependency in sorted(FORBIDDEN):
            with self.subTest(dependency=dependency), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                manifest = root / "Cargo.toml"
                manifest.write_text(
                    '[package]\nname = "ctx-history-provider-claude-cursor"\n'
                    "[dependencies]\n"
                    + "".join(f'{item} = "1"\n' for item in sorted(REQUIRED)),
                    encoding="utf-8",
                )
                (root / "src").mkdir()
                (root / "src/lib.rs").write_text(
                    "pub fn claude_jsonl_adapter<B> "
                    "pub fn claude_jsonl_adapter_for_named_home<B> "
                    "pub fn cursor_jsonl_adapter<B> "
                    "pub fn cursor_jsonl_adapter_with_source_root_lineage<B> "
                    "ProviderJsonlRuntime<B> CaptureProvider::Claude CaptureProvider::Cursor",
                    encoding="utf-8",
                )
                build = root / "BUILD.bazel"
                build.write_text(
                    f'"//crates/{dependency}:lib"\n',
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(
                    BoundaryError,
                    f"Claude/Cursor Bazel graph gained {dependency} authority",
                ):
                    validate_pack(manifest, build)


class ClaudeCursorCaptureBindingMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.cargo = root / "Cargo.toml"
        self.build = root / "BUILD.bazel"
        self.modules = root / "modules.rs"
        self.direct = root / "direct.rs"
        self.other = root / "other.rs"
        self.sources = root / "sources.rs"
        self.cargo.write_text(
            "[dependencies]\n"
            "ctx-history-provider-claude-cursor = "
            '{ path = "../ctx-history-provider-claude-cursor" }\n',
            encoding="utf-8",
        )
        self.build.write_text(
            '"//crates/ctx-history-provider-claude-cursor:lib"\n',
            encoding="utf-8",
        )
        self.modules.write_text("mod sqlite;\n", encoding="utf-8")
        self.direct.write_text(
            "claude_jsonl_adapter_for_named_home::<\n"
            "    CaptureProviderRuntime,\n"
            ">(source_root_lineage);\n",
            encoding="utf-8",
        )
        self.other.write_text(
            "cursor_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(\n"
            "    source_root_lineage,\n"
            ");\n",
            encoding="utf-8",
        )
        self.sources.write_text(
            "ctx_history_provider_claude_cursor\n",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate_capture(
            self.cargo,
            self.build,
            self.modules,
            self.direct,
            self.other,
            self.sources,
        )

    def test_root_lineage_aware_concrete_binding_passes(self) -> None:
        self.validate()

    def test_generic_runtime_binding_is_rejected(self) -> None:
        self.direct.write_text(
            "claude_jsonl_adapter_for_named_home::<B>(source_root_lineage);\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "concrete runtime"):
            self.validate()

    def test_missing_runtime_binding_is_rejected(self) -> None:
        self.direct.write_text(
            "claude_jsonl_adapter_for_named_home(source_root_lineage);\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "concrete runtime"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
