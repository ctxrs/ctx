import { compareReleaseVersions } from "./release-version.cjs";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

import {
  loadTargetMatrix,
  verifyEnvelope,
} from "./managed-pair-release-contract.mjs";
import { assertCurrentReleaseVersion } from "./frozen-cli-bridge.cjs";
import { releaseTrust } from "./release-authority.mjs";

const MATRIX_PATH = path.resolve(
  import.meta.dirname,
  "../../contracts/release-targets-v1.json",
);
const STABLE_VERSION = /^v(1\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))$/u;
const BASE_URL_PREFIX = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable";
const TARGETS = Object.freeze([
  Object.freeze({ id: "linux-arm64", metadata: "linux_aarch64", coreAlias: "ctx-linux-aarch64" }),
  Object.freeze({ id: "linux-x64", metadata: "linux_x64", coreAlias: "ctx" }),
  Object.freeze({ id: "macos-arm64", metadata: "macos_arm64", coreAlias: "ctx-macos-arm64" }),
  Object.freeze({ id: "macos-x64", metadata: "macos_x64", coreAlias: "ctx-macos-x64" }),
  Object.freeze({ id: "windows-x64", metadata: "windows_x64", coreAlias: "ctx.exe" }),
]);
const PUBLICATION_KEYS = Object.freeze([
  "channel", "component_objects", "contract", "pointer_object", "release_name",
  "release_set_object", "rollback_generation", "schema_version",
  "target_manifest_objects", "target_matrix_sha256",
]);
const RUNTIME_TRANSPORTS = Object.freeze([
  Object.freeze({ metadata: "linux_x64", name: "ctx-onnxruntime-linux-x64.tar.gz" }),
  Object.freeze({ metadata: "linux_aarch64", name: "ctx-onnxruntime-linux-aarch64.tar.gz" }),
  Object.freeze({ metadata: "windows_x64", name: "ctx-onnxruntime-windows-x64.zip" }),
  Object.freeze({ metadata: "macos_x64", name: "ctx-onnxruntime-macos-x64.tar.gz" }),
  Object.freeze({ metadata: "macos_arm64", name: "ctx-onnxruntime-macos-arm64.tar.gz" }),
]);
const RUNTIME_HANDOFF_KEYS = Object.freeze([
  "contract", "schema_version", "release_name", "public_source_commit",
  "runtime_version", "artifacts",
]);
const RUNTIME_ARTIFACT_KEYS = Object.freeze([
  "metadata", "name", "path", "sha256", "size_bytes",
  "source_name", "source_path", "source_sha256", "source_size_bytes",
]);
const LOWER_SHA256 = /^[0-9a-f]{64}$/u;
const SOURCE_COMMIT = /^[0-9a-f]{40}$/u;
const RUNTIME_VERSION = /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$/u;

function fail(message) { throw new Error(message); }
export function sha256(body) { return crypto.createHash("sha256").update(body).digest("hex"); }

function exactKeys(value, expected, label) {
  if (value == null || typeof value !== "object" || Array.isArray(value)
      || Object.keys(value).sort().join("\0") !== [...expected].sort().join("\0")) {
    fail(`${label} has missing or unknown fields`);
  }
}

function exactOrderedKeys(value, expected, label) {
  exactKeys(value, expected, label);
  if (Object.keys(value).join("\0") !== expected.join("\0")) {
    fail(`${label} fields are not in canonical order`);
  }
}

