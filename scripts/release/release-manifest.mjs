#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import {
  COMPONENT_KINDS,
  TARGET_IDS,
  canonicalJsonBytes,
  contractError,
  createSafeOutputTree,
  loadCandidate,
  loadInputAuthority,
  loadTargetMatrix,
  materializeReleaseInputs,
  sha256,
  signEnvelope,
  targetManifest,
  validateOutputPath,
  validateReleaseSet,
  validateTargetManifest,
  verifyEnvelope,
} from "./managed-pair-release-contract.mjs";
import {
  loadReleaseAuthorities,
  releaseTrust,
} from "./release-authority.mjs";
import {
  SECRET_STORE_AUTH_ENVIRONMENT_VARIABLES,
  SIGNING_KEY_ENVIRONMENT_VARIABLES,
} from "./release-signing-boundary.mjs";

import { loadUnifiedReleaseInputs } from "./unified-release-inputs.mjs";

import { assertCurrentReleaseVersion } from "./frozen-cli-bridge.cjs";

const MAX_PRIVATE_KEY_BYTES = 64 * 1024;
const SCRIPT_PATH = fileURLToPath(import.meta.url);

function parseArgs(argv) {
  const preflightOnly = argv.includes("--preflight-only");
  if (argv.filter((value) => value === "--preflight-only").length > 1) contractError("duplicate preflight option");
  argv = argv.filter((value) => value !== "--preflight-only");
  const allowed = new Set([
    "--authority-registry",
    "--candidate",
    "--output-dir",
    "--factory-dir",
    "--candidate-manifest-handoff",
    "--candidate-handoff-sha256",
    "--target-matrix",
  ]);
  if (argv.length % 2 !== 0) contractError("arguments must be --name value pairs");
  const result = new Map([["preflightOnly", preflightOnly]]);
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!allowed.has(name) || value == null || result.has(name)) {
      contractError("arguments contain an invalid or duplicate option");
    }
    result.set(name, value);
  }
  for (const required of [
    "--candidate", "--output-dir", "--factory-dir", "--target-matrix",
    "--candidate-manifest-handoff", "--candidate-handoff-sha256",
  ]) {
    if (!result.has(required)) contractError(`${required} is required`);
  }
  return result;
}

function assertSignerEnvironment(environment) {
  const forbidden = [
    ...SIGNING_KEY_ENVIRONMENT_VARIABLES,
    ...SECRET_STORE_AUTH_ENVIRONMENT_VARIABLES,
    ...Object.keys(environment).filter((name) =>
      name.startsWith("CTX_PRO_R2_") || name.startsWith("R2_")),
  ];
  const inherited = [...new Set(forbidden)].filter((name) =>
    Object.hasOwn(environment, name));
  if (inherited.length !== 0) {
    contractError(`managed-pair signer inherited forbidden authority: ${inherited.join(", ")}`);
  }
}

function writeSignedDocument(outputTree, rootSegments, signed) {
  const manifestPath = outputTree.writeFile([...rootSegments, "manifest.json"], signed.payloadBytes);
  const signaturePath = outputTree.writeFile(
    [...rootSegments, "manifest.sig"],
    Buffer.from(`${signed.signature.toString("base64")}\n`, "ascii"),
  );
  const envelopePath = outputTree.writeFile([...rootSegments, "envelope.json"], signed.envelopeBytes);
  return Object.freeze({ envelopePath, manifestPath, signaturePath });
}

function authorityFor(args, channel) {
  const registryPath = args.get("--authority-registry");
  if (registryPath == null) return releaseTrust(channel);
  const resolved = path.resolve(registryPath);
  const authorities = loadReleaseAuthorities({
    registryPath: resolved,
  });
  return releaseTrust(channel, authorities);
}

export function generateManagedPairRelease({
  authority,
  candidatePath,
  outputDir,
  privateKey,
  factoryDir,
  handoffDir,
  handoffDigest,
  targetMatrixPath,
  testHooks = {},
}) {
  const prepared = prepareManagedPairRelease({
    authority,
    candidatePath,
    outputDir,
    factoryDir,
    handoffDir,
    handoffDigest,
    targetMatrixPath,
    testHooks,
  });
  return finalizeManagedPairRelease(prepared, privateKey, testHooks);
}

