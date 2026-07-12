# Semantic Core ML Model Bundle

This directory produces and verifies the public
`intfloat/multilingual-e5-small` fp16 Core ML bundle. Packaging is offline: it
accepts already-converted `.mlpackage` directories, a pinned `tokenizer.json`,
and the upstream model license. It does not read credentials, private evals, or
machine-specific paths into the bundle.

## Reproducible production

1. On macOS 26.2 with Xcode 26.2 and CPython 3.11.9, create an isolated
   environment and install `requirements.lock`.
2. Resolve the model only at revision
   `614241f622f53c4eeff9890bdc4f31cfecc418b3`; retain its `LICENSE`,
   `tokenizer.json`, and source weights locally. Convert with the repository's
   pinned-revision converter and explicit fixed dimensions:

```bash
python scripts/convert-e5-coreml.py /artifacts/document.mlpackage \
  --batch 16 --sequence 512 --precision fp16
python scripts/convert-e5-coreml.py /artifacts/query.mlpackage \
  --batch 1 --sequence 512 --precision fp16
```

3. Run the producer with the same fixed tensor dimensions:

```bash
python scripts/semantic-model-bundle/produce.py \
  --tokenizer /public-snapshot/tokenizer.json \
  --document-model /artifacts/document.mlpackage \
  --query-model /artifacts/query.mlpackage \
  --model-license /public-snapshot/LICENSE \
  --bundle-version 1.0.0 \
  --document-batch-size 16 --query-batch-size 1 --sequence-length 512 \
  --output-dir /artifacts/ctx-e5-coreml-1.0.0 \
  --archive /artifacts/ctx-e5-coreml-1.0.0.tar.xz
```

The document and query packages have distinct fixed contracts: document inputs
are `[16, 512]` with output `[16, 384]`, while query inputs are `[1, 512]` with
output `[1, 384]`. The producer validates each package against its role. To
produce a bundle without a separate query package, omit both `--query-model`
and `--query-batch-size`; the manifest then omits `query_batch_size`, and the
runtime uses the document model for queries.

The producer rejects tool-version drift, symlinks, unknown source revisions,
and existing output paths. Tar member order, modes, owners, and mtimes are
normalized, so identical inputs produce an identical manifest and archive.

Verify without importing Core ML or loading model weights:

```bash
python scripts/semantic-model-bundle/verify.py /artifacts/ctx-e5-coreml-1.0.0
```

The manifest intentionally excludes itself from `files`; every other regular
file is listed with its complete lowercase SHA-256 and exact byte size. Empty
directories, symlinks, special files, unlisted payloads, and paths outside the
small allowlist are rejected.
