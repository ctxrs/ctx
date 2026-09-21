#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";
import zlib from "node:zlib";
import { assertCurrentReleaseVersion, assertFrozenBridgePromotion } from "./frozen-cli-bridge.cjs";
import candidateManifestContract from "./release-candidate-manifest-contract.cjs";

import { CLI_METADATA_PUBLIC_KEY_PEM } from "../../services/install-site/src/cli-install-script.js";
import {
  createR2Request,
  getR2Object,
  putImmutableR2Object,
} from "./core-r2.mjs";
import {
  HOSTED_MANAGED_PAIR_TARGETS,
  HOSTED_RUNTIME_TRANSPORTS,
  loadHostedManagedPairPublication,
  loadRuntimeTransportHandoff,
  renderHostedManagedPairMetadata,
  sha256,
  validateHostedManagedPairMetadata,
} from "./hosted-managed-pair-release.mjs";
import {
  loadHostedSemanticAssetCatalog,
  renderHostedSemanticMetadataExtension,
  snapshotHostedSemanticAsset,
} from "./hosted-semantic-release.mjs";
import {
  verifyRuntimeTransportHandoff,
} from "./verify-runtime-transport-handoff.mjs";

const STABLE_BUCKET = "ctx-releases-prod";
const R2_AUTHORITY = Object.freeze({
  accessKeyEnv: "CTX_RELEASE_R2_ACCESS_KEY_ID",
  bucket: STABLE_BUCKET,
  endpointEnv: "CTX_RELEASE_R2_ENDPOINT",
  label: "stable Core release",
  secretKeyEnv: "CTX_RELEASE_R2_SECRET_ACCESS_KEY",
});

function fail(message) { throw new Error(message); }

export function parseArgs(argv) {
  const command = argv[0];
  if (!new Set(["prepare", "publish"]).has(command)) fail("expected prepare or publish");
  const args = new Map();
  for (let index = 1; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith("--") || value == null || args.has(name)) fail("invalid arguments");
    args.set(name, value);
  }
  const base = command === "prepare"
    ? ["--metadata-out", "--public-ctx-repo", "--publication", "--published-at"]
    : ["--evidence-out", "--metadata", "--public-ctx-repo", "--publication", "--signature"];
  const required = [
    ...base,
    "--candidate-handoff-sha256",
    "--candidate-manifest-handoff",
    "--runtime-handoff",
    "--semantic-artifact-dir",
  ];
  if (args.size !== required.length || required.some((name) => !args.has(name))) {
    fail(`${command} requires the exact complete release handoff arguments`);
  }
  return { args, command };
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
    return body;
  } finally {
    fs.closeSync(descriptor);
  }
}