function readStableFile(file, label, maximumBytes) {
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

function parseJsonFile(file, label, maximumBytes) {
  const snapshot = readStableFile(file, label, maximumBytes);
  let value;
  try {
    value = JSON.parse(snapshot.body.toString("utf8"));
  } catch {
    fail(`${label} is not JSON`);
  }
  return { snapshot, value };
}

export function loadHostedManagedPairPublication(publicationPath) {
  const { snapshot, value: publication } = parseJsonFile(
    publicationPath,
    "managed-pair publication",
    1024 * 1024,
  );
  exactKeys(publication, PUBLICATION_KEYS, "managed-pair publication");
  const matrix = loadTargetMatrix(MATRIX_PATH);
  const versionMatch = typeof publication.release_name === "string"
    ? STABLE_VERSION.exec(publication.release_name)
    : null;
  const version = versionMatch?.[1];
  if (publication.contract !== "ctx-managed-pair-publication"
      || publication.schema_version !== 1
      || publication.channel !== "stable"
      || version == null
      || !Number.isSafeInteger(publication.rollback_generation)
      || publication.rollback_generation < 1
      || publication.target_matrix_sha256 !== matrix.digest
      || publication.pointer_object !== "channels/stable/managed-pair.json"
      || !Array.isArray(publication.component_objects)
      || publication.component_objects.length !== TARGETS.length * 2
      || !Array.isArray(publication.target_manifest_objects)
      || publication.target_manifest_objects.length !== TARGETS.length) {
    fail("managed-pair publication is not an exact stable 1.x authority");
  }
  assertCurrentReleaseVersion(version);
  const releaseSet = readStableFile(
    publication.release_set_object?.path,
    "managed-pair release set",
    2 * 1024 * 1024,
  );
  if (releaseSet.sha256 !== publication.release_set_object?.sha256
      || releaseSet.size !== publication.release_set_object?.size_bytes) {
    fail("managed-pair release set differs from publication evidence");
  }
  const releaseSetPayload = verifyEnvelope(releaseSet.body, matrix, releaseTrust("stable")).payload;
  if (releaseSetPayload.contract !== "ctx-managed-pair-release-set"
      || releaseSetPayload.release_name !== publication.release_name
      || releaseSetPayload.rollback_generation !== publication.rollback_generation) {
    fail("managed-pair release set differs from the selected stable 1.x release");
  }

  let publicCommit;
  let privateCommit;
  const targets = new Map();
  for (const target of TARGETS) {
    const manifestRecord = publication.target_manifest_objects.find(
      (entry) => entry?.target_id === target.id,
    );
    const manifest = readStableFile(
      manifestRecord?.path,
      `managed-pair ${target.id} envelope`,
      2 * 1024 * 1024,
    );
    if (manifest.sha256 !== manifestRecord?.sha256
        || manifest.size !== manifestRecord?.size_bytes
        || manifestRecord?.object_key !== `sha256/${manifest.sha256}/${manifestRecord?.name}`) {
      fail(`managed-pair ${target.id} envelope differs from publication evidence`);
    }
    const releaseSetReference = releaseSetPayload.target_manifests.find(
      (entry) => entry?.target_id === target.id,
    );
    if (releaseSetReference?.manifest_name !== manifestRecord.name
        || releaseSetReference?.manifest_object_key !== manifestRecord.object_key
        || releaseSetReference?.manifest_sha256 !== manifest.sha256
        || releaseSetReference?.manifest_size_bytes !== manifest.size) {
      fail(`managed-pair release set does not select the published ${target.id} envelope`);
    }
    const payload = verifyEnvelope(manifest.body, matrix, releaseTrust("stable")).payload;
    if (payload.contract !== "ctx-managed-pair-manifest" || payload.target.id !== target.id
        || payload.release_name !== publication.release_name
        || payload.rollback_generation !== publication.rollback_generation) {
      fail(`managed-pair ${target.id} envelope has the wrong release identity`);
    }
    const records = {};
    for (const component of ["core", "companion"]) {
      const record = publication.component_objects.find(
        (entry) => entry?.target_id === target.id && entry?.component === component,
      );
      const identity = payload.components[component];
      const artifact = readStableFile(
        record?.path,
        `${target.id} ${component} artifact`,
        256 * 1024 * 1024,
      );
      if (record?.name !== identity.artifact_name
          || record?.object_key !== identity.object_key
          || artifact.sha256 !== record?.sha256 || artifact.sha256 !== identity.sha256
          || artifact.size !== record?.size_bytes || artifact.size !== identity.size_bytes) {
        fail(`${target.id} ${component} artifact differs from its signed identity`);
      }
      records[component] = Object.freeze({ ...record, artifact, identity });
    }
    if (compareReleaseVersions(version, "1.5.0") >= 0) {
      const { core, companion } = payload.components;
      if (core.sha256 !== companion.sha256 || core.size_bytes !== companion.size_bytes
          || core.size_bytes > 256 * 1024 * 1024
          || core.build_identity.source_revision !== companion.build_identity.source_revision
          || core.build_identity.build_fingerprint !== companion.build_identity.build_fingerprint) {
        fail("unified legacy projection must bind one bounded public executable per platform");
      }
    }
    publicCommit ??= payload.components.core.build_identity.source_revision;
    privateCommit ??= payload.components.companion.build_identity.source_revision;
    if (payload.components.core.build_identity.source_revision !== publicCommit
        || payload.components.companion.build_identity.source_revision !== privateCommit) {
      fail("managed-pair target manifests do not share one exact source pair");
    }
    targets.set(target.id, Object.freeze({ ...target, manifest, manifestRecord, payload, ...records }));
  }
  if (!/^[0-9a-f]{40}$/u.test(publicCommit) || !/^[0-9a-f]{40}$/u.test(privateCommit)) {
    fail("managed-pair source identity is invalid");
  }
  return Object.freeze({
    baseUrl: `${BASE_URL_PREFIX}/${version}`,
    matrix,
    privateCommit,
    publication,
    publicationSnapshot: snapshot,
    publicCommit,
    releaseSet,
    targets,
    version,
  });
}

export function loadRuntimeTransportHandoff(handoffPath, loaded) {
  const { snapshot, value } = parseJsonFile(
    handoffPath,
    "runtime transport handoff",
    256 * 1024,
  );
  exactOrderedKeys(value, RUNTIME_HANDOFF_KEYS, "runtime transport handoff");
  const canonical = Buffer.from(`${JSON.stringify(value)}\n`, "utf8");
  if (!snapshot.body.equals(canonical)) {
    fail("runtime transport handoff is not canonical compact JSON");
  }
  if (value.contract !== "ctx-runtime-transport-handoff"
      || value.schema_version !== 1
      || value.release_name !== loaded.publication.release_name
      || value.public_source_commit !== loaded.publicCommit
      || !SOURCE_COMMIT.test(value.public_source_commit)
      || !RUNTIME_VERSION.test(value.runtime_version)
      || !Array.isArray(value.artifacts)
      || value.artifacts.length !== RUNTIME_TRANSPORTS.length) {
    fail("runtime transport handoff does not match the managed-pair release");
  }
  const root = path.dirname(snapshot.absolute);
  const artifacts = new Map();
  for (let index = 0; index < RUNTIME_TRANSPORTS.length; index += 1) {
    const expected = RUNTIME_TRANSPORTS[index];
    const expectedSourceName = expected.metadata === "windows_x64"
      ? expected.name
      : expected.name.replace(/\.tar\.gz$/u, ".tar.zst");
    const record = value.artifacts[index];
    exactOrderedKeys(record, RUNTIME_ARTIFACT_KEYS, `runtime ${expected.metadata} record`);
    if (record.metadata !== expected.metadata
        || record.name !== expected.name
        || record.path !== expected.name
        || !LOWER_SHA256.test(record.sha256)
        || record.sha256 === "0".repeat(64)
        || !Number.isSafeInteger(record.size_bytes)
        || record.size_bytes < 1
        || record.source_name !== expectedSourceName
        || record.source_path !== expectedSourceName
        || !LOWER_SHA256.test(record.source_sha256)
        || record.source_sha256 === "0".repeat(64)
        || !Number.isSafeInteger(record.source_size_bytes)
        || record.source_size_bytes < 1) {
      fail(`runtime ${expected.metadata} record is invalid`);
    }
    const artifact = readStableFile(
      path.join(root, record.path),
      `${expected.metadata} runtime transport`,
      1024 * 1024 * 1024,
    );
    if (artifact.sha256 !== record.sha256 || artifact.size !== record.size_bytes) {
      fail(`${expected.metadata} runtime transport differs from its handoff identity`);
    }
    const source = record.source_path === record.path
      ? artifact
      : readStableFile(
        path.join(root, record.source_path),
        `${expected.metadata} runtime producer archive`,
        1024 * 1024 * 1024,
      );
    if (source.sha256 !== record.source_sha256
        || source.size !== record.source_size_bytes) {
      fail(`${expected.metadata} runtime producer archive differs from its handoff identity`);
    }
    artifacts.set(expected.metadata, Object.freeze({ ...record, artifact, source }));
  }
  return Object.freeze({
    artifacts,
    publicCommit: value.public_source_commit,
    releaseName: value.release_name,
    snapshot,
    version: value.runtime_version,
  });
}

export function renderHostedManagedPairMetadata(loaded, publishedAt, runtimeHandoff) {
  assertCurrentReleaseVersion(loaded.version);
  if (typeof publishedAt !== "string" || publishedAt !== new Date(publishedAt).toISOString()) {
    fail("hosted publication timestamp must be canonical ISO-8601 UTC");
  }
  if (runtimeHandoff == null) {
    fail("hosted stable metadata requires the complete runtime transport handoff");
  }
  const lines = [
    "CTX_RELEASE_SCHEMA_VERSION=1",
    `CTX_RELEASE_VERSION=${loaded.version}`,
    "CTX_RELEASE_CHANNEL=stable",
    `CTX_RELEASE_BASE_URL=${loaded.baseUrl}`,
    `CTX_RELEASE_SOURCE_COMMIT=${loaded.publicCommit}`,
    `CTX_RELEASE_PUBLISHED_AT=${publishedAt}`,
    "CTX_RELEASE_SELF_UPGRADE_ALLOWED=true",
    "CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true",
  ];
  for (const target of TARGETS) {
    const selected = loaded.targets.get(target.id);
    lines.push(
      `CTX_RELEASE_ARTIFACT_${target.metadata}=${target.coreAlias}`,
      `CTX_RELEASE_SHA256_${target.metadata}=${selected.core.artifact.sha256}`,
    );
  }
  if (runtimeHandoff.releaseName !== loaded.publication.release_name
      || runtimeHandoff.publicCommit !== loaded.publicCommit) {
    fail("runtime transport handoff does not match the managed-pair release");
  }
  lines.push(`CTX_RELEASE_ONNXRUNTIME_VERSION=${runtimeHandoff.version}`);
  for (const expected of RUNTIME_TRANSPORTS) {
    const selected = runtimeHandoff.artifacts.get(expected.metadata);
    if (selected?.name !== expected.name || !LOWER_SHA256.test(selected?.sha256 ?? "")) {
      fail("runtime transport handoff is incomplete");
    }
    lines.push(
      `CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${expected.metadata}=${selected.name}`,
      `CTX_RELEASE_ONNXRUNTIME_SHA256_${expected.metadata}=${selected.sha256}`,
    );
  }
  for (const target of TARGETS) {
    const selected = loaded.targets.get(target.id);
    lines.push(
      `CTX_RELEASE_MANAGED_PAIR_ENVELOPE_${target.metadata}=${selected.manifestRecord.name}`,
      `CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_${target.metadata}=${selected.core.identity.object_key}`,
      `CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_${target.metadata}=${selected.core.artifact.sha256}`,
      `CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_${target.metadata}=${selected.companion.identity.object_key}`,
      `CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_${target.metadata}=${selected.companion.artifact.sha256}`,
    );
  }
  return Buffer.from(`${lines.join("\n")}\n`, "utf8");
}

export function validateHostedManagedPairMetadata(
  metadata,
  loaded,
  runtimeHandoff,
  metadataExtension = Buffer.alloc(0),
) {
  if (!Buffer.isBuffer(metadata) || metadata.length > 128 * 1024) {
    fail("hosted managed-pair metadata is not bounded bytes");
  }
  if (!Buffer.isBuffer(metadataExtension) || metadataExtension.length > 128 * 1024) {
    fail("hosted managed-pair metadata extension is not bounded bytes");
  }
  const match = /^CTX_RELEASE_PUBLISHED_AT=([^\r\n]+)$/mu.exec(metadata.toString("utf8"));
  if (match == null) fail("hosted managed-pair metadata omits its publication timestamp");
  const expected = Buffer.concat([
    renderHostedManagedPairMetadata(loaded, match[1], runtimeHandoff),
    metadataExtension,
  ]);
  if (!metadata.equals(expected)) fail("hosted managed-pair metadata differs from exact publication authority");
  return match[1];
}

export const HOSTED_MANAGED_PAIR_TARGETS = TARGETS;
export const HOSTED_RUNTIME_TRANSPORTS = RUNTIME_TRANSPORTS;
