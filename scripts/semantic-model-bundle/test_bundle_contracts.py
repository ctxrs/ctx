#!/usr/bin/env python3
"""Contract and tamper tests for deterministic Core ML model bundles."""

from __future__ import annotations

import argparse
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

import bundle_format
import produce


class BundleContractTest(unittest.TestCase):
    def test_query_bundle_uses_distinct_role_batches(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=True)

            self.assertEqual(bundle_format.verify_bundle(root), manifest)
            self.assertEqual(
                manifest["tensor_contract"]["document_batch_size"], 16
            )
            self.assertEqual(manifest["tensor_contract"]["query_batch_size"], 1)

    def test_no_query_bundle_omits_query_batch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=False)

            self.assertEqual(bundle_format.verify_bundle(root), manifest)
            self.assertNotIn("query_batch_size", manifest["tensor_contract"])

    def test_rejects_swapped_role_batches_and_wrong_tensor_shape(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=True)
            contract = manifest["tensor_contract"]
            contract["document_batch_size"] = 1
            contract["query_batch_size"] = 16
            for value in contract["inputs"]:
                value["shape"][0] = 1
            contract["output"]["shape"][0] = 1
            _rewrite_manifest(root, manifest)

            with self.assertRaisesRegex(bundle_format.BundleError, "fixed batch 16"):
                bundle_format.verify_bundle(root)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=True)
            manifest["tensor_contract"]["inputs"][0]["shape"] = [16, 384]
            _rewrite_manifest(root, manifest)

            with self.assertRaisesRegex(bundle_format.BundleError, "incompatible tensor"):
                bundle_format.verify_bundle(root)

    def test_rejects_query_declaration_mismatches_and_legacy_batch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=True)
            del manifest["tensor_contract"]["query_batch_size"]
            _rewrite_manifest(root, manifest)

            with self.assertRaisesRegex(bundle_format.BundleError, "missing or unknown"):
                bundle_format.verify_bundle(root)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=False)
            manifest["tensor_contract"]["query_batch_size"] = 1
            _rewrite_manifest(root, manifest)

            with self.assertRaisesRegex(bundle_format.BundleError, "missing or unknown"):
                bundle_format.verify_bundle(root)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = _write_fixture(root, has_query=False)
            contract = manifest["tensor_contract"]
            contract["batch_size"] = contract.pop("document_batch_size")
            _rewrite_manifest(root, manifest)

            with self.assertRaisesRegex(bundle_format.BundleError, "missing or unknown"):
                bundle_format.verify_bundle(root)

    def test_payload_tamper_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _write_fixture(root, has_query=True)
            (root / "query.mlpackage" / "Data" / "model.bin").write_bytes(b"tampered")

            with self.assertRaisesRegex(bundle_format.BundleError, "does not match"):
                bundle_format.verify_bundle(root)


class ProducerContractTest(unittest.TestCase):
    def test_role_batch_arguments_are_conditional_and_exact(self) -> None:
        produce._validate_role_batches(_role_args(query_model=None, query_batch_size=None))
        produce._validate_role_batches(
            _role_args(query_model=Path("query.mlpackage"), query_batch_size=1)
        )

        for args in (
            _role_args(document_batch_size=1),
            _role_args(query_model=Path("query.mlpackage"), query_batch_size=None),
            _role_args(query_model=None, query_batch_size=1),
            _role_args(query_model=Path("query.mlpackage"), query_batch_size=16),
        ):
            with self.assertRaises(SystemExit):
                produce._validate_role_batches(args)

    def test_coreml_packages_are_checked_against_their_own_role_shape(self) -> None:
        document_spec = _coreml_spec(16)
        query_spec = _coreml_spec(1)
        modules = _fake_coreml_modules(
            {"document.mlpackage": document_spec, "query.mlpackage": query_spec}
        )
        with mock.patch.dict(sys.modules, modules):
            produce._validate_coreml_contract(
                Path("document.mlpackage"), 16, 512, "document model"
            )
            produce._validate_coreml_contract(
                Path("query.mlpackage"), 1, 512, "query model"
            )
            with self.assertRaisesRegex(bundle_format.BundleError, "incompatible contract"):
                produce._validate_coreml_contract(
                    Path("query.mlpackage"), 16, 512, "document model"
                )


