import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { TextDecoder } from "node:util";

import {
  TARGET_IDS, COMPONENT_KINDS, MAX_COMPONENT_BYTES, MAX_AGGREGATE_COMPONENT_BYTES,
  MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, MIN_RSA_MODULUS_BITS,
  MAX_RSA_MODULUS_BITS, MAX_HISTORICAL_TARGET_MATRICES, MAX_ROLLBACK_GENERATION,
  CORE_CAPABILITY_PROTOCOL_FINGERPRINT, SHA256, COMMIT, NAME, RUST_TARGET,
  RELATIVE_FILE, FIXED_CONTRACT_FILE, contractError, exactKeys,
  canonicalJson, canonicalJsonBytes, sha256, parseJsonStrict,
  readStableRegularFile, readJsonFile, requireString, requireSize, strictBase64,
  requireSnapshot, requireCompatibility, fixedContractEntry, releaseVersionContract,
  validReleaseName, loadInputAuthority, loadTargetMatrix, trustedTargetMatrix,
  loadCandidate, READ_BLOCK_BYTES, capturePath, verifyCapturedPath,
  openStableRegularFile, finishStableRegularFile, sameIdentity, statIdentity,
} from "./managed-pair-release-io.mjs";

export {
  TARGET_IDS, COMPONENT_KINDS, MAX_COMPONENT_BYTES, MAX_AGGREGATE_COMPONENT_BYTES,
  MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, MIN_RSA_MODULUS_BITS,
  MAX_RSA_MODULUS_BITS, MAX_HISTORICAL_TARGET_MATRICES, MAX_ROLLBACK_GENERATION,
  CORE_CAPABILITY_PROTOCOL_FINGERPRINT, contractError, exactKeys, canonicalJson,
  canonicalJsonBytes, sha256, parseJsonStrict, readStableRegularFile, readJsonFile,
  loadInputAuthority, loadTargetMatrix, trustedTargetMatrix, loadCandidate,
};

export const NATIVE_RECEIPT_VALIDATION_POLICY = "native-receipts-required-v1";
export const FACTORY_ONLY_VALIDATION_POLICY = "factory-only-human-override-v1";

function componentFields(target, kind) {
  if (kind === "core") {
    return {
      artifactName: target.public_artifact,
      rustTarget: target.public_rust_target,
      slot: target.managed_pair_core_slot,
    };
  }
  return {
    artifactName: target.helper_artifact,
    rustTarget: target.public_rust_target,
    slot: target.managed_pair_companion_slot,
  };
}

export function hashStableFile(file, label, maximumBytes, hook = undefined) {
  const opened = openStableRegularFile(file, label, maximumBytes);
  const digest = crypto.createHash("sha256");
  const buffer = Buffer.allocUnsafe(READ_BLOCK_BYTES);
  let total = 0;
  try {
    for (;;) {
      const count = fs.readSync(opened.descriptor, buffer, 0, buffer.length, null);
      if (count === 0) break;
      total += count;
      if (total > maximumBytes) contractError(`${label} exceeds its size bound`);
      digest.update(buffer.subarray(0, count));
    }
    finishStableRegularFile(opened, label, hook);
    return Object.freeze({
      digest: digest.digest("hex"),
      identity: opened.captured,
      path: opened.captured.absolute,
      sizeBytes: total,
    });
  } finally {
    buffer.fill(0);
    fs.closeSync(opened.descriptor);
  }
}

function requireNonzeroCommit(value, label) {
  requireString(value, COMMIT, label);
  if (value === "0".repeat(40)) contractError(`${label} is zero`);
  return value;
}

export function validateOutputPath(outputDir) {
  assertDescriptorOutputSupport();
  const output = path.resolve(outputDir);
  if (output === path.parse(output).root || pathEntryExists(output, "output directory")) {
    contractError("output directory must not already exist and must have a real parent");
  }
  safeOutputSegment(path.basename(output), "output directory name");
  const parent = capturePath(path.dirname(output), "output parent");
  if (!parent.identities.at(-1)?.isDirectory) contractError("output parent must be a directory");
  const parentStat = fs.lstatSync(parent.absolute, { bigint: true });
  if ((typeof process.getuid === "function" && parentStat.uid !== BigInt(process.getuid()))
      || (parentStat.mode & 0o022n) !== 0n) {
    contractError("output parent must be owner-private before signing");
  }
  return Object.freeze({ output, parent });
}