export function prepareManagedPairRelease({
  authority,
  candidatePath,
  outputDir,
  factoryDir,
  handoffDir,
  handoffDigest,
  targetMatrixPath,
  testHooks = {},
}) {
  const inputAuthority = loadInputAuthority();
  const candidate = loadCandidate(path.resolve(candidatePath), inputAuthority);
  if (candidate.channel !== authority.channel
      || candidate.target_matrix_sha256 == null) {
    contractError("candidate channel differs from the selected release authority");
  }
  if (candidate.channel === "stable") assertCurrentReleaseVersion(candidate.release_name.slice(1));
  const matrix = loadTargetMatrix(
    path.resolve(targetMatrixPath),
    candidate.target_matrix_sha256,
    inputAuthority,
  );
  const inputs = loadUnifiedReleaseInputs({ factoryDir, handoffDir, handoffDigest, candidate, matrix });
  const outputPreflight = validateOutputPath(outputDir);
  return Object.freeze({ authority, candidate, inputAuthority, inputs, matrix, outputPreflight });
}

export function finalizeManagedPairRelease(prepared, privateKey, testHooks = {}) {
  const { authority, candidate, matrix, outputPreflight } = prepared;
  if (candidate.channel === "stable") assertCurrentReleaseVersion(candidate.release_name.slice(1));
  const outputTree = createSafeOutputTree(outputPreflight, testHooks);
  try {
    const inputs = materializeReleaseInputs(prepared.inputs, outputTree, testHooks);

    const signedTargets = [];
    const targetReferences = [];
    const componentObjects = [];
    for (const targetInput of inputs.targets) {
      const payload = targetManifest(candidate, matrix, inputs, targetInput, authority);
      validateTargetManifest(payload, matrix, authority);
      const signed = signEnvelope(payload, privateKey, authority);
      verifyEnvelope(signed.envelopeBytes, matrix, authority);
      const manifestName = `ctx-managed-pair-${targetInput.targetId}.json`;
      const envelopeDigest = sha256(signed.envelopeBytes);
      const reference = {
        target_id: targetInput.targetId,
        manifest_name: manifestName,
        manifest_object_key: `sha256/${envelopeDigest}/${manifestName}`,
        manifest_sha256: envelopeDigest,
        manifest_size_bytes: signed.envelopeBytes.length,
      };
      targetReferences.push(reference);
      signedTargets.push({ reference, signed, targetInput });
      for (const component of COMPONENT_KINDS) {
        const value = targetInput.components[component];
        componentObjects.push({
          target_id: targetInput.targetId,
          component,
          name: value.artifactName,
          object_key: value.objectKey,
          sha256: value.digest,
          size_bytes: value.sizeBytes,
          path: value.artifactPath,
        });
      }
    }
    if (signedTargets.length !== TARGET_IDS.length || componentObjects.length !== 10) {
      contractError("release generation did not produce five targets and ten components");
    }

    const releaseSet = {
      contract: "ctx-managed-pair-release-set",
      schema_version: 1,
      channel: candidate.channel,
      release_authority_key_id: authority.releaseKeyId,
      release_name: candidate.release_name,
      target_matrix_sha256: matrix.digest,
      rollback_generation: candidate.rollback_generation,
      accepted_pair: { ...inputs.acceptedPair },
      snapshot: { ...inputs.snapshot },
      compatibility: { ...inputs.compatibility },
      target_manifests: targetReferences,
    };
    validateReleaseSet(releaseSet, matrix, authority);
    const signedReleaseSet = signEnvelope(releaseSet, privateKey, authority);
    verifyEnvelope(signedReleaseSet.envelopeBytes, matrix, authority);
    const releaseSetName = "ctx-managed-pair-release-set.json";
    const releaseSetDigest = sha256(signedReleaseSet.envelopeBytes);

    const targetManifestObjects = [];
    for (const value of signedTargets) {
      const files = writeSignedDocument(
        outputTree,
        ["targets", value.targetInput.targetId],
        value.signed,
      );
      targetManifestObjects.push({
        target_id: value.targetInput.targetId,
        name: value.reference.manifest_name,
        object_key: value.reference.manifest_object_key,
        sha256: value.reference.manifest_sha256,
        size_bytes: value.reference.manifest_size_bytes,
        path: files.envelopePath,
      });
    }
    const releaseSetFiles = writeSignedDocument(
      outputTree,
      ["release-set"],
      signedReleaseSet,
    );
    const publication = {
      contract: "ctx-managed-pair-publication",
      schema_version: 1,
      channel: candidate.channel,
      release_name: candidate.release_name,
      rollback_generation: candidate.rollback_generation,
      target_matrix_sha256: matrix.digest,
      component_objects: componentObjects,
      target_manifest_objects: targetManifestObjects,
      release_set_object: {
        name: releaseSetName,
        object_key: `sha256/${releaseSetDigest}/${releaseSetName}`,
        sha256: releaseSetDigest,
        size_bytes: signedReleaseSet.envelopeBytes.length,
        path: releaseSetFiles.envelopePath,
      },
      pointer_object: `channels/${candidate.channel}/managed-pair.json`,
    };
    const publicationPath = outputTree.writeFile(
      ["publication.json"],
      canonicalJsonBytes(publication),
    );
    outputTree.assertReachable();
    return Object.freeze({
      publication,
      publicationPath,
      releaseSet,
      releaseSetEnvelopePath: releaseSetFiles.envelopePath,
    });
  } finally {
    outputTree.close();
  }
}