function writeExclusive(file, body, mode) {
  const absolute = path.resolve(file);
  fs.mkdirSync(path.dirname(absolute), { recursive: true, mode: 0o700 });
  const descriptor = fs.openSync(absolute, "wx", mode);
  try {
    fs.writeFileSync(descriptor, body);
    fs.fchmodSync(descriptor, mode);
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
}

export function strictSignature(file, metadata) {
  const body = readStableFile(file, "metadata signature", 16 * 1024);
  const text = body.toString("utf8");
  if (!/^[A-Za-z0-9+/]+={0,2}\n$/u.test(text)) fail("metadata signature is not canonical base64");
  const signature = Buffer.from(text.trim(), "base64");
  if (signature.toString("base64") !== text.trim()
      || !crypto.verify("RSA-SHA256", metadata, CLI_METADATA_PUBLIC_KEY_PEM, signature)) {
    fail("metadata signature does not verify with installer trust");
  }
  return body;
}

export function immutableArtifactObjects(loaded, runtimeHandoff) {
  const root = `artifacts/stable/${loaded.version}`;
  const byKey = new Map();
  const add = (key, body, contentType) => {
    const current = byKey.get(key);
    if (current != null && !current.body.equals(body)) fail(`publication repeats ${key}`);
    byKey.set(key, { body, contentType, key });
  };
  for (const target of HOSTED_MANAGED_PAIR_TARGETS) {
    const selected = loaded.targets.get(target.id);
    add(`${root}/${target.coreAlias}`, selected.core.artifact.body, "application/octet-stream");
    add(
      `${root}/${target.coreAlias}.gz`,
      zlib.gzipSync(selected.core.artifact.body, { level: 9, mtime: 0 }),
      "application/gzip",
    );
    add(`${root}/${selected.core.identity.object_key}`, selected.core.artifact.body, "application/octet-stream");
    add(
      `${root}/${selected.companion.identity.object_key}`,
      selected.companion.artifact.body,
      "application/octet-stream",
    );
    add(`${root}/${selected.manifestRecord.name}`, selected.manifest.body, "application/json");
  }
  if (runtimeHandoff != null) {
    for (const expected of HOSTED_RUNTIME_TRANSPORTS) {
      const selected = runtimeHandoff.artifacts.get(expected.metadata);
      add(
        `${root}/${selected.name}`,
        selected.artifact.body,
        selected.name.endsWith(".zip") ? "application/zip" : "application/gzip",
      );
    }
  }
  return [...byKey.values()];
}

function immutableMetadataObjects(loaded, metadata, signature) {
  return [
    {
      body: metadata,
      contentType: "text/plain; charset=utf-8",
      key: `releases/stable/${loaded.version}/ctx-release-metadata.env`,
    },
    {
      body: signature,
      contentType: "text/plain; charset=utf-8",
      key: `releases/stable/${loaded.version}/ctx-release-metadata.env.sig`,
    },
  ];
}

export function pointerBytes(loaded, metadata, signature) {
  return Buffer.from(`${JSON.stringify({
    channel: "stable",
    contract: "ctx-cli-release-pointer",
    metadata_object: `releases/stable/${loaded.version}/ctx-release-metadata.env`,
    metadata_sha256: sha256(metadata),
    schema_version: 1,
    signature_object: `releases/stable/${loaded.version}/ctx-release-metadata.env.sig`,
    signature_sha256: sha256(signature),
    version: loaded.version,
  })}\n`, "utf8");
}

function pointerVersion(body) {
  let value;
  try { value = JSON.parse(body.toString("utf8")); } catch { fail("hosted pointer is invalid"); }
  const keys = ["channel", "contract", "metadata_object", "metadata_sha256", "schema_version",
    "signature_object", "signature_sha256", "version"];
  if (value == null || typeof value !== "object" || Array.isArray(value)
      || Object.keys(value).sort().join("\0") !== keys.sort().join("\0")
      || value.contract !== "ctx-cli-release-pointer" || value.schema_version !== 1
      || value.channel !== "stable"
      || !/^[0-9a-f]{64}$/u.test(value.metadata_sha256 ?? "")
      || !/^[0-9a-f]{64}$/u.test(value.signature_sha256 ?? "")
      || value.metadata_object !== `releases/stable/${value.version}/ctx-release-metadata.env`
      || value.signature_object !== `${value.metadata_object}.sig`) {
    fail("hosted pointer is invalid");
  }
  compareStableVersions(value.version, value.version);
  return value.version;
}

export async function promoteCurrentPointer(request, loaded, body) {
  assertCurrentReleaseVersion(loaded.version);
  if (pointerVersion(body) !== loaded.version) fail("current pointer differs from the selected release");
  await assertFrozenBridgePromotion(loaded.version);
  const key = "releases/stable/current-v2.json";
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const current = await getR2Object(request, STABLE_BUCKET, key, 16 * 1024);
    if (current == null) fail("current stable feed has not been initialized by retained B source");
    if (current.body.equals(body)) return "existing-identical";
    if (compareStableVersions(pointerVersion(current.body), loaded.version) >= 0) {
      fail("stable pointer cannot be replaced by this release");
    }
    const currentEtag = strongConditionalEtag(current.etag);
    const headers = {
      "content-length": String(body.length),
      "content-type": "application/json; charset=utf-8",
      "x-amz-meta-sha256": sha256(body),
      "if-match": currentEtag,
    };
    const response = await request("PUT", STABLE_BUCKET, key, body, headers);
    await response.arrayBuffer();
    if (response.status === 412) continue;
    if (![200, 201, 204].includes(response.status)) {
      fail(`stable pointer PUT failed with status ${response.status}`);
    }
    const stored = await getR2Object(request, STABLE_BUCKET, key, body.length + 1);
    if (stored == null || !stored.body.equals(body)) fail("stable pointer readback failed");
    return "promoted";
  }
  fail("stable pointer changed concurrently too many times");
}

export function strongConditionalEtag(value) {
  if (typeof value !== "string") fail("stable pointer ETag is invalid");
  const match = /^(?:W\/)?("[^"\r\n]+")$/u.exec(value);
  if (match == null) fail("stable pointer ETag is invalid");
  return match[1];
}

export function compareStableVersions(left, right) {
  const parse = (value) => {
    const match = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u.exec(value);
    if (match == null) fail("stable pointer version is not canonical SemVer");
    return match.slice(1).map(BigInt);
  };
  const leftParts = parse(left);
  const rightParts = parse(right);
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] < rightParts[index]) return -1;
    if (leftParts[index] > rightParts[index]) return 1;
  }
  return 0;
}