export function assertOutputPathUnchanged(preflight) {
  verifyCapturedPath(preflight.parent, "output parent");
  if (pathEntryExists(preflight.output, "output directory")) {
    contractError("output directory appeared after preflight");
  }
}

function pathEntryExists(file, label) {
  try {
    fs.lstatSync(file);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    contractError(`${label} cannot be checked without following links`);
  }
}

function safeOutputSegment(value, label) {
  if (typeof value !== "string" || !NAME.test(value) || value === "." || value === "..") {
    contractError(`${label} is not a safe path segment`);
  }
  return value;
}

function descriptorPath(descriptor, segment = undefined) {
  const root = `/proc/self/fd/${descriptor}`;
  return segment === undefined ? root : `${root}/${safeOutputSegment(segment, "output path")}`;
}

function closeDescriptors(descriptors) {
  for (const descriptor of [...descriptors].reverse()) {
    try {
      fs.closeSync(descriptor);
    } catch {
      // Preserve the original contract failure while closing best-effort handles.
    }
  }
}

function assertDescriptorOutputSupport() {
  const directoryFlag = fs.constants.O_DIRECTORY;
  const noFollowFlag = fs.constants.O_NOFOLLOW;
  if (process.platform !== "linux" || directoryFlag == null || noFollowFlag == null
      || !fs.existsSync("/proc/self/fd")) {
    contractError("safe descriptor-relative output creation is unavailable on this platform");
  }
  return { directoryFlag, noFollowFlag };
}

