import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { TextDecoder } from "node:util";

const SEMANTIC_ARTIFACTS = Object.freeze([
  Object.freeze({ name: "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz", contentType: "application/x-xz" }),
  Object.freeze({ name: "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz", contentType: "application/x-xz" }),
  Object.freeze({ name: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz", contentType: "application/x-xz" }),
  Object.freeze({ name: "ctx-onnxruntime-linux-x64.tar.zst", contentType: "application/zstd" }),
  Object.freeze({ name: "ctx-onnxruntime-linux-aarch64.tar.zst", contentType: "application/zstd" }),
  Object.freeze({ name: "ctx-onnxruntime-macos-arm64.tar.zst", contentType: "application/zstd" }),
  Object.freeze({ name: "ctx-onnxruntime-macos-x64.tar.zst", contentType: "application/zstd" }),
  Object.freeze({ name: "ctx-windowsml-windows-x64.zip", contentType: "application/zip" }),
  Object.freeze({ name: "ctx-onnxruntime-linux-x64-cuda12.tar.zst", contentType: "application/zstd" }),
]);
const CANDIDATE_MANIFESTS = Object.freeze([
  Object.freeze({ key: "linux_x64", name: "ctx.candidate.json" }),
  Object.freeze({ key: "linux_aarch64", name: "ctx-linux-aarch64.candidate.json" }),
  Object.freeze({ key: "macos_arm64", name: "ctx-macos-arm64.candidate.json" }),
  Object.freeze({ key: "macos_x64", name: "ctx-macos-x64.candidate.json" }),
  Object.freeze({ key: "windows_x64", name: "ctx.exe.candidate.json" }),
]);
const CORE_GITHUB_HANDOFF = "ctx-core-github-handoff.json";
const CORE_GITHUB_HANDOFF_METADATA_KEY =
  "CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256";
const SEMANTIC_METADATA_KEYS = Object.freeze([
  "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION",
  "CTX_RELEASE_SEMANTIC_ASSETS",
  "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml",
  "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml",
  "CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda",
  "CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu",
]);
const LOWER_SHA256 = /^[0-9a-f]{64}$/u;
const UTF8 = new TextDecoder("utf-8", { fatal: true });

function fail(message) { throw new Error(message); }
export function sha256(body) {
  return crypto.createHash("sha256").update(body).digest("hex");
}

export function readStableSemanticFile(file, label, maximumBytes) {
  const absolute = path.resolve(file);
  let descriptor;
  try {
    descriptor = fs.openSync(
      absolute,
      fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0),
    );
  } catch {
    fail(`${label} is not an identity-safe bounded file`);
  }
  try {
    const before = fs.fstatSync(descriptor, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n
        || before.size < 1n || before.size > BigInt(maximumBytes)) {
      fail(`${label} is not an identity-safe bounded file`);
    }
    const body = fs.readFileSync(descriptor);
    const after = fs.fstatSync(descriptor, { bigint: true });
    const current = fs.lstatSync(absolute, { bigint: true });
    if (!current.isFile() || current.isSymbolicLink() || current.nlink !== 1n
        || before.dev !== after.dev || before.ino !== after.ino
        || before.size !== after.size || before.mtimeNs !== after.mtimeNs
        || before.ctimeNs !== after.ctimeNs || BigInt(body.length) !== before.size
        || current.dev !== after.dev || current.ino !== after.ino
        || current.size !== after.size || current.mtimeNs !== after.mtimeNs
        || current.ctimeNs !== after.ctimeNs) {
      fail(`${label} changed while being read`);
    }
    return Object.freeze({ absolute, body, sha256: sha256(body), size: body.length });
  } finally {
    fs.closeSync(descriptor);
  }
}

function parseMetadata(body, label) {
  let text;
  try {
    text = UTF8.decode(body);
  } catch {
    fail(`${label} is not UTF-8`);
  }
  if (!text.endsWith("\n") || text.includes("\r")) fail(`${label} is not canonical text`);
  const values = new Map();
  const keys = [];
  for (const [index, line] of text.slice(0, -1).split("\n").entries()) {
    const equals = line.indexOf("=");
    const key = line.slice(0, equals);
    const value = line.slice(equals + 1);
    if (equals <= 0 || !/^[A-Za-z0-9_]+$/u.test(key) || value === ""
        || value !== value.trim() || values.has(key)) {
      fail(`${label} line ${index + 1} is not canonical KEY=value metadata`);
    }
    values.set(key, value);
    keys.push(key);
  }
  return { keys, values };
}

function decodeCanonicalBase64Json(value, label) {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(value)) {
    fail(`${label} is not canonical base64`);
  }
  const body = Buffer.from(value, "base64");
  if (body.toString("base64") !== value) fail(`${label} is not canonical base64`);
  let decoded;
  try {
    decoded = UTF8.decode(body);
    return JSON.parse(decoded);
  } catch {
    fail(`${label} is not UTF-8 JSON`);
  }
}

