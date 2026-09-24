#!/usr/bin/env python3
"""Regressions for pruning the SDK's offline Cargo manifests."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "prepare-sdk-cargo-workspace.py"
SPEC = importlib.util.spec_from_file_location("prepare_sdk_cargo_workspace", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
generator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(generator)


class OptionalDependencyPruningTest(unittest.TestCase):
    def prune(self, dependency_section: str, available: set[str]) -> dict:
        # indexmap keeps an optional serde compatibility dependency under
        # cfg(any()) while also using serde for its own tests.
        source = f'''[package]
name = "fixture"
version = "0.1.0"

[features]
default = ["std"]
serde = [
    "dep:serde_core",
    "dep:serde",
]
std = []
test_only = [
    "dep:quickcheck",
]

[dependencies.serde_core]
version = "1"
optional = true

[{dependency_section}]
version = "1"
optional = true

[dependencies.quickcheck]
version = "1"
optional = true

[dev-dependencies.serde]
version = "1"
features = ["derive"]

[dev-dependencies.quickcheck]
version = "1"
'''
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text(source, encoding="utf-8")
            generator.remove_unavailable_optional_dependencies(manifest, available)
            return generator.load_toml(manifest)

    def test_dev_dependency_removal_preserves_retained_dependency_features(self) -> None:
        for section in ("dependencies.serde", 'target."cfg(any())".dependencies.serde'):
            with self.subTest(section=section):
                result = self.prune(section, {"serde", "serde_core"})
                self.assertEqual(result["features"]["serde"], ["dep:serde_core", "dep:serde"])
                dependencies = result
                if section.startswith("target."):
                    dependencies = result["target"]["cfg(any())"]
                self.assertTrue(dependencies["dependencies"]["serde"]["optional"])
                self.assertNotIn("dev-dependencies", result)
                self.assertNotIn("quickcheck", result["dependencies"])
                self.assertEqual(result["features"]["test_only"], [])
                self.assertEqual(result["features"]["default"], ["std"])

    def test_unavailable_dependency_still_loses_its_feature_reference(self) -> None:
        for section in ("dependencies.serde", 'target."cfg(any())".dependencies.serde'):
            with self.subTest(section=section):
                result = self.prune(section, {"serde_core"})
                self.assertEqual(result["features"]["serde"], ["dep:serde_core"])
                self.assertNotIn("serde", result["dependencies"])
                self.assertNotIn("target", result)
                self.assertNotIn("dev-dependencies", result)
                self.assertNotIn("quickcheck", result["dependencies"])
                self.assertEqual(result["features"]["test_only"], [])
                self.assertEqual(result["features"]["default"], ["std"])


if __name__ == "__main__":
    unittest.main()