export function createSafeOutputTree(preflight, testHooks = {}) {
  const { directoryFlag, noFollowFlag } = assertDescriptorOutputSupport();
  verifyCapturedPath(preflight.parent, "output parent");
  if (pathEntryExists(preflight.output, "output directory")) {
    contractError("output directory appeared after preflight");
  }
  const parentLeaf = preflight.parent.identities.at(-1);
  let parentDescriptor;
  try {
    parentDescriptor = fs.openSync(
      preflight.parent.absolute,
      fs.constants.O_RDONLY | directoryFlag | noFollowFlag,
    );
  } catch {
    contractError("output parent cannot be opened without following links");
  }
  const descriptors = [parentDescriptor];
  try {
    const parentStat = fs.fstatSync(parentDescriptor, { bigint: true });
    let descriptorNamespaceStat;
    try {
      descriptorNamespaceStat = fs.statSync(descriptorPath(parentDescriptor), { bigint: true });
    } catch {
      contractError("descriptor-relative output namespace is unavailable");
    }
    if (!parentStat.isDirectory()
        || !descriptorNamespaceStat.isDirectory()
        || !sameIdentity(
          statIdentity(parentStat, false),
          statIdentity(descriptorNamespaceStat, false),
        )
        || !sameIdentity(parentLeaf.identity, statIdentity(parentStat, true))
        || (typeof process.getuid === "function" && parentStat.uid !== BigInt(process.getuid()))
        || (parentStat.mode & 0o022n) !== 0n) {
      contractError("output parent is not a stable owner-private directory");
    }
    if (testHooks.afterOutputParentOpen != null) testHooks.afterOutputParentOpen();
    verifyCapturedPath(preflight.parent, "output parent");
    const outputName = safeOutputSegment(path.basename(preflight.output), "output directory name");
    const outputAt = descriptorPath(parentDescriptor, outputName);
    try {
      fs.mkdirSync(outputAt, { mode: 0o700 });
    } catch {
      contractError("output directory could not be created exclusively");
    }
    const created = fs.lstatSync(outputAt, { bigint: true });
    if (!created.isDirectory() || created.isSymbolicLink()) {
      contractError("output directory creation did not produce a real directory");
    }
    if (testHooks.afterOutputDirectoryCreate != null) {
      testHooks.afterOutputDirectoryCreate({ output: preflight.output });
    }
    let rootDescriptor;
    try {
      rootDescriptor = fs.openSync(
        outputAt,
        fs.constants.O_RDONLY | directoryFlag | noFollowFlag,
      );
    } catch {
      contractError("output directory was substituted before it could be opened");
    }
    descriptors.push(rootDescriptor);
    const rootStat = fs.fstatSync(rootDescriptor, { bigint: true });
    const rootIdentity = statIdentity(rootStat, false);
    if (!rootStat.isDirectory()
        || !sameIdentity(statIdentity(created, false), rootIdentity)
        || (rootStat.mode & 0o077n) !== 0n) {
      contractError("output directory identity or mode changed during creation");
    }
    verifyCapturedPath(preflight.parent, "output parent", false);
    const directories = new Map([["", Object.freeze({
      descriptor: rootDescriptor,
      identity: rootIdentity,
    })]]);

    const verifyDirectory = (record, label) => {
      const stat = fs.fstatSync(record.descriptor, { bigint: true });
      if (!stat.isDirectory()
          || !sameIdentity(record.identity, statIdentity(stat, false))) {
        contractError(`${label} directory identity changed during output generation`);
      }
    };
    const ensureDirectory = (segments) => {
      let record = directories.get("");
      let key = "";
      for (const raw of segments) {
        const segment = safeOutputSegment(raw, "output directory segment");
        verifyDirectory(record, key || "output root");
        key = key === "" ? segment : `${key}/${segment}`;
        const existing = directories.get(key);
        if (existing != null) {
          record = existing;
          continue;
        }
        const childAt = descriptorPath(record.descriptor, segment);
        try {
          fs.mkdirSync(childAt, { mode: 0o700 });
        } catch {
          contractError(`output directory ${key} could not be created exclusively`);
        }
        const childCreated = fs.lstatSync(childAt, { bigint: true });
        let childDescriptor;
        try {
          childDescriptor = fs.openSync(
            childAt,
            fs.constants.O_RDONLY | directoryFlag | noFollowFlag,
          );
        } catch {
          contractError(`output directory ${key} was substituted during creation`);
        }
        descriptors.push(childDescriptor);
        const childStat = fs.fstatSync(childDescriptor, { bigint: true });
        if (!childCreated.isDirectory() || childCreated.isSymbolicLink()
            || !childStat.isDirectory()
            || !sameIdentity(statIdentity(childCreated, false), statIdentity(childStat, false))
            || (childStat.mode & 0o077n) !== 0n) {
          contractError(`output directory ${key} is not a stable private directory`);
        }
        record = Object.freeze({
          descriptor: childDescriptor,
          identity: statIdentity(childStat, false),
        });
        directories.set(key, record);
      }
      verifyDirectory(record, key || "output root");
      return record;
    };
    const assertReachable = () => {
      for (const [key, record] of directories) verifyDirectory(record, key || "output root");
      verifyCapturedPath(preflight.parent, "output parent", false);
      const reachable = fs.lstatSync(preflight.output, { bigint: true });
      if (reachable.isSymbolicLink() || !reachable.isDirectory()
          || !sameIdentity(rootIdentity, statIdentity(reachable, false))) {
        contractError("output directory was substituted during generation");
      }
    };
    const withFile = (segments, mode, writer) => {
      if (!Array.isArray(segments) || segments.length === 0) {
        contractError("output file path is empty");
      }
      const normalized = segments.map((segment) =>
        safeOutputSegment(segment, "output file segment"));
      const parent = ensureDirectory(normalized.slice(0, -1));
      const name = normalized.at(-1);
      let descriptor;
      try {
        descriptor = fs.openSync(
          descriptorPath(parent.descriptor, name),
          fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | noFollowFlag,
          mode,
        );
      } catch {
        contractError(`output file ${normalized.join("/")} could not be created exclusively`);
      }
      try {
        const before = fs.fstatSync(descriptor, { bigint: true });
        if (!before.isFile() || before.nlink !== 1n) {
          contractError(`output file ${normalized.join("/")} is not a private regular file`);
        }
        writer(descriptor);
        fs.fsyncSync(descriptor);
        const after = fs.fstatSync(descriptor, { bigint: true });
        if (!sameIdentity(statIdentity(before, false), statIdentity(after, false))) {
          contractError(`output file ${normalized.join("/")} identity changed while writing`);
        }
      } finally {
        fs.closeSync(descriptor);
      }
      assertReachable();
      return path.join(preflight.output, ...normalized);
    };
    return Object.freeze({
      assertReachable,
      close() {
        closeDescriptors(descriptors);
        descriptors.length = 0;
      },
      output: preflight.output,
      withFile,
      writeFile(segments, bytes, mode = 0o600) {
        return withFile(segments, mode, (descriptor) => fs.writeFileSync(descriptor, bytes));
      },
    });
  } catch (error) {
    closeDescriptors(descriptors);
    throw error;
  }
}