export function prepareMetadata(loaded, runtimeHandoff, semanticExtension, {
  publishedAt, handoffDir, publicRepo, environment = process.env,
}) {
  const metadata = Buffer.concat([
    renderHostedManagedPairMetadata(loaded, publishedAt, runtimeHandoff),
    semanticExtension,
  ]);
  candidateManifestContract.verifyCandidateManifestHandoff({
    values: candidateManifestContract.parseEnvMetadata(metadata, "hosted prepare"),
    label: "hosted prepare", handoffDir, publicRepo, environment,
  });
  return metadata;
}

export async function run(argv, environment = process.env, fetchImplementation = globalThis.fetch) {
  const { args, command } = parseArgs(argv);
  const loaded = loadHostedManagedPairPublication(args.get("--publication"));
  assertCurrentReleaseVersion(loaded.version);
  const runtimeHandoff = loadRuntimeTransportHandoff(args.get("--runtime-handoff"), loaded);
  verifyRuntimeTransportHandoff(args.get("--public-ctx-repo"), runtimeHandoff);
  const semanticExtension = renderHostedSemanticMetadataExtension(
    args.get("--semantic-artifact-dir"),
    args.get("--candidate-manifest-handoff"),
    args.get("--candidate-handoff-sha256"),
  );
  if (command === "prepare") {
    const metadata = prepareMetadata(loaded, runtimeHandoff, semanticExtension, {
      publishedAt: args.get("--published-at"),
      handoffDir: args.get("--candidate-manifest-handoff"),
      publicRepo: args.get("--public-ctx-repo"),
      environment,
    });
    writeExclusive(args.get("--metadata-out"), metadata, 0o600);
    return { metadata_sha256: sha256(metadata), status: "prepared" };
  }
  if (environment.CTX_RELEASE_R2_BUCKET !== STABLE_BUCKET) {
    fail("stable Core release bucket differs from fixed authority");
  }
  const metadata = readStableFile(args.get("--metadata"), "release metadata", 128 * 1024);
  validateHostedManagedPairMetadata(metadata, loaded, runtimeHandoff, semanticExtension);
  const signature = strictSignature(args.get("--signature"), metadata);
  const semanticAssets = loadHostedSemanticAssetCatalog(metadata);
  await assertFrozenBridgePromotion(loaded.version);
  const request = createR2Request(R2_AUTHORITY, environment, fetchImplementation);
  const pointer = pointerBytes(loaded, metadata, signature);
  const results = [];
  const objectEvidence = [];
  const publishObject = async (object) => {
    const state = await putImmutableR2Object(request, STABLE_BUCKET, object);
    results.push({ key: object.key, state });
    objectEvidence.push({
      key: object.key,
      sha256: sha256(object.body),
      size_bytes: object.body.length,
    });
  };
  for (const object of immutableArtifactObjects(loaded, runtimeHandoff)) {
    await publishObject(object);
  }
  for (const semantic of semanticAssets) {
    const snapshot = snapshotHostedSemanticAsset(
      args.get("--semantic-artifact-dir"),
      semantic,
    );
    await publishObject({
      body: snapshot.body,
      contentType: semantic.contentType,
      key: `artifacts/stable/${loaded.version}/${semantic.name}`,
    });
  }
  for (const object of immutableMetadataObjects(loaded, metadata, signature)) {
    await publishObject(object);
  }
  const pointerState = await promoteCurrentPointer(request, loaded, pointer);
  const frozenBridge = await assertFrozenBridgePromotion(loaded.version);
  const evidence = {
    channel: "stable",
    contract: "ctx-hosted-managed-pair-publication",
    metadata_sha256: sha256(metadata),
    objects: objectEvidence,
    pointer_sha256: sha256(pointer),
    pointer_state: pointerState,
    current_pointer_key: "releases/stable/current-v2.json",
    frozen_bridge: frozenBridge,
    private_commit: loaded.privateCommit,
    public_commit: loaded.publicCommit,
    release_name: loaded.publication.release_name,
    runtime_handoff_sha256: runtimeHandoff.snapshot.sha256,
    schema_version: 1,
    supplementary_assets_included: true,
    supplementary_profile: "complete",
  };
  writeExclusive(args.get("--evidence-out"), Buffer.from(`${JSON.stringify(evidence, null, 2)}\n`), 0o600);
  return { evidence, results, status: "published" };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  run(process.argv.slice(2)).then(
    (result) => process.stdout.write(`${JSON.stringify(result)}\n`),
    (error) => {
      process.stderr.write(`hosted stable publication failed: ${error.message}\n`);
      process.exitCode = 1;
    },
  );
}
