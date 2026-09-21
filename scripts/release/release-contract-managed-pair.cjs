"use strict";


const MANAGED_PAIR_COMPANIONS = new Map([
  ["linux_x64", "ctx-pro-linux-x64"],
  ["linux_aarch64", "ctx-pro-linux-arm64"],
  ["macos_arm64", "ctx-pro-macos-arm64"],
  ["macos_x64", "ctx-pro-macos-x64"],
  ["windows_x64", "ctx-pro-windows-x64.exe"],
]);
const MANAGED_PAIR_TARGET_IDS = new Map([
  ["linux_x64", "linux-x64"],
  ["linux_aarch64", "linux-arm64"],
  ["macos_arm64", "macos-arm64"],
  ["macos_x64", "macos-x64"],
  ["windows_x64", "windows-x64"],
]);
const RUNTIME_TRANSPORTS = [
  { key: "linux_x64", artifact: "ctx-onnxruntime-linux-x64.tar.gz" },
  { key: "linux_aarch64", artifact: "ctx-onnxruntime-linux-aarch64.tar.gz" },
  { key: "windows_x64", artifact: "ctx-onnxruntime-windows-x64.zip" },
  { key: "macos_x64", artifact: "ctx-onnxruntime-macos-x64.tar.gz" },
  { key: "macos_arm64", artifact: "ctx-onnxruntime-macos-arm64.tar.gz" },
];

function createManagedPairContract({
  MATRIX,
  MATRIX_KEYS,
  assertSha,
  fail,
  metadataMatrixKeys,
  parseSemanticAuthorities,
  requireCandidateManifestDigestMatrix,
  requireValue,
}) {
  function assertExactTargetKeys(values, label, prefix, matrixLabel) {
    const expected = [...MATRIX_KEYS].sort();
    const actual = metadataMatrixKeys(values, prefix);
    if (actual.join("\n") !== expected.join("\n")) {
      const missing = expected.filter((key) => !actual.includes(key));
      const unexpected = actual.filter((key) => !MATRIX_KEYS.has(key));
      fail(
        `${label} metadata has wrong ${matrixLabel} matrix for ${prefix}: `
          + `missing ${missing.join(", ") || "<none>"}; `
          + `unexpected ${unexpected.join(", ") || "<none>"}`,
      );
    }
  }

  function assertContentAddressedObject(value, digest, artifact, field) {
    if (value !== `sha256/${digest}/${artifact}`) {
      fail(`${field} is not the exact content-addressed object key`);
    }
  }

  function validateManagedPairMatrix(values, label) {
    const fields = [
      "ENVELOPE",
      "CORE_OBJECT",
      "CORE_SHA256",
      "COMPANION_OBJECT",
      "COMPANION_SHA256",
    ];
    for (const field of fields) {
      assertExactTargetKeys(
        values,
        label,
        `CTX_RELEASE_MANAGED_PAIR_${field}_`,
        `managed-pair ${field.toLowerCase()}`,
      );
    }
    for (const entry of MATRIX) {
      const envelope = requireValue(
        values,
        `CTX_RELEASE_MANAGED_PAIR_ENVELOPE_${entry.key}`,
        label,
      );
      const expectedEnvelope = `ctx-managed-pair-${MANAGED_PAIR_TARGET_IDS.get(entry.key)}.json`;
      if (envelope !== expectedEnvelope) {
        fail(`${label} metadata managed-pair envelope for ${entry.platform} is not ${expectedEnvelope}`);
      }
      const coreSha = requireValue(values, `CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_${entry.key}`, label);
      const companionSha = requireValue(
        values,
        `CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_${entry.key}`,
        label,
      );
      assertSha(coreSha, `${label} managed-pair Core checksum for ${entry.platform}`);
      assertSha(companionSha, `${label} managed-pair companion checksum for ${entry.platform}`);
      if (coreSha !== values[`CTX_RELEASE_SHA256_${entry.key}`]) {
        fail(`${label} metadata managed-pair Core checksum differs from the Core artifact matrix`);
      }
      assertContentAddressedObject(
        requireValue(values, `CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_${entry.key}`, label),
        coreSha,
        entry.candidateArtifact,
        `${label} managed-pair Core object for ${entry.platform}`,
      );
      assertContentAddressedObject(
        requireValue(values, `CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_${entry.key}`, label),
        companionSha,
        MANAGED_PAIR_COMPANIONS.get(entry.key),
        `${label} managed-pair companion object for ${entry.platform}`,
      );
    }
  }

  function validateRuntimeTransportMatrix(values, label) {
    const runtimeKeys = Object.keys(values).filter((key) => key.startsWith("CTX_RELEASE_ONNXRUNTIME_"));
    const expected = new Set(["CTX_RELEASE_ONNXRUNTIME_VERSION"]);
    for (const entry of RUNTIME_TRANSPORTS) {
      expected.add(`CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${entry.key}`);
      expected.add(`CTX_RELEASE_ONNXRUNTIME_SHA256_${entry.key}`);
    }
    if (runtimeKeys.length !== expected.size || runtimeKeys.some((key) => !expected.has(key))) {
      fail(`${label} metadata has a partial or unexpected ONNX Runtime transport matrix`);
    }
    const version = requireValue(values, "CTX_RELEASE_ONNXRUNTIME_VERSION", label);
    if (!/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(version)) {
      fail(`${label} metadata ONNX Runtime version is not canonical SemVer`);
    }
    for (const entry of RUNTIME_TRANSPORTS) {
      const artifact = requireValue(values, `CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${entry.key}`, label);
      const digest = requireValue(values, `CTX_RELEASE_ONNXRUNTIME_SHA256_${entry.key}`, label);
      if (artifact !== entry.artifact) {
        fail(`${label} metadata ONNX Runtime artifact for ${entry.key} is not ${entry.artifact}`);
      }
      assertSha(digest, `${label} ONNX Runtime checksum for ${entry.key}`);
    }
  }

  function validateSupplementaryProfile(values, label) {
    const hasRuntime = Object.keys(values).some((key) => key.startsWith("CTX_RELEASE_ONNXRUNTIME_"));
    const hasSemantic = Object.keys(values).some((key) => key.startsWith("CTX_RELEASE_SEMANTIC_"));
    const hasCandidates = Object.keys(values).some(
      (key) => key.startsWith("CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_"),
    );
    if (!hasRuntime && !hasSemantic && !hasCandidates) {
      fail(`${label} metadata is missing the required supplementary release profile`);
    }
    if (!hasRuntime || !hasSemantic || !hasCandidates) {
      fail(`${label} metadata has a partial supplementary release profile`);
    }
    validateRuntimeTransportMatrix(values, label);
    requireCandidateManifestDigestMatrix(values, label);
    const semanticSchema = requireValue(values, "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION", label);
    if (semanticSchema !== "1") {
      fail(`${label} metadata semantic runtime schema is ${semanticSchema}, expected 1`);
    }
    parseSemanticAuthorities(values, label);
    return "complete";
  }

  return {
    RUNTIME_TRANSPORTS,
    validateManagedPairMatrix,
    validateSupplementaryProfile,
  };
}

module.exports = { createManagedPairContract };
