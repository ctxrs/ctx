#!/usr/bin/env python3
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_provider_mistral_mux_boundary import BoundaryError, validate


MANIFEST = """\
[package]
name = "ctx-history-provider-mistral-mux"

[dependencies]
chrono.workspace = true
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-capture-runtime = { path = "../ctx-history-capture-runtime" }
ctx-history-core = { path = "../ctx-history-core" }
ctx-history-jsonl = { path = "../ctx-history-jsonl" }
ctx-history-provider-runtime = { path = "../ctx-history-provider-runtime" }
ctx-history-source-io = { path = "../ctx-history-source-io" }
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true

[dev-dependencies]
ctx-history-jsonl = { path = "../ctx-history-jsonl", features = ["test-support"] }
tempfile.workspace = true
uuid.workspace = true
"""

BUILD = "\n".join(
    f'"//crates/{label}",'
    for label in (
        "ctx-history-capture-model:lib",
        "ctx-history-capture-runtime:lib",
        "ctx-history-core:lib",
        "ctx-history-jsonl:lib",
        "ctx-history-jsonl:test_support_lib",
        "ctx-history-provider-runtime:lib",
        "ctx-history-provider-runtime:test_support_lib",
        "ctx-history-source-io:lib",
        "ctx-history-source-io:test_support_lib",
    )
)
CAPTURE_MANIFEST = "[dependencies]\n"
CAPTURE_BUILD = ""
PROVIDER_MODULES = "mod codex;\n"
SOURCE_BACKED = """\
use ctx_history_provider_mistral_mux::{
    mistral_vibe_jsonl_adapter_with_source_root_lineage, mux_jsonl_adapter_with_source_root_lineage,
};
"""
REGISTRATION = """\
mistral_vibe_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(
            source_root_lineage,
        );
mux_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(source_root_lineage);
"""
SOURCE_FIXTURES = {
    "lib.rs": """\
mod mistral_vibe;
mod mux;
pub fn mistral_vibe_jsonl_adapter<B>(value: B) -> ProviderJsonlRuntime<B> { todo!() }
pub fn mistral_vibe_jsonl_adapter_with_source_root_lineage<B>(value: B) -> ProviderJsonlRuntime<B> { todo!() }
pub fn mux_jsonl_adapter<B>() -> ProviderJsonlRuntime<B> { todo!() }
pub fn mux_jsonl_adapter_with_source_root_lineage<B>(value: B) -> ProviderJsonlRuntime<B> { todo!() }
""",
    "mistral_vibe.rs": "CaptureProvider::MistralVibe\n",
    "mistral_vibe/native_path.rs": "mistral-vibe-content-occurrence-v1\n",
    "mistral_vibe/native_path/source_backed.rs": "ProviderBaseEventLookup<B>\n",
    "mistral_vibe/native_path/source_backed/activity.rs": "",
    "mistral_vibe/native_path/source_backed/tests.rs": "",
    "mistral_vibe/schema.rs": "",
    "mistral_vibe/source.rs": "",
    "mux.rs": "CaptureProvider::Mux\n",
    "mux/metadata.rs": "",
    "mux/native_path.rs": "",
    "mux/native_path/source_backed.rs": "ProviderBaseEventLookup<B>\n",
    "mux/native_path/source_backed/projection.rs": "mux-content-occurrence-v1\n",
    "mux/native_path/source_backed/projection/seam.rs": "",
    "mux/native_path/source_backed/tests.rs": "",
    "mux/normalization.rs": "",
    "mux/source.rs": "",
}


class MistralMuxBoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.manifest = root / "pack/Cargo.toml"
        self.build = root / "pack/BUILD.bazel"
        self.capture_manifest = root / "capture/Cargo.toml"
        self.capture_build = root / "capture/BUILD.bazel"
        self.provider_modules = root / "capture/providers.rs"
        self.source_backed = root / "capture/source_backed.rs"
        self.registration = root / "capture/other.rs"
        for path, contents in (
            (self.manifest, MANIFEST),
            (self.build, BUILD),
            (self.capture_manifest, CAPTURE_MANIFEST),
            (self.capture_build, CAPTURE_BUILD),
            (self.provider_modules, PROVIDER_MODULES),
            (self.source_backed, SOURCE_BACKED),
            (self.registration, REGISTRATION),
        ):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents, encoding="utf-8")
        for relative, contents in SOURCE_FIXTURES.items():
            path = self.manifest.parent / "src" / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents, encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate(
            self.manifest,
            self.build,
            self.capture_manifest,
            self.capture_build,
            self.provider_modules,
            self.source_backed,
            self.registration,
        )

    def test_narrow_pack_passes(self) -> None:
        self.validate()

    def test_renamed_capture_dependency_is_rejected(self) -> None:
        self.manifest.write_text(
            MANIFEST
            + '\ncapture_alias = { package = "ctx-history-capture", path = "../ctx-history-capture" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "forbidden Cargo dependency"):
            self.validate()

    def test_shared_jsonl_compat_copy_is_rejected(self) -> None:
        source = self.manifest.parent / "src/mux/source.rs"
        source.write_text("type CaptureJsonlRuntime = ();\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "shared/capture authority"):
            self.validate()

    def test_third_provider_source_is_rejected(self) -> None:
        source = self.manifest.parent / "src/native_jsonl.rs"
        source.write_text("struct NativeJsonl;\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "source ownership drifted"):
            self.validate()

    def test_mistral_source_backed_tests_are_required(self) -> None:
        source = (
            self.manifest.parent
            / "src/mistral_vibe/native_path/source_backed/tests.rs"
        )
        source.unlink()
        with self.assertRaisesRegex(BoundaryError, "source ownership drifted"):
            self.validate()

    def test_capture_cannot_reclaim_mux_module(self) -> None:
        self.provider_modules.write_text("mod mux;\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture still owns"):
            self.validate()

    def test_capture_registration_must_bind_runtime_and_root_lineage(self) -> None:
        self.registration.write_text(
            REGISTRATION.replace(
                "mux_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(source_root_lineage)",
                "mux_jsonl_adapter::<CaptureProviderRuntime>()",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "thin registration drifted"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