function copyVerifiedArtifact(component, outputTree, segments, hook = undefined) {
  const opened = openStableRegularFile(component.artifactPath, "prepared component artifact", MAX_COMPONENT_BYTES);
  if (opened.captured.identities.length !== component.artifactSourceIdentity.identities.length
      || !opened.captured.identities.every((part, index) =>
        sameIdentity(part.identity, component.artifactSourceIdentity.identities[index]?.identity ?? {}))) {
    fs.closeSync(opened.descriptor);
    contractError("component artifact identity changed after signing preflight");
  }
  const digest = crypto.createHash("sha256");
  const buffer = Buffer.allocUnsafe(READ_BLOCK_BYTES);
  let total = 0;
  try {
    const destination = outputTree.withFile(segments, 0o600, (output) => {
      for (;;) {
        const count = fs.readSync(opened.descriptor, buffer, 0, buffer.length, null);
        if (count === 0) break;
        total += count;
        if (total > MAX_COMPONENT_BYTES) contractError("component artifact exceeds its copy bound");
        digest.update(buffer.subarray(0, count));
        let written = 0;
        while (written < count) {
          written += fs.writeSync(output, buffer, written, count - written);
        }
      }
    });
    finishStableRegularFile(opened, "prepared component artifact", hook);
    if (total !== component.sizeBytes || digest.digest("hex") !== component.digest) {
      contractError("component artifact changed after signing preflight");
    }
    return destination;
  } finally {
    buffer.fill(0);
    fs.closeSync(opened.descriptor);
  }
}

export function snapshotVerifiedArtifact({
  source,
  destination,
  digest: expectedDigest,
  sizeBytes: expectedSize,
  label,
  hook = undefined,
}) {
  const opened = openStableRegularFile(source, label, MAX_COMPONENT_BYTES);
  fs.mkdirSync(path.dirname(destination), { mode: 0o700, recursive: true });
  const output = fs.openSync(destination, "wx", 0o600);
  const digest = crypto.createHash("sha256");
  const buffer = Buffer.allocUnsafe(READ_BLOCK_BYTES);
  let total = 0;
  try {
    for (;;) {
      const count = fs.readSync(opened.descriptor, buffer, 0, buffer.length, null);
      if (count === 0) break;
      total += count;
      if (total > MAX_COMPONENT_BYTES) contractError(`${label} exceeds its copy bound`);
      digest.update(buffer.subarray(0, count));
      let written = 0;
      while (written < count) written += fs.writeSync(output, buffer, written, count - written);
    }
    finishStableRegularFile(opened, label, hook);
    fs.fsyncSync(output);
  } finally {
    buffer.fill(0);
    fs.closeSync(output);
    fs.closeSync(opened.descriptor);
  }
  if (total !== expectedSize || digest.digest("hex") !== expectedDigest) {
    contractError(`${label} bytes differ from the publication record`);
  }
  const retained = openStableRegularFile(destination, `${label} retained snapshot`, MAX_COMPONENT_BYTES);
  return Object.freeze({
    descriptor: retained.descriptor,
    path: destination,
    sizeBytes: total,
  });
}