function semanticMetadataSnapshot(artifactDir) {
  const snapshot = readStableSemanticFile(
    path.join(path.resolve(artifactDir), "semantic-release.env"),
    "semantic release metadata handoff",
    128 * 1024,
  );
  const parsed = parseMetadata(snapshot.body, "semantic release metadata handoff");
  if (parsed.keys.join("\0") !== SEMANTIC_METADATA_KEYS.join("\0")
      || parsed.values.get("CTX_RELEASE_SEMANTIC_SCHEMA_VERSION") !== "1") {
    fail("semantic release metadata handoff has the wrong exact field set");
  }
  return { ...snapshot, values: parsed.values };
}

export function renderHostedSemanticMetadataExtension(
  semanticArtifactDir,
  candidateManifestHandoff,
  expectedCoreGithubHandoffSha256,
) {
  const semantic = semanticMetadataSnapshot(semanticArtifactDir);
  const candidateRoot = path.resolve(candidateManifestHandoff);
  if (!LOWER_SHA256.test(expectedCoreGithubHandoffSha256 ?? "")
      || expectedCoreGithubHandoffSha256 === "0".repeat(64)) {
    fail("expected Core GitHub handoff digest is invalid");
  }
  const handoff = readStableSemanticFile(
    path.join(candidateRoot, CORE_GITHUB_HANDOFF),
    "Core GitHub handoff document",
    16 * 1024 * 1024,
  );
  if (handoff.sha256 !== expectedCoreGithubHandoffSha256) {
    fail("Core GitHub handoff differs from its independent expected digest");
  }
  const lines = [
    `${CORE_GITHUB_HANDOFF_METADATA_KEY}=${expectedCoreGithubHandoffSha256}`,
    ...CANDIDATE_MANIFESTS.map((candidate) => {
      const manifest = readStableSemanticFile(
        path.join(candidateRoot, candidate.name),
        `public candidate manifest ${candidate.name}`,
        16 * 1024 * 1024,
      );
      return `CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_${candidate.key}=${manifest.sha256}`;
    }),
  ];
  return Buffer.concat([
    Buffer.from(`${lines.join("\n")}\n`, "utf8"),
    semantic.body,
  ]);
}

export function loadHostedSemanticAssetCatalog(metadata) {
  const parsed = parseMetadata(metadata, "hosted release metadata");
  const encoded = parsed.values.get("CTX_RELEASE_SEMANTIC_ASSETS");
  const catalog = decodeCanonicalBase64Json(encoded ?? "", "semantic asset catalog");
  if (catalog?.schema_version !== 1 || catalog.assets == null
      || typeof catalog.assets !== "object" || Array.isArray(catalog.assets)) {
    fail("semantic asset catalog has the wrong schema");
  }
  const records = new Map();
  for (const asset of Object.values(catalog.assets)) {
    if (asset == null || typeof asset !== "object" || Array.isArray(asset)
        || typeof asset.artifact !== "string"
        || !LOWER_SHA256.test(asset.archive_sha256 ?? "")
        || records.has(asset.artifact)) {
      fail("semantic asset catalog has an invalid or repeated archive identity");
    }
    records.set(asset.artifact, asset.archive_sha256);
  }
  if (records.size !== SEMANTIC_ARTIFACTS.length
      || SEMANTIC_ARTIFACTS.some((artifact) => !records.has(artifact.name))) {
    fail("semantic asset catalog does not contain the exact hosted archive set");
  }
  return SEMANTIC_ARTIFACTS.map((artifact) => Object.freeze({
    ...artifact,
    sha256: records.get(artifact.name),
  }));
}

export function snapshotHostedSemanticAsset(artifactDir, asset) {
  const expected = SEMANTIC_ARTIFACTS.find((candidate) => candidate.name === asset?.name);
  if (expected == null || asset.contentType !== expected.contentType
      || !LOWER_SHA256.test(asset.sha256 ?? "")) {
    fail("semantic release archive has an invalid publication identity");
  }
  const snapshot = readStableSemanticFile(
    path.join(path.resolve(artifactDir), expected.name),
    `semantic release archive ${expected.name}`,
    2 * 1024 * 1024 * 1024,
  );
  if (snapshot.sha256 !== asset.sha256) {
    fail(`semantic release archive ${expected.name} differs from signed metadata`);
  }
  return snapshot;
}

export const HOSTED_SEMANTIC_ARTIFACTS = SEMANTIC_ARTIFACTS;
export const HOSTED_CANDIDATE_MANIFESTS = CANDIDATE_MANIFESTS;