async function readPrivateKey() {
  const chunks = [];
  let total = 0;
  for await (const input of process.stdin) {
    const chunk = Buffer.isBuffer(input) ? Buffer.from(input) : Buffer.from(input);
    total += chunk.length;
    if (total > MAX_PRIVATE_KEY_BYTES) {
      chunk.fill(0);
      for (const stored of chunks) stored.fill(0);
      contractError("managed-pair signing key exceeds 64 KiB");
    }
    chunks.push(chunk);
  }
  const privateKey = Buffer.concat(chunks);
  for (const chunk of chunks) chunk.fill(0);
  if (privateKey.length === 0) contractError("managed-pair signing key is missing");
  return privateKey;
}

async function main() {
  assertSignerEnvironment(process.env);
  const args = parseArgs(process.argv.slice(2));
  const inputAuthority = loadInputAuthority();
  const candidate = loadCandidate(path.resolve(args.get("--candidate")), inputAuthority);
  const authority = authorityFor(args, candidate.channel);
  const prepared = prepareManagedPairRelease({
    authority,
    candidatePath: args.get("--candidate"),
    outputDir: args.get("--output-dir"),
    factoryDir: args.get("--factory-dir"),
    handoffDir: args.get("--candidate-manifest-handoff"),
    handoffDigest: args.get("--candidate-handoff-sha256"),
    targetMatrixPath: args.get("--target-matrix"),
  });
  if (args.get("preflightOnly")) {
    process.stdout.write(`${JSON.stringify({ status: "prepared", source_commit: prepared.inputs.acceptedPair.public_source_commit,
      validation_policy: prepared.inputs.validationPolicy, targets: 5, components: 10 })}\n`);
    return;
  }
  const privateKey = await readPrivateKey();
  try {
    const result = finalizeManagedPairRelease(prepared, privateKey);
    process.stdout.write(`${JSON.stringify({
      channel: result.publication.channel,
      release_name: result.publication.release_name,
      rollback_generation: result.publication.rollback_generation,
      publication: result.publicationPath,
      validation_policy: prepared.inputs.validationPolicy,
      targets: 5,
      components: 10,
    })}\n`);
  } finally {
    privateKey.fill(0);
  }
}

if (process.argv[1] != null
    && fs.realpathSync(process.argv[1]) === fs.realpathSync(SCRIPT_PATH)) {
  try {
    await main();
  } catch (error) {
    process.stderr.write(`error: ${error instanceof Error ? error.message : "managed-pair release generation failed"}\n`);
    process.exitCode = 1;
  }
}