export function materializeReleaseInputs(inputs, outputTree, testHooks = {}) {
  const targets = [];
  for (const input of inputs.targets) {
    const components = {};
    for (const kind of COMPONENT_KINDS) {
      const component = input.components[kind];
      const destinationSegments = ["artifacts", input.targetId, component.artifactName];
      const hook = testHooks.afterArtifactCopy == null
        ? undefined
        : () => testHooks.afterArtifactCopy({ kind, path: component.artifactPath, targetId: input.targetId });
      const destination = copyVerifiedArtifact(component, outputTree, destinationSegments, hook);
      components[kind] = Object.freeze({ ...component, artifactPath: destination });
    }
    targets.push(Object.freeze({ ...input, components: Object.freeze(components) }));
  }
  return Object.freeze({ ...inputs, targets: Object.freeze(targets) });
}

export function targetManifest(candidate, matrix, inputs, targetInput, trust) {
  const { target, targetId, components } = targetInput;
  return {
    contract: "ctx-managed-pair-manifest",
    schema_version: 1,
    channel: candidate.channel,
    release_authority_key_id: trust.releaseKeyId,
    release_name: candidate.release_name,
    target: {
      id: targetId,
      os: target.os,
      arch: target.arch,
      core_rust_target: target.public_rust_target,
      companion_rust_target: target.public_rust_target,
    },
    install_geometry: {
      install_root: "<install-root>",
      managed_bin_dir: "<install-root>/bin",
      core_slot: `<install-root>/${target.managed_pair_core_slot}`,
      companion_slot: `<install-root>/${target.managed_pair_companion_slot}`,
    },
    target_matrix_sha256: matrix.digest,
    rollback_generation: candidate.rollback_generation,
    snapshot: { ...inputs.snapshot },
    compatibility: { ...inputs.compatibility },
    components: Object.fromEntries(COMPONENT_KINDS.map((kind) => {
      const component = components[kind];
      return [kind, {
        artifact_name: component.artifactName,
        object_key: component.objectKey,
        sha256: component.digest,
        size_bytes: component.sizeBytes,
        install_slot: `<install-root>/${component.slot}`,
        build_identity: { ...component.buildIdentity },
      }];
    })),
  };
}

function validateBuildIdentity(identity, kind, rustTarget) {
  exactKeys(identity, ["component", "rust_target", "source_revision", "build_fingerprint"], `${kind} build identity`);
  if (identity.component !== kind || identity.rust_target !== rustTarget
      || !RUST_TARGET.test(identity.rust_target)) {
    contractError(`${kind} build identity does not match the fixed target`);
  }
  requireString(identity.source_revision, COMMIT, `${kind} source revision`);
  requireString(identity.build_fingerprint, SHA256, `${kind} build fingerprint`);
}

