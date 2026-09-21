"use strict";

const fs = require("node:fs");

function createSemanticContract({
  assertSafeArtifactName,
  assertSha,
  fail,
  layoutPath,
  metadataMatrixKeys,
  requireValue,
}) {
  const SEMANTIC_LAYOUT = JSON.parse(
    fs.readFileSync(layoutPath, "utf8"),
  );
  assertExactObjectKeys(
    SEMANTIC_LAYOUT,
    [
      "schema_version",
      "model_contract",
      "runtime_install_manifest_contract",
      "publication_pins",
      "assets",
      "targets",
    ],
    "semantic runtime layout",
  );
  const EXPECTED_SEMANTIC_PATHS = [
    ["apple-silicon", "coreml", "apple_silicon_coreml"],
    ["windows", "windows-ml", "windows_windows_ml"],
    ["linux-nvidia", "ort-cuda", "linux_nvidia_ort_cuda"],
    ["universal", "ort-cpu", "universal_ort_cpu"],
  ];
  const EXPECTED_SEMANTIC_ASSET_IDS = [
    "onnx_model",
    "onnx_model_o4_fp16",
    "apple_coreml",
    "linux_x64_cpu",
    "linux_aarch64_cpu",
    "macos_arm64_cpu",
    "macos_x64_cpu",
    "windows_ml",
    "linux_cuda12",
  ];
  const EXPECTED_APPLE_COREML_ASSET_IDS = [
    "onnx_model",
    "macos_arm64_cpu",
    "apple_coreml",
  ];
  const EXPECTED_TARGET_ASSET_IDS = [
    EXPECTED_APPLE_COREML_ASSET_IDS,
    ["onnx_model_o4_fp16", "windows_ml"],
    ["onnx_model_o4_fp16", "linux_cuda12"],
    [
      "onnx_model",
      "linux_x64_cpu",
      "linux_aarch64_cpu",
      "macos_arm64_cpu",
      "macos_x64_cpu",
      "windows_ml",
    ],
  ];
  const EXPECTED_RUNTIME_MANIFEST_FIELDS = [
    "schema_version",
    "manager",
    "metadata_trust",
    "runtime",
    "platform",
    "version",
    "sha256",
    "artifact_url",
    "installed_at",
    "files",
  ];
  const EXPECTED_MODEL_ARTIFACTS = {
    onnx_model: "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz",
    onnx_model_o4_fp16: "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz",
  };
  const EXPECTED_CUDA_PATHS = [
    "GIT_COMMIT_ID",
    "LICENSE",
    "NVIDIA-CUDA-LICENSE.txt",
    "NVIDIA-CUDNN-LICENSE.txt",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "lib/libcublas.so.12",
    "lib/libcublasLt.so.12",
    "lib/libcudart.so.12",
    "lib/libcudnn.so.9",
    "lib/libcudnn_graph.so.9",
    "lib/libcudnn_ops.so.9",
    "lib/libcufft.so.11",
    "lib/libcurand.so.10",
    "lib/libnvrtc.so.12",
    "lib/libonnxruntime.so",
    "lib/libonnxruntime_providers_cuda.so",
    "lib/libonnxruntime_providers_shared.so",
  ];
  if (
    SEMANTIC_LAYOUT.schema_version !== 1
    || JSON.stringify(
      SEMANTIC_LAYOUT.targets.map((entry) => [entry.target, entry.backend, entry.key]),
    ) !== JSON.stringify(EXPECTED_SEMANTIC_PATHS)
  ) {
    throw new Error("semantic runtime layout must contain the exact four target/backend paths");
  }
  if (
    JSON.stringify(Object.keys(SEMANTIC_LAYOUT.assets).sort())
    !== JSON.stringify([...EXPECTED_SEMANTIC_ASSET_IDS].sort())
  ) {
    throw new Error("semantic runtime layout must contain the exact nine-asset catalog");
  }
  if (
    JSON.stringify(SEMANTIC_LAYOUT.targets.map((target) => target.asset_ids))
      !== JSON.stringify(EXPECTED_TARGET_ASSET_IDS)
  ) {
    throw new Error("semantic runtime targets have the wrong normalized asset composition");
  }
  const runtimeManifestContract = SEMANTIC_LAYOUT.runtime_install_manifest_contract;
  if (
    runtimeManifestContract.schema_version !== 1
    || runtimeManifestContract.manager !== "ctx-hosted-installer"
    || runtimeManifestContract.metadata_trust !== "signed-release-metadata"
    || JSON.stringify(runtimeManifestContract.required_fields)
      !== JSON.stringify(EXPECTED_RUNTIME_MANIFEST_FIELDS)
    || runtimeManifestContract.publication
      !== "write_last_after_extraction_and_file_verification"
    || runtimeManifestContract.loader !== "rehash_every_required_file_before_load"
  ) {
    throw new Error("semantic runtime install-manifest contract does not match the public loader");
  }
  function semanticTargetAssets(target) {
    return target.asset_ids.map((assetId) => SEMANTIC_LAYOUT.assets[assetId]);
  }
  function validateModelPublicationPinLayout(model, expectedAssetId) {
    assertExactObjectKeys(
      model,
      ["asset_id", "signed_metadata_paths", "runtime_files"],
      "semantic model publication pins",
    );
    if (model.asset_id !== expectedAssetId) {
      throw new Error(`semantic model publication pins must bind ${expectedAssetId}`);
    }
    if (
      !Array.isArray(model.signed_metadata_paths)
      || JSON.stringify(model.signed_metadata_paths) !== JSON.stringify(["LICENSE", "manifest.json"])
    ) {
      throw new Error(
        "semantic model publication pins must identify LICENSE and manifest.json metadata",
      );
    }
    if (
      model.runtime_files === null
      || typeof model.runtime_files !== "object"
      || Array.isArray(model.runtime_files)
      || Object.keys(model.runtime_files).length !== 5
    ) {
      throw new Error("semantic model publication pins must contain exactly five runtime files");
    }
    for (const [filePath, pin] of Object.entries(model.runtime_files)) {
      if (
        filePath === ""
        || filePath.includes("\\")
        || filePath.startsWith("/")
        || filePath.endsWith("/")
        || filePath.split("/").some((part) => part === "" || part === "." || part === "..")
      ) {
        throw new Error(`semantic model publication pin has unsafe path ${filePath}`);
      }
      assertExactObjectKeys(pin, ["size", "sha256"], `semantic model publication pin ${filePath}`);
      if (!Number.isSafeInteger(pin.size) || pin.size <= 0) {
        throw new Error(`semantic model publication pin ${filePath} has invalid size`);
      }
      assertSha(pin.sha256, `semantic model publication pin ${filePath}`);
    }
    const modelAsset = SEMANTIC_LAYOUT.assets[model.asset_id];
    const modelPackagePaths = [
      ...model.signed_metadata_paths,
      ...Object.keys(model.runtime_files),
    ].sort();
    const expectedArtifact = EXPECTED_MODEL_ARTIFACTS[expectedAssetId];
    if (
      modelAsset?.role !== "model"
      || modelAsset.backend !== "onnx"
      || modelAsset.artifact !== expectedArtifact
      || modelAsset.path_prefix !== expectedArtifact.replace(/\.tar\.xz$/, "")
      || modelAsset.tree_prefixes.length !== 0
      || modelPackagePaths.length !== 7
      || JSON.stringify([...modelAsset.exact_paths].sort()) !== JSON.stringify(modelPackagePaths)
    ) {
      throw new Error(
        "semantic model package must contain exactly two signed metadata "
        + "and five pinned runtime files",
      );
    }
  }
  function validateSemanticPublicationPinLayout() {
    const pins = SEMANTIC_LAYOUT.publication_pins;
    assertExactObjectKeys(
      pins,
      ["schema_version", "model", "accelerator_model", "coreml", "windows_ml"],
      "semantic publication pins",
    );
    if (pins.schema_version !== 1) {
      throw new Error("semantic publication pins have unsupported schema");
    }

    validateModelPublicationPinLayout(pins.model, "onnx_model");
    validateModelPublicationPinLayout(pins.accelerator_model, "onnx_model_o4_fp16");

    const coreml = pins.coreml;
    assertExactObjectKeys(
      coreml,
      ["asset_id", "archive_sha256", "manifest_path", "manifest_sha256"],
      "CoreML publication pins",
    );
    if (coreml.asset_id !== "apple_coreml") {
      throw new Error("CoreML publication pins must bind apple_coreml");
    }
    assertSha(coreml.archive_sha256, "CoreML archive publication pin");
    assertSha(coreml.manifest_sha256, "CoreML manifest publication pin");
    const coremlAsset = SEMANTIC_LAYOUT.assets[coreml.asset_id];
    if (
      coreml.manifest_path !== "manifest.json"
      || coremlAsset?.role !== "accelerator"
      || coremlAsset.backend !== "coreml"
      || !coremlAsset.exact_paths.includes(coreml.manifest_path)
    ) {
      throw new Error("CoreML publication pins do not match the canonical CoreML asset");
    }

    const windowsMl = pins.windows_ml;
    assertExactObjectKeys(
      windowsMl,
      ["asset_id", "source_package", "source_package_sha256", "ort_version", "ort_commit"],
      "Windows ML publication pins",
    );
    assertSha(windowsMl.source_package_sha256, "Windows ML source-package publication pin");
    const windowsMlAsset = SEMANTIC_LAYOUT.assets[windowsMl.asset_id];
    if (
      windowsMl.asset_id !== "windows_ml"
      || windowsMl.source_package !== "Microsoft.Windows.AI.MachineLearning"
      || windowsMl.source_package_sha256
        !== "691165fa3c07a04b752cbf4a07e93ed13a418e9dea1ee89eb163d2225e2ba3af"
      || windowsMl.ort_version !== "1.24.6"
      || windowsMl.ort_commit !== "800ac32bc82d562c611d641b1112a8aa9f90c4f9"
      || windowsMlAsset?.role !== "cpu-runtime"
      || windowsMlAsset.backend !== "windows-ml"
      || windowsMlAsset.version !== "2.1.74"
      || windowsMlAsset.platform !== "windows-x64"
    ) {
      throw new Error("Windows ML publication pins do not match the canonical stable package");
    }
    return pins;
  }
  const SEMANTIC_PUBLICATION_PINS = validateSemanticPublicationPinLayout();
  const cudaLayout = SEMANTIC_LAYOUT.assets.linux_cuda12;
  if (
    cudaLayout?.artifact !== "ctx-onnxruntime-linux-x64-cuda12.tar.zst"
    || cudaLayout.platform !== "linux-x64-cuda12"
    || cudaLayout.format !== "tar.zst"
    || cudaLayout.path_prefix !== ""
    || cudaLayout.max_expanded_bytes !== 2147483648
    || cudaLayout.max_files !== EXPECTED_CUDA_PATHS.length
    || JSON.stringify(cudaLayout.exact_paths) !== JSON.stringify(EXPECTED_CUDA_PATHS)
    || cudaLayout.tree_prefixes.length !== 0
  ) {
    throw new Error(
      "CUDA runtime layout must contain the exact self-contained ORT/CUDA12/cuDNN inventory",
    );
  }
  const EXPECTED_CPU_PLATFORMS = [
    "linux-x64",
    "linux-aarch64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
  ];
  const EXPECTED_CPU_BACKENDS = [
    "ort-cpu",
    "ort-cpu",
    "ort-cpu",
    "ort-cpu",
    "windows-ml",
  ];
  const coremlAssets = semanticTargetAssets(SEMANTIC_LAYOUT.targets[0]);
  if (
    JSON.stringify(coremlAssets.map((asset) => asset.role))
      !== JSON.stringify(["model", "cpu-runtime", "accelerator"])
    || coremlAssets[0].backend !== "onnx"
    || coremlAssets[0].platform !== "any"
    || coremlAssets[1].backend !== "ort-cpu"
    || coremlAssets[1].platform !== "macos-arm64"
    || coremlAssets[2].backend !== "coreml"
    || coremlAssets[2].platform !== "macos-arm64"
  ) {
    throw new Error(
      "Core ML authority must bind the FP32 ONNX model, macOS arm64 CPU runtime, "
      + "and CoreML model bundle",
    );
  }
  const windowsMlAssets = semanticTargetAssets(SEMANTIC_LAYOUT.targets[1]);
  if (
    JSON.stringify(windowsMlAssets.map((asset) => asset.role))
      !== JSON.stringify(["model", "cpu-runtime"])
    || windowsMlAssets[0].backend !== "onnx"
    || windowsMlAssets[0].platform !== "any"
    || windowsMlAssets[1].backend !== "windows-ml"
    || windowsMlAssets[1].platform !== "windows-x64"
  ) {
    throw new Error(
      "Windows ML authority must bind the O4 FP16 model and one self-contained Windows ML runtime",
    );
  }
  const cudaAssets = semanticTargetAssets(SEMANTIC_LAYOUT.targets[2]);
  if (
    JSON.stringify(cudaAssets.map((asset) => asset.role))
      !== JSON.stringify(["model", "accelerator"])
    || cudaAssets[0].backend !== "onnx"
    || cudaAssets[0].platform !== "any"
    || cudaAssets[1].backend !== "ort-cuda"
    || cudaAssets[1].platform !== "linux-x64-cuda12"
  ) {
    throw new Error("CUDA authority must bind the O4 FP16 model and self-contained CUDA runtime");
  }
  const universalAssets = semanticTargetAssets(SEMANTIC_LAYOUT.targets[3]);
  if (
    universalAssets[0]?.role !== "model"
    || JSON.stringify(universalAssets.slice(1).map((asset) => asset.role))
      !== JSON.stringify(EXPECTED_CPU_PLATFORMS.map(() => "cpu-runtime"))
    || JSON.stringify(universalAssets.slice(1).map((asset) => asset.platform))
      !== JSON.stringify(EXPECTED_CPU_PLATFORMS)
    || JSON.stringify(universalAssets.slice(1).map((asset) => asset.backend))
      !== JSON.stringify(EXPECTED_CPU_BACKENDS)
  ) {
    throw new Error("universal authority must include CPU assets for every public platform");
  }
  const referencedSemanticAssets = new Set(
    SEMANTIC_LAYOUT.targets.flatMap((target) => target.asset_ids),
  );
  if (
    JSON.stringify([...referencedSemanticAssets].sort())
    !== JSON.stringify(Object.keys(SEMANTIC_LAYOUT.assets).sort())
  ) {
    throw new Error("semantic asset catalog and authority references must exactly match");
  }

  const SEMANTIC_AUTHORITY_KEYS = new Set(
    SEMANTIC_LAYOUT.targets.map((entry) => entry.key),
  );
  const SEMANTIC_LAYOUT_BY_KEY = new Map(
    SEMANTIC_LAYOUT.targets.map((entry) => [entry.key, entry]),
  );
  const SEMANTIC_ASSET_LAYOUTS = new Map();
  for (const asset of Object.values(SEMANTIC_LAYOUT.assets)) {
    if (SEMANTIC_ASSET_LAYOUTS.has(asset.artifact)) {
      throw new Error(`semantic artifact catalog repeats ${asset.artifact}`);
    }
    SEMANTIC_ASSET_LAYOUTS.set(asset.artifact, { layout: asset });
  }

  function canonicalJson(value) {
    if (Array.isArray(value)) {
      return `[${value.map(canonicalJson).join(",")}]`;
    }
    if (value !== null && typeof value === "object") {
      return `{${Object.keys(value).sort().map((key) => (
        `${JSON.stringify(key)}:${canonicalJson(value[key])}`
      )).join(",")}}`;
    }
    return JSON.stringify(value);
  }

  function assertExactObjectKeys(value, expected, field) {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      fail(`${field} must be an object`);
    }
    const actual = Object.keys(value).sort();
    const wanted = [...expected].sort();
    if (actual.join(",") !== wanted.join(",")) {
      fail(`${field} must have exactly these fields: ${wanted.join(", ")}`);
    }
  }

  function decodeCanonicalAuthority(encoded, field) {
    if (
      !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)
      || Buffer.from(encoded, "base64").toString("base64") !== encoded
    ) {
      fail(`${field} is not canonical base64`);
    }
    let text;
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.from(encoded, "base64"));
    } catch (error) {
      fail(`${field} is not UTF-8: ${error.message}`);
    }
    let authority;
    try {
      authority = JSON.parse(text);
    } catch (error) {
      fail(`${field} is not JSON: ${error.message}`);
    }
    if (canonicalJson(authority) !== text) {
      fail(`${field} JSON is not canonical`);
    }
    return authority;
  }

  function validateSemanticAsset(asset, expected, field) {
    const expectedKeys = [
      "role",
      "backend",
      "version",
      "platform",
      "artifact",
      "archive_format",
      "archive_path_prefix",
      "archive_sha256",
      "max_expanded_bytes",
      "max_files",
      "files",
    ];
    assertExactObjectKeys(
      asset,
      expectedKeys,
      field,
    );
    for (const key of [
      "role",
      "backend",
      "version",
      "platform",
      "artifact",
      "max_expanded_bytes",
      "max_files",
    ]) {
      const expectedValue = expected[key];
      if (asset[key] !== expectedValue) {
        fail(`${field} ${key} is ${asset[key]}, expected ${expectedValue}`);
      }
    }
    if (asset.archive_format !== expected.format) {
      fail(`${field} archive_format is ${asset.archive_format}, expected ${expected.format}`);
    }
    if (asset.archive_path_prefix !== expected.path_prefix) {
      fail(
        `${field} archive_path_prefix is ${asset.archive_path_prefix}, `
        + `expected ${expected.path_prefix}`,
      );
    }
    if (!["model", "cpu-runtime", "accelerator"].includes(asset.role)) {
      fail(`${field} has unsupported semantic asset role ${asset.role}`);
    }
    assertSafeArtifactName(asset.artifact, `${field} artifact`);
    assertSha(asset.archive_sha256, `${field} archive checksum`);
    if (asset.archive_sha256 !== asset.archive_sha256.toLowerCase()) {
      fail(`${field} archive checksum must be lowercase`);
    }
    if (!Array.isArray(asset.files) || asset.files.length === 0 || asset.files.length > expected.max_files) {
      fail(`${field} must contain 1..${expected.max_files} ordered file records`);
    }
    const allowedPaths = new Set(expected.exact_paths);
    const allowedPrefixes = expected.tree_prefixes;
    const relativePaths = [];
    const foldedPaths = new Set();
    let total = 0;
    let previousPath = "";
    for (const [index, record] of asset.files.entries()) {
      const recordField = `${field} file record ${index + 1}`;
      assertExactObjectKeys(record, ["path", "size", "sha256"], recordField);
      if (
        typeof record.path !== "string"
        || record.path === ""
        || record.path.includes("\\")
        || record.path.startsWith("/")
        || record.path.endsWith("/")
        || record.path.split("/").some((part) => part === "" || part === "." || part === "..")
      ) {
        fail(`${recordField} has unsafe or non-canonical path`);
      }
      if (record.path <= previousPath) {
        fail(`${field} file records must have unique paths in byte order`);
      }
      previousPath = record.path;
      const folded = record.path.toLowerCase();
      if (foldedPaths.has(folded)) {
        fail(`${field} file records contain a case collision: ${record.path}`);
      }
      foldedPaths.add(folded);
      const relative = record.path;
      if (
        !allowedPaths.has(relative)
        && !allowedPrefixes.some((prefix) => relative.startsWith(prefix))
      ) {
        fail(`${field} contains unexpected file ${record.path}`);
      }
      relativePaths.push(relative);
      if (!Number.isSafeInteger(record.size) || record.size <= 0) {
        fail(`${recordField} size must be a positive safe integer`);
      }
      total += record.size;
      if (!Number.isSafeInteger(total) || total > expected.max_expanded_bytes) {
        fail(`${field} exceeds expanded-size limit of ${expected.max_expanded_bytes} bytes`);
      }
      assertSha(record.sha256, `${recordField} checksum`);
      if (record.sha256 !== record.sha256.toLowerCase()) {
        fail(`${recordField} checksum must be lowercase`);
      }
    }
    const missingPaths = expected.exact_paths.filter((required) => !relativePaths.includes(required));
    const missingPrefixes = expected.tree_prefixes.filter(
      (required) => !relativePaths.some((value) => value.startsWith(required)),
    );
    if (missingPaths.length > 0 || missingPrefixes.length > 0) {
      fail(`${field} is missing required archive paths: ${[...missingPaths, ...missingPrefixes].join(", ")}`);
    }
  }

  function validateSemanticModelPublicationPin(modelPin, assets, field) {
    const modelAsset = assets.get(modelPin.asset_id);
    const modelFiles = new Map(modelAsset.files.map((record) => [record.path, record]));
    const expectedModelPaths = [
      ...modelPin.signed_metadata_paths,
      ...Object.keys(modelPin.runtime_files),
    ].sort();
    if (
      modelFiles.size !== 7
      || JSON.stringify([...modelFiles.keys()].sort()) !== JSON.stringify(expectedModelPaths)
    ) {
      fail(`${field} pinned model package must contain exactly seven signed file records`);
    }
    for (const [filePath, expected] of Object.entries(modelPin.runtime_files)) {
      const actual = modelFiles.get(filePath);
      for (const pinField of ["size", "sha256"]) {
        if (actual[pinField] !== expected[pinField]) {
          fail(
            `${field} immutable ${modelPin.asset_id} publication pin mismatch `
            + `for ${filePath} ${pinField}: `
            + `got ${actual[pinField]}, expected ${expected[pinField]}`,
          );
        }
      }
    }
  }

  function validateSemanticPublicationPins(assets, field) {
    validateSemanticModelPublicationPin(SEMANTIC_PUBLICATION_PINS.model, assets, field);
    validateSemanticModelPublicationPin(
      SEMANTIC_PUBLICATION_PINS.accelerator_model,
      assets,
      field,
    );
    const coremlPin = SEMANTIC_PUBLICATION_PINS.coreml;
    const coremlAsset = assets.get(coremlPin.asset_id);
    if (coremlAsset.archive_sha256 !== coremlPin.archive_sha256) {
      fail(
        `${field} immutable CoreML archive publication pin mismatch: `
        + `got ${coremlAsset.archive_sha256}, expected ${coremlPin.archive_sha256}`,
      );
    }
    const coremlManifest = coremlAsset.files.find(
      (record) => record.path === coremlPin.manifest_path,
    );
    if (coremlManifest?.sha256 !== coremlPin.manifest_sha256) {
      fail(
        `${field} immutable CoreML manifest publication pin mismatch: `
        + `got ${coremlManifest?.sha256 || "<missing>"}, expected ${coremlPin.manifest_sha256}`,
      );
    }

  }

  function parseSemanticCatalog(values, label) {
    const field = `${label} metadata CTX_RELEASE_SEMANTIC_ASSETS`;
    const catalog = decodeCanonicalAuthority(
      requireValue(values, "CTX_RELEASE_SEMANTIC_ASSETS", label),
      field,
    );
    assertExactObjectKeys(catalog, ["schema_version", "assets"], field);
    if (catalog.schema_version !== 1) {
      fail(`${field} has unsupported asset-catalog schema`);
    }
    assertExactObjectKeys(catalog.assets, Object.keys(SEMANTIC_LAYOUT.assets), `${field} assets`);
    const assets = new Map();
    for (const [assetId, expected] of Object.entries(SEMANTIC_LAYOUT.assets)) {
      const assetField = `${field} asset ${assetId}`;
      validateSemanticAsset(catalog.assets[assetId], expected, assetField);
      assets.set(assetId, catalog.assets[assetId]);
    }
    validateSemanticPublicationPins(assets, field);
    return assets;
  }

  function parseSemanticAuthorities(values, label) {
    const assets = parseSemanticCatalog(values, label);
    const prefix = "CTX_RELEASE_SEMANTIC_AUTHORITY_";
    const actual = metadataMatrixKeys(values, prefix);
    const expected = [...SEMANTIC_AUTHORITY_KEYS].sort();
    const missing = expected.filter((key) => !actual.includes(key));
    const unexpected = actual.filter((key) => !SEMANTIC_AUTHORITY_KEYS.has(key));
    if (missing.length > 0 || unexpected.length > 0) {
      const details = [];
      if (missing.length > 0) details.push(`missing ${missing.join(", ")}`);
      if (unexpected.length > 0) details.push(`unexpected ${unexpected.join(", ")}`);
      fail(`${label} metadata has wrong semantic target/backend matrix: ${details.join("; ")}`);
    }
    const authorities = new Map();
    for (const key of expected) {
      const field = `${label} metadata ${prefix}${key}`;
      const authority = decodeCanonicalAuthority(values[`${prefix}${key}`], field);
      const layout = SEMANTIC_LAYOUT_BY_KEY.get(key);
      assertExactObjectKeys(
        authority,
        [
          "schema_version",
          "target",
          "backend",
          "model_contract",
          "runtime_install_manifest_schema_version",
          "asset_ids",
        ],
        field,
      );
      if (authority.schema_version !== 1) {
        fail(`${field} has unsupported authority schema`);
      }
      if (authority.target !== layout.target || authority.backend !== layout.backend) {
        fail(
          `${field} selects ${authority.target}/${authority.backend}, `
          + `expected ${layout.target}/${layout.backend}`,
        );
      }
      if (canonicalJson(authority.model_contract) !== canonicalJson(SEMANTIC_LAYOUT.model_contract)) {
        fail(`${field} does not use the pinned multilingual E5 contract`);
      }
      if (
        authority.runtime_install_manifest_schema_version
        !== SEMANTIC_LAYOUT.runtime_install_manifest_contract.schema_version
      ) {
        fail(`${field} does not require verified runtime install-manifest schema 1`);
      }
      if (
        !Array.isArray(authority.asset_ids)
        || authority.asset_ids.some((assetId) => typeof assetId !== "string")
        || authority.asset_ids.join("\n") !== layout.asset_ids.join("\n")
      ) {
        fail(`${field} has the wrong normalized asset composition`);
      }
      authorities.set(key, authority);
    }
    return { authorities, assets };
  }

  return {
    SEMANTIC_ASSET_LAYOUTS,
    SEMANTIC_AUTHORITY_KEYS,
    SEMANTIC_LAYOUT_BY_KEY,
    canonicalJson,
    decodeCanonicalAuthority,
    parseSemanticAuthorities,
    parseSemanticCatalog,
  };
}

module.exports = { createSemanticContract };