def _write_fixture(root: Path, has_query: bool) -> dict:
    payloads = {
        "LICENSES/MODEL_LICENSE.txt": b"license\n",
        "PROVENANCE.json": b"{}\n",
        "THIRD_PARTY_NOTICES.md": b"notices\n",
        "document.mlpackage/Data/model.bin": b"document model",
        "tokenizer.json": b"{}\n",
    }
    if has_query:
        payloads["query.mlpackage/Data/model.bin"] = b"query model"
    for relative, body in payloads.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)

    artifacts = {
        "document_model": "document.mlpackage",
        "tokenizer": "tokenizer.json",
    }
    contract = {
        "document_batch_size": 16,
        "document_prefix": "passage: ",
        "embedding_dimensions": 384,
        "inputs": [
            {"dtype": "int32", "name": name, "shape": [16, 512]}
            for name in ("input_ids", "attention_mask", "token_type_ids")
        ],
        "max_sequence_length": 512,
        "normalization": "l2",
        "output": {
            "dtype": "float32",
            "name": "sentence_embeddings",
            "shape": [16, 384],
        },
        "pooling": "attention_mask_mean",
        "query_prefix": "query: ",
    }
    if has_query:
        artifacts["query_model"] = "query.mlpackage"
        contract["query_batch_size"] = 1
    manifest = {
        "artifacts": artifacts,
        "bundle_id": bundle_format.BUNDLE_ID,
        "bundle_version": "1.0.0",
        "files": bundle_format.file_records(root),
        "model": {
            "embedding_space_id": bundle_format.EMBEDDING_SPACE_ID,
            "id": bundle_format.MODEL_ID,
            "precision": "fp16",
            "source_revision": bundle_format.MODEL_REVISION,
        },
        "schema_version": bundle_format.SCHEMA_VERSION,
        "tensor_contract": contract,
    }
    _rewrite_manifest(root, manifest)
    return manifest


def _rewrite_manifest(root: Path, manifest: dict) -> None:
    (root / bundle_format.MANIFEST_NAME).write_bytes(bundle_format.canonical_json(manifest))


def _role_args(
    *,
    document_batch_size: int = 16,
    query_model: Path | None = None,
    query_batch_size: int | None = None,
) -> argparse.Namespace:
    return argparse.Namespace(
        document_batch_size=document_batch_size,
        query_model=query_model,
        query_batch_size=query_batch_size,
        sequence_length=512,
    )


def _coreml_spec(batch: int) -> types.SimpleNamespace:
    def feature(name: str, data_type: int, shape: list[int]) -> types.SimpleNamespace:
        array = types.SimpleNamespace(dataType=data_type, shape=shape)
        feature_type = types.SimpleNamespace(
            WhichOneof=lambda _: "multiArrayType", multiArrayType=array
        )
        return types.SimpleNamespace(name=name, type=feature_type)

    inputs = [feature(name, 1, [batch, 512]) for name in (
        "input_ids",
        "attention_mask",
        "token_type_ids",
    )]
    outputs = [feature("sentence_embeddings", 2, [batch, 384])]
    return types.SimpleNamespace(
        description=types.SimpleNamespace(input=inputs, output=outputs)
    )


def _fake_coreml_modules(specs: dict[str, types.SimpleNamespace]) -> dict[str, object]:
    coremltools = types.ModuleType("coremltools")
    coremltools.models = types.SimpleNamespace(
        MLModel=lambda path, skip_model_load: types.SimpleNamespace(
            get_spec=lambda: specs[path]
        )
    )
    proto = types.ModuleType("coremltools.proto")
    feature_types = types.ModuleType("coremltools.proto.FeatureTypes_pb2")
    feature_types.ArrayFeatureType = types.SimpleNamespace(INT32=1, FLOAT32=2)
    return {
        "coremltools": coremltools,
        "coremltools.proto": proto,
        "coremltools.proto.FeatureTypes_pb2": feature_types,
    }


if __name__ == "__main__":
    unittest.main()
