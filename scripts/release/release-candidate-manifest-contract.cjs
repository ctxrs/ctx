"use strict";

const childProcess = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const CANDIDATE_MANIFEST_DIGEST_PREFIX =
  "CTX_RELEASE_CANDIDATE_MANIFEST_SHA256_";
const CORE_GITHUB_HANDOFF = "ctx-core-github-handoff.json";
const CORE_GITHUB_HANDOFF_METADATA_KEY =
  "CTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256";
const PUBLIC_MANIFEST_AUTHORITY_COMMIT =
  "4eb7234af45b568a4200e7331570d9056a1c5cdd";

const CANDIDATE_MANIFEST_MATRIX = Object.freeze([
  Object.freeze({
    key: "linux_x64",
    id: "linux-x64",
    platform: "linux-x64",
    artifact: "ctx",
    constructionLabel: "scripts/release/build-public-candidate-on-linux.sh",
    manifest: "ctx.candidate.json",
    rustTriple: "x86_64-unknown-linux-gnu",
  }),
  Object.freeze({
    key: "linux_aarch64",
    id: "linux-arm64",
    platform: "linux-aarch64",
    artifact: "ctx-linux-aarch64",
    constructionLabel: "scripts/release/build-public-candidate-on-linux.sh",
    manifest: "ctx-linux-aarch64.candidate.json",
    rustTriple: "aarch64-unknown-linux-gnu",
  }),
  Object.freeze({
    key: "macos_arm64",
    id: "macos-arm64",
    platform: "macos-arm64",
    artifact: "ctx-macos-arm64",
    constructionLabel: "scripts/release/build-public-candidate-on-linux.sh",
    manifest: "ctx-macos-arm64.candidate.json",
    rustTriple: "aarch64-apple-darwin",
  }),
  Object.freeze({
    key: "macos_x64",
    id: "macos-x64",
    platform: "macos-x64",
    artifact: "ctx-macos-x64",
    constructionLabel: "scripts/release/build-public-candidate-on-linux.sh",
    manifest: "ctx-macos-x64.candidate.json",
    rustTriple: "x86_64-apple-darwin",
  }),
  Object.freeze({
    key: "windows_x64",
    id: "windows-x64",
    platform: "windows-x64",
    artifact: "ctx.exe",
    constructionLabel: "scripts/release/build-public-candidate-on-linux.sh",
    manifest: "ctx.exe.candidate.json",
    rustTriple: "x86_64-pc-windows-gnu",
  }),
]);

function candidateManifestDigestField(entry) {
  return `${CANDIDATE_MANIFEST_DIGEST_PREFIX}${entry.key}`;
}

function requireCandidateManifestDigestMatrix(values, label) {
  const expectedKeys = CANDIDATE_MANIFEST_MATRIX.map((entry) => entry.key).sort();
  const actualKeys = Object.keys(values)
    .filter((key) => key.startsWith(CANDIDATE_MANIFEST_DIGEST_PREFIX))
    .map((key) => key.slice(CANDIDATE_MANIFEST_DIGEST_PREFIX.length))
    .sort();
  if (actualKeys.join("\n") !== expectedKeys.join("\n")) {
    const missing = expectedKeys.filter((key) => !actualKeys.includes(key));
    const unexpected = actualKeys.filter((key) => !expectedKeys.includes(key));
    const details = [];
    if (missing.length > 0) details.push(`missing ${missing.join(", ")}`);
    if (unexpected.length > 0) {
      details.push(`unexpected ${unexpected.join(", ")}`);
    }
    throw new Error(
      `${label} metadata has wrong candidate manifest digest matrix: ${details.join("; ")}`,
    );
  }

  const digests = new Map();
  for (const entry of CANDIDATE_MANIFEST_MATRIX) {
    const field = candidateManifestDigestField(entry);
    const digest = values[field];
    if (!/^[0-9a-f]{64}$/.test(digest || "") || digest === "0".repeat(64)) {
      throw new Error(`${label} metadata ${field} is not a nonzero lowercase SHA-256 digest`);
    }
    digests.set(entry.key, digest);
  }
  return digests;
}

function compareCandidateManifestDigestMatrices(stableValues, versionedValues) {
  for (const entry of CANDIDATE_MANIFEST_MATRIX) {
    const field = candidateManifestDigestField(entry);
    if (stableValues[field] !== versionedValues[field]) {
      throw new Error(`stable metadata ${field} does not match versioned metadata`);
    }
  }
}