export function validateTargetManifest(value, matrix, trust) {
  exactKeys(value, [
    "contract", "schema_version", "channel", "release_authority_key_id",
    "release_name", "target", "install_geometry", "target_matrix_sha256",
    "rollback_generation", "snapshot", "compatibility", "components",
  ], "managed-pair target manifest");
  if (value.contract !== "ctx-managed-pair-manifest" || value.schema_version !== 1
      || value.channel !== trust.channel
      || value.release_authority_key_id !== trust.releaseKeyId) {
    contractError("managed-pair target manifest has an invalid authority envelope");
  }
  requireString(value.release_name, NAME, "target-manifest release name");
  if (value.target_matrix_sha256 !== matrix.digest) contractError("target manifest binds a different matrix");
  requireSize(value.rollback_generation, MAX_ROLLBACK_GENERATION, "target-manifest rollback generation");
  requireSnapshot(value.snapshot, "target-manifest snapshot");
  requireCompatibility(value.compatibility, "target-manifest compatibility");
  exactKeys(value.target, ["id", "os", "arch", "core_rust_target", "companion_rust_target"], "target-manifest target");
  const target = matrix.targets.get(value.target.id);
  if (target == null || value.target.os !== target.os || value.target.arch !== target.arch
      || value.target.core_rust_target !== target.public_rust_target
      || value.target.companion_rust_target !== target.public_rust_target) {
    contractError("target manifest does not match the fixed target matrix");
  }
  const geometry = {
    install_root: "<install-root>",
    managed_bin_dir: "<install-root>/bin",
    core_slot: `<install-root>/${target.managed_pair_core_slot}`,
    companion_slot: `<install-root>/${target.managed_pair_companion_slot}`,
  };
  exactKeys(value.install_geometry, Object.keys(geometry), "target-manifest install geometry");
  if (canonicalJson(value.install_geometry) !== canonicalJson(geometry)) contractError("target manifest has invalid install geometry");
  exactKeys(value.components, COMPONENT_KINDS, "target-manifest components");
  for (const kind of COMPONENT_KINDS) {
    const component = value.components[kind];
    const fixed = componentFields(target, kind);
    exactKeys(component, ["artifact_name", "object_key", "sha256", "size_bytes", "install_slot", "build_identity"], `${kind} target-manifest component`);
    requireString(component.sha256, SHA256, `${kind} target-manifest digest`);
    if (component.artifact_name !== fixed.artifactName
        || component.object_key !== `sha256/${component.sha256}/${component.artifact_name}`
        || component.install_slot !== `<install-root>/${fixed.slot}`) {
      contractError(`${kind} target-manifest component differs from the fixed target`);
    }
    requireSize(component.size_bytes, MAX_COMPONENT_BYTES, `${kind} target-manifest size`);
    validateBuildIdentity(component.build_identity, kind, fixed.rustTarget);
  }
  return value;
}

export function validateReleaseSet(value, matrix, trust) {
  exactKeys(value, [
    "contract", "schema_version", "channel", "release_authority_key_id",
    "release_name", "target_matrix_sha256", "rollback_generation", "snapshot",
    "accepted_pair", "compatibility", "target_manifests",
  ], "managed-pair release set");
  if (value.contract !== "ctx-managed-pair-release-set" || value.schema_version !== 1
      || value.channel !== trust.channel || value.release_authority_key_id !== trust.releaseKeyId
      || value.target_matrix_sha256 !== matrix.digest) {
    contractError("managed-pair release set has an invalid authority envelope");
  }
  requireString(value.release_name, NAME, "release-set name");
  requireSize(value.rollback_generation, MAX_ROLLBACK_GENERATION, "release-set rollback generation");
  exactKeys(value.accepted_pair, [
    "acceptance_receipt_sha256", "candidate_digest", "protocol_fingerprint",
    "public_source_commit", "private_source_commit",
  ], "release-set accepted pair");
  for (const name of [
    "acceptance_receipt_sha256", "candidate_digest", "protocol_fingerprint",
  ]) requireString(value.accepted_pair[name], SHA256, `release-set accepted pair ${name}`);
  requireNonzeroCommit(value.accepted_pair.public_source_commit, "release-set accepted public source revision");
  requireNonzeroCommit(value.accepted_pair.private_source_commit, "release-set accepted private source revision");
  requireSnapshot(value.snapshot, "release-set snapshot");
  requireCompatibility(value.compatibility, "release-set compatibility");
  if (!Array.isArray(value.target_manifests) || value.target_manifests.length !== TARGET_IDS.length
      || value.target_manifests.map((entry) => entry?.target_id).join("\0") !== TARGET_IDS.join("\0")) {
    contractError("release set must contain the exact five-target ordered matrix");
  }
  for (const entry of value.target_manifests) {
    exactKeys(entry, ["target_id", "manifest_name", "manifest_object_key", "manifest_sha256", "manifest_size_bytes"], "release-set target reference");
    requireString(entry.manifest_name, NAME, "target-manifest name");
    requireString(entry.manifest_sha256, SHA256, "target-manifest digest");
    requireSize(entry.manifest_size_bytes, MAX_MANIFEST_BYTES, "target-manifest size");
    if (entry.manifest_object_key !== `sha256/${entry.manifest_sha256}/${entry.manifest_name}`) {
      contractError("target-manifest object key is not content-addressed");
    }
  }
  return value;
}

