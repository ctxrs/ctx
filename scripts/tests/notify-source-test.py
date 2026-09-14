"""Check the actual patched source and Cargo inventory, without compiling ctx."""

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
import notify_source


ROOT = Path(__file__).resolve().parents[2]
ARCHIVE = Path(sys.argv.pop(1)).resolve()
spec = importlib.util.spec_from_file_location(
    "inventory", ROOT / "scripts/release/cargo-release-inventory.py"
)
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)


class NotifySourceTest(unittest.TestCase):
    def test_exact_patch_preserves_graph_and_declares_local_correction(self):
        with tempfile.TemporaryDirectory() as directory:
            source = notify_source.prepare(ARCHIVE, Path(directory) / "patched")
            self.assertEqual(notify_source.digest(source / "Cargo.toml"), notify_source.MANIFEST_SHA256)
            self.assertEqual(notify_source.digest(source / "src/fsevent.rs"), notify_source.FSEVENT_SHA256)
            # No diagnostic hook, dependency change, or idle polling is shipped.
            code = (source / "src/fsevent.rs").read_text()
            self.assertNotIn("lost_stop_probe", code)
            self.assertIn("while !thread_handle.is_finished()", code)
            config = (source.parent / "config.toml").read_text()
            self.assertEqual(json.loads(config.removeprefix("paths = ")), [str(source)])
            package = {"name": "notify", "version": notify_source.VERSION,
                       "manifest_path": str(source / "Cargo.toml"), "source": None}
            metadata = {"packages": [package]}
            notify_source.bind_metadata(metadata, source)
            self.assertEqual(package["source"], notify_source.REGISTRY)
            self.assertEqual(metadata["source_patches"][0]["patch_sha256"], notify_source.digest(notify_source.PATCH))
            self.assertEqual(inventory.package_label(package, ROOT),
                "@@rules_rust++crate+crates__notify-9.0.0-rc.4//:notify")
            materials = inventory.safe_materials(package, ROOT)
            self.assertTrue(all(item["kind"] == "external" for item in materials))
            notice = next(item for item in materials if item["logical"].endswith("NOTICE.ctx-patch"))
            self.assertIn(notify_source.FSEVENT_SHA256, Path(notice["path"]).read_text())
            notice_path = source / "NOTICE.ctx-patch"
            notice_text = notice_path.read_text()
            notice_path.unlink()
            with self.assertRaisesRegex(ValueError, "notice missing"):
                notify_source.verify(source)
            notice_path.write_text(notice_text)
            with self.assertRaisesRegex(ValueError, "did not select"):
                notify_source.bind_metadata({"packages": []}, source)
            (source / "src/fsevent.rs").write_text(code.replace("while !thread_handle.is_finished()", "if !thread_handle.is_finished()"))
            with self.assertRaisesRegex(ValueError, "source mismatch"):
                notify_source.verify(source)

    def test_rejects_wrong_archive_without_creating_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "wrong.crate"
            archive.write_bytes(b"not the pinned crate")
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                notify_source.prepare(archive, root / "patched")
            self.assertFalse((root / "patched").exists())

    def test_bazel_and_release_constructor_bind_same_patch_and_version(self):
        module = (ROOT / "MODULE.bazel").read_text()
        factory = (ROOT / "scripts/release/build-public-candidate-on-linux.sh").read_text()
        lock = (ROOT / "Cargo.lock").read_text()
        self.assertIn(f'version = "{notify_source.VERSION}"', module)
        self.assertIn(f'patches = ["//tools/bazel/patches:{notify_source.PATCH.name}"]', module)
        self.assertIn(notify_source.ARCHIVE_SHA256, lock)
        self.assertIn(notify_source.ARCHIVE_SHA256, factory)
        self.assertIn('notify_args=(--config "${stage_dir}/notify/config.toml")', factory)
        self.assertIn('"${notify_args[@]}"', factory)
        self.assertIn('notify_inventory_args=(--notify-source "${notify_source}")', factory)
        self.assertIn('"${notify_inventory_args[@]}"', factory)
        self.assertIn("--remap-path-prefix=${notify_source}=", factory)


if __name__ == "__main__":
    unittest.main()