function canonicalJson(value) {
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function sameIdentity(left, right) {
  return ["dev", "ino", "mode", "nlink", "size", "mtimeNs", "ctimeNs"].every(
    (field) => left[field] === right[field],
  );
}

function snapshotRegularFile(file, label, maximumBytes) {
  let descriptor;
  try {
    descriptor = fs.openSync(
      file,
      fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0),
    );
    const before = fs.fstatSync(descriptor, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n || before.size > BigInt(maximumBytes)) {
      throw new Error(`${label} is not a bounded single-link regular file`);
    }
    const bytes = fs.readFileSync(descriptor);
    const after = fs.fstatSync(descriptor, { bigint: true });
    const pathIdentity = fs.lstatSync(file, { bigint: true });
    if (
      BigInt(bytes.length) !== before.size ||
      !sameIdentity(before, after) ||
      !sameIdentity(before, pathIdentity)
    ) {
      throw new Error(`${label} changed while its snapshot was read`);
    }
    return { bytes, identity: before };
  } catch (error) {
    if (error.message?.startsWith(label)) throw error;
    throw new Error(`could not snapshot ${label}: ${error.message}`);
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
}

function parseEnvMetadata(bytes, label) {
  const values = {};
  const lines = bytes.toString("utf8").split(/\r?\n/);
  for (const [index, rawLine] of lines.entries()) {
    if (rawLine === "" || /^[ \t]*#/.test(rawLine)) continue;
    const equals = rawLine.indexOf("=");
    if (equals <= 0) {
      throw new Error(`${label} metadata line ${index + 1} is not KEY=value`);
    }
    const key = rawLine.slice(0, equals);
    const value = rawLine.slice(equals + 1);
    if (
      key !== key.trim() ||
      value !== value.trim() ||
      !/^[A-Za-z0-9_]+$/.test(key)
    ) {
      throw new Error(`${label} metadata line ${index + 1} is not canonical`);
    }
    if (Object.hasOwn(values, key)) {
      throw new Error(`${label} metadata repeats ${key}`);
    }
    values[key] = value;
  }
  return values;
}

function publicKeyJwk(publicKeyPem, label) {
  try {
    const jwk = crypto.createPublicKey(publicKeyPem).export({ format: "jwk" });
    if (jwk.kty !== "RSA" || !jwk.n || !jwk.e) {
      throw new Error("key is not RSA");
    }
    return jwk;
  } catch (error) {
    throw new Error(`could not parse ${label}: ${error.message}`);
  }
}

function productionMetadataPublicKey(
  publicRepo,
  sourceCommit,
  environment,
) {
  const installerPath = path.join(
    path.resolve(publicRepo),
    "services/install-site/src/cli-install-script.js",
  );
  const installer = snapshotRegularFile(
    installerPath,
    "hosted installer metadata public-key source",
    1024 * 1024,
  ).bytes.toString("utf8");
  const installerMatch = installer.match(
    /DEFAULT_METADATA_PUBLIC_KEY_PEM\s*=\s*`([\s\S]*?)`;/,
  );
  if (!installerMatch) {
    throw new Error("hosted installer metadata public key is unavailable");
  }

  const publicSourceCandidates = [
    "crates/ctx-upgrade-engine/src/upgrade/metadata.rs",
    "crates/ctx-cli/src/upgrade/metadata.rs",
    "crates/ctx-cli/src/upgrade.rs",
  ];
  const publicSourcePath = publicSourceCandidates
    .map((candidate) => path.join(path.resolve(publicRepo), candidate))
    .find((candidate) => fs.existsSync(candidate));
  if (!publicSourcePath) {
    throw new Error("public CLI metadata public-key source is unavailable");
  }
  assertTrackedFile(
    publicRepo,
    sourceCommit,
    path.relative(path.resolve(publicRepo), publicSourcePath),
    "public CLI metadata public-key source",
    environment,
  );
  const publicSource = snapshotRegularFile(
    publicSourcePath,
    "public CLI metadata public-key source",
    1024 * 1024,
  ).bytes.toString("utf8");
  const publicMatch = publicSource.match(
    /(?:DEFAULT|RELEASE)_METADATA_PUBLIC_KEY_PEM\s*:\s*&str\s*=\s*r#?"([\s\S]*?)"#?;/,
  );
  if (!publicMatch) {
    throw new Error("public CLI metadata public key is unavailable");
  }

  const installerKey = installerMatch[1].trim();
  const installerJwk = publicKeyJwk(
    installerKey,
    "hosted installer metadata public key",
  );
  const publicJwk = publicKeyJwk(
    publicMatch[1].trim(),
    "public CLI metadata public key",
  );
  if (installerJwk.n !== publicJwk.n || installerJwk.e !== publicJwk.e) {
    throw new Error(
      "public CLI metadata public key does not match hosted installer metadata public key",
    );
  }
  return installerKey;
}

function decodeCanonicalBase64(bytes, label) {
  const text = bytes.toString("ascii");
  const body = text.endsWith("\n") ? text.slice(0, -1) : text;
  if (
    body.length === 0 ||
    body.length % 4 !== 0 ||
    !/^[A-Za-z0-9+/]+={0,2}$/.test(body)
  ) {
    throw new Error(`${label} is not canonical base64`);
  }
  const decoded = Buffer.from(body, "base64");
  if (decoded.length === 0 || decoded.toString("base64") !== body) {
    throw new Error(`${label} is not canonical base64`);
  }
  return decoded;
}

function requireCandidateBinding(candidate, entry, values, label) {
  const expectedArtifact = values[`CTX_RELEASE_ARTIFACT_${entry.key}`];
  const expectedArtifactSha256 = values[`CTX_RELEASE_SHA256_${entry.key}`];
  const target = candidate?.target;
  const source = candidate?.source;
  const artifact = candidate?.artifact;
  if (
    candidate?.schema_version !== 1 ||
    candidate?.kind !== "ctx-public-cli-candidate" ||
    candidate?.product !== "core" ||
    candidate?.version !== values.CTX_RELEASE_VERSION ||
    candidate?.construction?.authority !== "linux-cross-cargo-zigbuild-v1" ||
    candidate?.construction?.label !== entry.constructionLabel ||
    target?.id !== entry.id ||
    target?.platform !== entry.platform ||
    target?.rust_triple !== entry.rustTriple ||
    source?.clean !== true ||
    source?.commit !== values.CTX_RELEASE_SOURCE_COMMIT ||
    expectedArtifact !== entry.artifact ||
    !/^[0-9a-f]{64}$/.test(expectedArtifactSha256 || "") ||
    expectedArtifactSha256 === "0".repeat(64) ||
    artifact?.file !== entry.artifact ||
    artifact?.sha256 !== expectedArtifactSha256 ||
    !Number.isSafeInteger(artifact?.size_bytes) ||
    artifact.size_bytes <= 0
  ) {
    throw new Error(
      `${label} ${entry.manifest} does not bind the signed release version, source, platform, and artifact`,
    );
  }
}

function snapshotCandidateManifest(handoffDir, entry, values, label) {
  const manifestPath = path.join(handoffDir, entry.manifest);
  const manifest = snapshotRegularFile(
    manifestPath,
    `${label} ${entry.manifest}`,
    16 * 1024 * 1024,
  );
  let candidate;
  try {
    candidate = JSON.parse(manifest.bytes.toString("utf8"));
  } catch (error) {
    throw new Error(`${label} ${entry.manifest} is malformed JSON: ${error.message}`);
  }
  const canonical = `${canonicalJson(candidate)}\n`;
  if (!Buffer.from(canonical, "utf8").equals(manifest.bytes)) {
    throw new Error(`${label} ${entry.manifest} is not canonical JSON`);
  }
  requireCandidateBinding(candidate, entry, values, label);
  const digest = sha256(manifest.bytes);
  const sidecarName = `${entry.manifest}.sha256`;
  const sidecar = snapshotRegularFile(
    path.join(handoffDir, sidecarName),
    `${label} ${sidecarName}`,
    65,
  );
  if (sidecar.bytes.toString("ascii") !== `${digest}\n`) {
    throw new Error(
      `${label} ${sidecarName} does not match the independently recomputed manifest digest`,
    );
  }
  const signedDigest = values[candidateManifestDigestField(entry)];
  if (digest !== signedDigest) {
    throw new Error(
      `${label} ${entry.manifest} digest does not match ${candidateManifestDigestField(entry)}`,
    );
  }
  return {
    digest,
    manifest_identity: manifest.identity,
    sidecar_identity: sidecar.identity,
  };
}

function run(command, args, options = {}) {
  const result = childProcess.spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  });
  if (result.error) {
    throw new Error(`could not execute ${command}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = result.stderr.trim() || result.stdout.trim() || `exit ${result.status}`;
    throw new Error(`${command} rejected candidate manifest handoff: ${detail}`);
  }
  return result.stdout.trim();
}

function assertTrackedFile(repo, commit, relative, label, environment) {
  const root = path.resolve(repo);
  const file = path.join(root, relative);
  const options = { env: environment };
  const workingHash = run(
    "git",
    ["-C", root, "hash-object", "--", file],
    options,
  );
  const committedHash = run(
    "git",
    ["-C", root, "rev-parse", `${commit}:${relative}`],
    options,
  );
  if (workingHash !== committedHash) {
    throw new Error(`${label} differs from public source commit ${commit}`);
  }
}

function assertPublicVerifierCheckout(publicRepo, sourceCommit, environment) {
  const resolved = path.resolve(publicRepo);
  const verifier = path.join(resolved, "scripts/release-sbom.py");
  if (!fs.existsSync(path.join(resolved, "Cargo.toml")) || !fs.existsSync(verifier)) {
    throw new Error(`public ctx checkout does not contain the release verifier: ${resolved}`);
  }
  const verifierSnapshot = snapshotRegularFile(
    verifier,
    "public release verifier",
    2 * 1024 * 1024,
  );
  const gitOptions = { env: environment };
  const head = run("git", ["-C", resolved, "rev-parse", "HEAD"], gitOptions);
  if (head !== sourceCommit) {
    throw new Error(`public ctx checkout is at ${head}, expected ${sourceCommit}`);
  }
  try {
    run(
      "git",
      [
        "-C",
        resolved,
        "merge-base",
        "--is-ancestor",
        PUBLIC_MANIFEST_AUTHORITY_COMMIT,
        sourceCommit,
      ],
      gitOptions,
    );
  } catch {
    throw new Error(
      `public source commit ${sourceCommit} is not a descendant of manifest authority ${PUBLIC_MANIFEST_AUTHORITY_COMMIT}`,
    );
  }
  assertTrackedFile(
    resolved,
    sourceCommit,
    "scripts/release-sbom.py",
    "public release verifier",
    environment,
  );
  const status = run(
    "git",
    ["-C", resolved, "status", "--porcelain"],
    gitOptions,
  );
  if (status !== "") {
    throw new Error("public ctx checkout must be clean before candidate manifest verification");
  }
  run("python3", [
    "-I", "-B", path.join(__dirname, "released-source-continuity.py"),
    "--public-repo", resolved, "--source-commit", sourceCommit,
  ], gitOptions);
  return {
    repo: resolved,
    verifier,
    verifier_identity: verifierSnapshot.identity,
  };
}

function verifySignedCandidateManifestHandoff({
  metadataPath,
  signaturePath,
  handoffDir,
  publicRepo,
  python = "python3",
  environment = process.env,
}) {
  const metadata = snapshotRegularFile(
    metadataPath,
    "signed release metadata",
    1024 * 1024,
  );
  const signature = snapshotRegularFile(
    signaturePath,
    "signed release metadata signature",
    64 * 1024,
  );
  const values = parseEnvMetadata(metadata.bytes, "signed release");
  if (
    values.CTX_RELEASE_SCHEMA_VERSION !== "1" ||
    values.CTX_RELEASE_CHANNEL !== "stable"
  ) {
    throw new Error("signed release metadata is not stable schema 1 metadata");
  }
  const sourceCommit = values.CTX_RELEASE_SOURCE_COMMIT;
  if (!/^[0-9a-f]{40}$/.test(sourceCommit || "") || sourceCommit === "0".repeat(40)) {
    throw new Error("signed release metadata has an invalid source commit");
  }
  const verificationEnvironment = { ...environment };
  delete verificationEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
  delete verificationEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY;
  assertPublicVerifierCheckout(
    publicRepo,
    sourceCommit,
    verificationEnvironment,
  );
  const publicKey = productionMetadataPublicKey(
    publicRepo,
    sourceCommit,
    verificationEnvironment,
  );
  const verified = crypto.verify(
    "RSA-SHA256",
    metadata.bytes,
    { key: publicKey, padding: crypto.constants.RSA_PKCS1_PADDING },
    decodeCanonicalBase64(signature.bytes, "signed release metadata signature"),
  );
  if (!verified) {
    throw new Error("signed release metadata signature verification failed");
  }
  const digests = verifyCandidateManifestHandoff({
    values,
    label: "signed stable",
    handoffDir,
    publicRepo,
    python,
    environment,
  });
  const metadataAfter = snapshotRegularFile(
    metadataPath,
    "signed release metadata",
    1024 * 1024,
  );
  const signatureAfter = snapshotRegularFile(
    signaturePath,
    "signed release metadata signature",
    64 * 1024,
  );
  if (
    !sameIdentity(metadata.identity, metadataAfter.identity) ||
    !sameIdentity(signature.identity, signatureAfter.identity)
  ) {
    throw new Error("signed release metadata changed while candidate handoff was verified");
  }
  return digests;
}

function verifyCandidateManifestHandoff({
  values,
  label,
  handoffDir,
  publicRepo,
  python = "python3",
  environment = process.env,
}) {
  const digests = requireCandidateManifestDigestMatrix(values, label);
  const expectedHandoffSha256 = values[CORE_GITHUB_HANDOFF_METADATA_KEY];
  if (
    !/^[0-9a-f]{64}$/.test(expectedHandoffSha256 || "") ||
    expectedHandoffSha256 === "0".repeat(64)
  ) {
    throw new Error(
      `${label} metadata ${CORE_GITHUB_HANDOFF_METADATA_KEY} is not a nonzero lowercase SHA-256 digest`,
    );
  }
  const sourceCommit = values.CTX_RELEASE_SOURCE_COMMIT;
  if (!/^[0-9a-f]{40}$/.test(sourceCommit || "") || sourceCommit === "0".repeat(40)) {
    throw new Error(`${label} metadata has invalid CTX_RELEASE_SOURCE_COMMIT`);
  }
  const verifierEnvironment = { ...environment };
  delete verifierEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
  delete verifierEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY;
  const checkout = assertPublicVerifierCheckout(
    publicRepo,
    sourceCommit,
    verifierEnvironment,
  );
  const resolvedHandoff = path.resolve(handoffDir);
  const handoffDocument = snapshotRegularFile(
    path.join(resolvedHandoff, CORE_GITHUB_HANDOFF),
    `${label} ${CORE_GITHUB_HANDOFF}`,
    16 * 1024 * 1024,
  );
  const snapshots = new Map();
  for (const entry of CANDIDATE_MANIFEST_MATRIX) {
    snapshots.set(
      entry.key,
      snapshotCandidateManifest(resolvedHandoff, entry, values, label),
    );
  }

  const verifiedDigest = run(
    python,
    [
      "-I",
      checkout.verifier,
      "verify-release",
      "--handoff-dir",
      resolvedHandoff,
      "--expected-handoff-sha256",
      expectedHandoffSha256,
    ],
    { env: verifierEnvironment },
  );
  if (verifiedDigest !== expectedHandoffSha256) {
    throw new Error("public release verifier returned the wrong Core GitHub handoff digest");
  }

  for (const entry of CANDIDATE_MANIFEST_MATRIX) {
    const after = snapshotCandidateManifest(resolvedHandoff, entry, values, label);
    const before = snapshots.get(entry.key);
    if (
      after.digest !== before.digest ||
      !sameIdentity(after.manifest_identity, before.manifest_identity) ||
      !sameIdentity(after.sidecar_identity, before.sidecar_identity)
    ) {
      throw new Error(`${label} candidate manifest handoff changed while verified`);
    }
  }
  const handoffDocumentAfter = snapshotRegularFile(
    path.join(resolvedHandoff, CORE_GITHUB_HANDOFF),
    `${label} ${CORE_GITHUB_HANDOFF}`,
    16 * 1024 * 1024,
  );
  if (!sameIdentity(handoffDocument.identity, handoffDocumentAfter.identity)) {
    throw new Error(`${label} Core GitHub handoff changed while verified`);
  }
  const checkoutAfter = assertPublicVerifierCheckout(
    checkout.repo,
    sourceCommit,
    verifierEnvironment,
  );
  if (!sameIdentity(checkout.verifier_identity, checkoutAfter.verifier_identity)) {
    throw new Error("public release verifier changed while candidate handoff was verified");
  }
  return Object.fromEntries(
    CANDIDATE_MANIFEST_MATRIX.map((entry) => [entry.key, digests.get(entry.key)]),
  );
}

module.exports = {
  CANDIDATE_MANIFEST_DIGEST_PREFIX,
  CANDIDATE_MANIFEST_MATRIX,
  CORE_GITHUB_HANDOFF_METADATA_KEY,
  PUBLIC_MANIFEST_AUTHORITY_COMMIT,
  candidateManifestDigestField,
  compareCandidateManifestDigestMatrices,
  parseEnvMetadata,
  requireCandidateManifestDigestMatrix,
  sameIdentity,
  snapshotRegularFile,
  verifyCandidateManifestHandoff,
  verifySignedCandidateManifestHandoff,
};