export function signEnvelope(payload, privateKey, trust) {
  const payloadBytes = canonicalJsonBytes(payload);
  if (payloadBytes.length === 0 || payloadBytes.length > MAX_MANIFEST_BYTES) {
    contractError("managed-pair manifest is outside its bounded size");
  }
  let key;
  try {
    key = crypto.createPrivateKey(privateKey);
  } catch {
    contractError("managed-pair signing key is invalid");
  }
  const publicKey = crypto.createPublicKey(key);
  const bits = publicKey.asymmetricKeyDetails?.modulusLength ?? 0;
  if (bits < MIN_RSA_MODULUS_BITS || bits > MAX_RSA_MODULUS_BITS) {
    contractError("managed-pair signing key is outside the public RSA verifier bounds");
  }
  const publicDigest = sha256(publicKey.export({ format: "der", type: "pkcs1" }));
  if (publicDigest !== trust.publicKeyDigest) contractError("managed-pair signing key does not match channel authority");
  const signature = crypto.sign("RSA-SHA256", payloadBytes, {
    key,
    padding: crypto.constants.RSA_PKCS1_PADDING,
  });
  if (!crypto.verify("RSA-SHA256", payloadBytes, trust.publicKey, signature)) {
    contractError("managed-pair signature self-check failed");
  }
  const envelope = {
    schema_version: 1,
    manifest_base64: payloadBytes.toString("base64"),
    signature_base64: signature.toString("base64"),
  };
  return Object.freeze({
    envelope,
    envelopeBytes: canonicalJsonBytes(envelope),
    payload,
    payloadBytes,
    signature,
  });
}

function decodeEnvelope(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length === 0 || bytes.length > MAX_MANIFEST_BYTES) {
    contractError("managed-pair envelope is outside its bounded size");
  }
  const envelope = parseJsonStrict(bytes, "managed-pair signed envelope");
  exactKeys(envelope, ["schema_version", "manifest_base64", "signature_base64"], "managed-pair signed envelope");
  if (envelope.schema_version !== 1 || canonicalJsonBytes(envelope).compare(bytes) !== 0) {
    contractError("managed-pair signed envelope is not canonical V1 JSON");
  }
  const payloadBytes = strictBase64(envelope.manifest_base64, "envelope payload", MAX_MANIFEST_BYTES);
  const signature = strictBase64(envelope.signature_base64, "envelope signature", MAX_SIGNATURE_BYTES);
  const payload = parseJsonStrict(payloadBytes, "managed-pair envelope payload");
  if (canonicalJsonBytes(payload).compare(payloadBytes) !== 0) {
    contractError("managed-pair envelope payload is not canonical JSON");
  }
  return Object.freeze({ envelope, payload, payloadBytes, signature });
}

export function verifyEnvelope(bytes, matrix, trust) {
  const decoded = decodeEnvelope(bytes);
  const { envelope, payload, payloadBytes, signature } = decoded;
  const modulusBytes = Math.ceil(trust.publicKey.asymmetricKeyDetails.modulusLength / 8);
  if (signature.length !== modulusBytes
      || !crypto.verify("RSA-SHA256", payloadBytes, trust.publicKey, signature)) {
    contractError("managed-pair envelope signature is invalid");
  }
  if (payload.contract === "ctx-managed-pair-manifest") validateTargetManifest(payload, matrix, trust);
  else if (payload.contract === "ctx-managed-pair-release-set") validateReleaseSet(payload, matrix, trust);
  else contractError("managed-pair envelope contains an unsupported contract");
  return Object.freeze({ envelope, payload, payloadBytes, signature });
}

export function verifyReleaseSetWithTrustedMatrix(bytes, inputAuthority, trust) {
  const decoded = decodeEnvelope(bytes);
  if (decoded.payload?.contract !== "ctx-managed-pair-release-set") {
    contractError("existing release pointer does not contain a release set");
  }
  const matrix = trustedTargetMatrix(
    inputAuthority,
    decoded.payload.target_matrix_sha256,
  );
  return Object.freeze({
    matrix,
    verified: verifyEnvelope(bytes, matrix, trust),
  });
}
