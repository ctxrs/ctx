#!/usr/bin/env node

import childProcess from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { createR2Request, putImmutableR2Object } from "./core-r2.mjs";

export const COREML_REPAIR = Object.freeze({
  archiveBasename: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
  archiveSha256: "25fbf333d1e72f5c075973ef968dfa1446459f61f3ac63ef3690d9865435af17",
  archiveSizeBytes: 423625016,
  assetId: "apple_coreml",
  bucket: "ctx-releases-prod",
  key: "artifacts/stable/1.0.0/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
  manifestPath: "manifest.json",
  manifestSha256: "20a94162aca7c2f9f65be27839cd6867ec1c54e142fdf0c652de20139dffbc19",
  validatorRelativePath: "scripts/semantic-release-assets.py",
});

// This is deliberately an authority list, not a Python import resolver. The
// repair executes exactly these Git-committed bytes and rejects every other
// shape, including a newly added local helper.
export const COREML_VALIDATOR_CLOSURE = Object.freeze({
  files: Object.freeze([
    Object.freeze({ path: "scripts/semantic-release-assets.py", sha256: "d5b154c2b04a7fee4cb3e10425f4d60d972771b6faca095bbb87c78d97f4be38" }),
    Object.freeze({ path: "scripts/semantic_release_assets/__init__.py", sha256: "db8a0c4c76473cb5843c0646d428de4f86bbea5cff21624e1a54087ca9e09f81" }),
    Object.freeze({ path: "scripts/semantic_release_assets/common.py", sha256: "5b7c6b2e9f134e4f0115a41929b5d2250ac50e1593d84081512d4c383ca6f97d" }),
    Object.freeze({ path: "scripts/semantic_release_assets/contracts.py", sha256: "a5948c17ef55f7ce2dfc621a56bd97ab436fcdcdec157296b6c318e93b974ccf" }),
  ]),
  sha256: "e4ff84760632654232f33bf76c7102a3e7a90e8b2f8bdcf8c9f5d90a641331d8",
});

const R2_AUTHORITY = Object.freeze({
  accessKeyEnv: "CTX_RELEASE_R2_ACCESS_KEY_ID",
  bucket: COREML_REPAIR.bucket,
  endpointEnv: "CTX_RELEASE_R2_ENDPOINT",
  label: "CoreML semantic object repair",
  secretKeyEnv: "CTX_RELEASE_R2_SECRET_ACCESS_KEY",
});
const PUBLIC_SOURCE = "clean-explicit-public-ctx-git-worktree-snapshot";
const MAX_SIDECAR_BYTES = 64 * 1024;
const MAX_ASSET_SIDECAR_BYTES = 16 * 1024 * 1024;
const MAX_EVIDENCE_BYTES = 16 * 1024;
const MAX_VALIDATOR_FILE_BYTES = 2 * 1024 * 1024;
const SHA256 = /^[0-9a-f]{64}$/u;
const GIT_OID = /^[0-9a-f]{40,64}$/u;
const CREDENTIAL_FAILURES = new Set([
  "access-key-fetch-failed",
  "secret-key-fetch-failed",
  "endpoint-fetch-failed",
  "bucket-fetch-failed",
  "bucket-mismatch",
]);

function fail(message) { throw new Error(message); }
function sha256(body) { return crypto.createHash("sha256").update(body).digest("hex"); }
function canonicalValue(candidate) {
  if (Array.isArray(candidate)) return candidate.map(canonicalValue);
  if (candidate != null && typeof candidate === "object") {
    return Object.fromEntries(Object.keys(candidate).sort().map((key) => [key, canonicalValue(candidate[key])]));
  }
  return candidate;
}
function canonicalJson(value) {
  return `${JSON.stringify(canonicalValue(value))}\n`;
}
function canonicalEqual(left, right) {
  return canonicalJson(left) === canonicalJson(right);
}

function parseArgs(argv) {
  const command = argv[0];
  const requirements = new Map([
    ["preflight", ["--archive", "--evidence-out", "--public-ctx-repo"]],
    ["begin-credentials", ["--archive", "--evidence-out"]],
    ["credential-failed", ["--evidence-out", "--failure"]],
    ["publish", ["--archive", "--evidence-out"]],
  ]);
  if (!requirements.has(command)) fail("expected a CoreML repair command");
  const args = new Map();
  for (let index = 1; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith("--") || value == null || args.has(name)) fail("invalid arguments");
    args.set(name, value);
  }
  const required = requirements.get(command);
  if (args.size !== required.length || required.some((name) => !args.has(name))) {
    fail(`${command} requires exactly ${required.join(", ")}`);
  }
  if (command === "credential-failed" && !CREDENTIAL_FAILURES.has(args.get("--failure"))) {
    fail("credential-failed requires a fixed sanitized failure");
  }
  return { args, command };
}

function readStableFile(file, label, maximumBytes, expectedBytes = undefined) {
  const absolute = path.resolve(file);
  let descriptor;
  try {
    descriptor = fs.openSync(absolute, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  } catch {
    fail(`${label} is not an identity-safe bounded file`);
  }
  try {
    const before = fs.fstatSync(descriptor, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n || before.size < 1n
        || before.size > BigInt(maximumBytes)
        || (expectedBytes != null && before.size !== BigInt(expectedBytes))) {
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
    return { absolute, body, sizeBytes: Number(before.size) };
  } finally {
    fs.closeSync(descriptor);
  }
}

function parseCanonicalSha256Sidecar(body) {
  const expected = `${COREML_REPAIR.archiveSha256}  ${COREML_REPAIR.archiveBasename}\n`;
  if (body.toString("utf8") !== expected) fail("CoreML .sha256 sidecar is not canonical");
  return { sha256: sha256(body), size_bytes: body.length };
}

function parseCanonicalAssetSidecar(body) {
  const text = body.toString("utf8");
  let record;
  try { record = JSON.parse(text); } catch { fail("CoreML .asset.json sidecar is not canonical"); }
  const manifest = Array.isArray(record?.asset?.files)
    ? record.asset.files.find((entry) => entry?.path === COREML_REPAIR.manifestPath)
    : undefined;
  if (text !== canonicalJson(record)
      || record?.id !== COREML_REPAIR.assetId
      || record?.asset?.artifact !== COREML_REPAIR.archiveBasename
      || record?.asset?.archive_sha256 !== COREML_REPAIR.archiveSha256
      || manifest?.sha256 !== COREML_REPAIR.manifestSha256) {
    fail("CoreML .asset.json sidecar is not canonical");
  }
  return { sha256: sha256(body), size_bytes: body.length };
}

function loadArtifact(archivePath, dependencies) {
  const archive = dependencies.readStableFile(archivePath, "CoreML archive", COREML_REPAIR.archiveSizeBytes, COREML_REPAIR.archiveSizeBytes);
  if (path.basename(archive.absolute) !== COREML_REPAIR.archiveBasename
      || archive.sizeBytes !== COREML_REPAIR.archiveSizeBytes
      || dependencies.sha256(archive.body) !== COREML_REPAIR.archiveSha256) {
    fail("CoreML archive identity differs from the immutable repair pin");
  }
  const checksum = dependencies.readStableFile(`${archive.absolute}.sha256`, "CoreML .sha256 sidecar", MAX_SIDECAR_BYTES);
  const asset = dependencies.readStableFile(`${archive.absolute}.asset.json`, "CoreML .asset.json sidecar", MAX_ASSET_SIDECAR_BYTES);
  return Object.freeze({
    archive,
    assetSidecar: parseCanonicalAssetSidecar(asset.body),
    checksumSidecar: parseCanonicalSha256Sidecar(checksum.body),
  });
}

function artifactIdentity(artifact) {
  return {
    archive: {
      basename: COREML_REPAIR.archiveBasename,
      sha256: COREML_REPAIR.archiveSha256,
      size_bytes: COREML_REPAIR.archiveSizeBytes,
    },
    embedded_manifest: { path: COREML_REPAIR.manifestPath, sha256: COREML_REPAIR.manifestSha256 },
    sidecars: { asset_json: artifact.assetSidecar, sha256: artifact.checksumSidecar },
  };
}

function runChecked(command, argv, options, dependencies, label) {
  const result = dependencies.spawnSync(command, argv, options);
  if (result?.error != null || result?.status !== 0 || result?.signal != null) fail(`${label} failed`);
  return result;
}
function outputText(result, label) {
  const output = Buffer.isBuffer(result.stdout) ? result.stdout.toString("utf8") : result.stdout;
  if (typeof output !== "string") fail(`${label} returned malformed output`);
  return output.trim();
}
function checkedGit(checkout, argv, dependencies, label) {
  return outputText(runChecked("git", ["-C", checkout, ...argv], {
    encoding: "buffer", env: { LC_ALL: "C", PATH: process.env.PATH ?? "/usr/bin:/bin" }, maxBuffer: MAX_VALIDATOR_FILE_BYTES,
  }, dependencies, label), label);
}
function checkedGitBytes(checkout, argv, dependencies, label) {
  const result = runChecked("git", ["-C", checkout, ...argv], {
    encoding: "buffer", env: { LC_ALL: "C", PATH: process.env.PATH ?? "/usr/bin:/bin" }, maxBuffer: MAX_VALIDATOR_FILE_BYTES,
  }, dependencies, label);
  if (!Buffer.isBuffer(result.stdout) || result.stdout.length === 0 || result.stdout.length > MAX_VALIDATOR_FILE_BYTES) {
    fail(`${label} returned invalid bytes`);
  }
  return result.stdout;
}

function assertPinnedClosure(closure) {
  if (!canonicalEqual(closure.files, COREML_VALIDATOR_CLOSURE.files)
      || closure.sha256 !== COREML_VALIDATOR_CLOSURE.sha256
      || sha256(Buffer.from(canonicalJson(closure.files), "utf8")) !== COREML_VALIDATOR_CLOSURE.sha256) {
    fail("CoreML validator closure differs from the fixed repair authority");
  }
}

function capturePublicValidatorClosure(publicCtxRepo, dependencies) {
  const requested = path.resolve(publicCtxRepo);
  const stat = fs.lstatSync(requested);
  if (!stat.isDirectory() || stat.isSymbolicLink()) fail("public ctx checkout must be a non-symlink directory");
  const checkout = fs.realpathSync(requested);
  const root = checkedGit(checkout, ["rev-parse", "--show-toplevel"], dependencies, "public checkout discovery");
  if (path.resolve(root) !== checkout) fail("public ctx checkout must be its Git worktree root");
  const before = {
    commit: checkedGit(checkout, ["rev-parse", "HEAD^{commit}"], dependencies, "public checkout commit"),
    status: checkedGit(checkout, ["status", "--porcelain=v1", "--untracked-files=all"], dependencies, "public checkout status"),
    tree: checkedGit(checkout, ["rev-parse", "HEAD^{tree}"], dependencies, "public checkout tree"),
  };
  if (!GIT_OID.test(before.commit) || !GIT_OID.test(before.tree) || before.status !== "") {
    fail("public ctx checkout must be a clean committed worktree");
  }
  assertPinnedClosure(COREML_VALIDATOR_CLOSURE);
  const closure = new Map();
  for (const authority of COREML_VALIDATOR_CLOSURE.files) {
    const body = checkedGitBytes(checkout, ["show", `${before.commit}:${authority.path}`], dependencies, "public validator closure capture");
    if (dependencies.sha256(body) !== authority.sha256) fail(`public validator closure identity differs: ${authority.path}`);
    closure.set(authority.path, body);
  }
  const after = {
    commit: checkedGit(checkout, ["rev-parse", "HEAD^{commit}"], dependencies, "public checkout commit"),
    status: checkedGit(checkout, ["status", "--porcelain=v1", "--untracked-files=all"], dependencies, "public checkout status"),
    tree: checkedGit(checkout, ["rev-parse", "HEAD^{tree}"], dependencies, "public checkout tree"),
  };
  if (!canonicalEqual(before, after) || after.status !== "") fail("public ctx checkout changed during validator capture");
  const snapshot = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-coreml-semantic-validator-snapshot."));
  fs.chmodSync(snapshot, 0o700);
  try {
    for (const [relative, body] of closure) {
      const destination = path.join(snapshot, relative);
      fs.mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
      fs.writeFileSync(destination, body, { flag: "wx", mode: 0o600 });
      fs.chmodSync(destination, 0o400);
    }
  } catch (error) {
    fs.rmSync(snapshot, { force: true, recursive: true });
    throw error;
  }
  return { closure: COREML_VALIDATOR_CLOSURE, publicCommit: before.commit, publicTree: before.tree, snapshot };
}

function validateValidatorOutput(stdout) {
  if (typeof stdout !== "string") fail("public CoreML semantic validator returned malformed output");
  const lines = stdout.trimEnd().split("\n");
  if (lines.length !== 3 || lines[0] !== `archive_sha256=${COREML_REPAIR.archiveSha256}`
      || lines[1] !== `manifest_sha256=${COREML_REPAIR.manifestSha256}` || !lines[2].startsWith("cache_bundle=")) {
    fail("public CoreML semantic validator did not confirm the pinned archive identity");
  }
}

export function runPublicValidator(publicCtxRepo, archive, dependencies) {
  const captured = capturePublicValidatorClosure(publicCtxRepo, dependencies);
  const cache = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-coreml-semantic-validator-cache."));
  fs.chmodSync(cache, 0o700);
  try {
    if (fs.readdirSync(cache).length !== 0) fail("public CoreML semantic validator cache is not fresh");
    const result = dependencies.spawnSync("python3", ["-I", path.join(captured.snapshot, COREML_REPAIR.validatorRelativePath), "bind-coreml-cache", "--archive", archive.absolute, "--cache-dir", cache], {
      encoding: "utf8",
      env: { HOME: cache, LC_ALL: "C", PATH: process.env.PATH ?? "/usr/bin:/bin", PYTHONDONTWRITEBYTECODE: "1", TMPDIR: cache, XDG_CACHE_HOME: cache, XDG_CONFIG_HOME: cache, XDG_DATA_HOME: cache, XDG_STATE_HOME: cache },
      maxBuffer: MAX_SIDECAR_BYTES,
      timeout: 10 * 60_000,
    });
    if (result.error != null || result.status !== 0 || result.signal != null) fail("public CoreML semantic validator failed");
    validateValidatorOutput(result.stdout);
    return Object.freeze({
      closure: captured.closure,
      public_commit: captured.publicCommit,
      public_source: PUBLIC_SOURCE,
      public_tree: captured.publicTree,
      validator_path: COREML_REPAIR.validatorRelativePath,
      validator_sha256: COREML_VALIDATOR_CLOSURE.files[0].sha256,
    });
  } finally {
    fs.rmSync(cache, { force: true, recursive: true });
    fs.rmSync(captured.snapshot, { force: true, recursive: true });
  }
}

function evidenceBody(evidence) {
  const body = Buffer.from(`${JSON.stringify(canonicalValue(evidence), null, 2)}\n`, "utf8");
  if (body.length > MAX_EVIDENCE_BYTES) fail("CoreML repair evidence exceeds its bound");
  return body;
}
function fsyncDirectory(directory) {
  const descriptor = fs.openSync(directory, fs.constants.O_RDONLY | (fs.constants.O_DIRECTORY ?? 0));
  try { fs.fsyncSync(descriptor); } finally { fs.closeSync(descriptor); }
}
function evidenceLockPath(file) { return `${path.resolve(file)}.transition.lock`; }

export function writeExclusiveEvidence(file, evidence) {
  const body = evidenceBody(evidence);
  const absolute = path.resolve(file);
  const directory = path.dirname(absolute);
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  const descriptor = fs.openSync(absolute, "wx", 0o600);
  try {
    fs.writeFileSync(descriptor, body);
    fs.fchmodSync(descriptor, 0o600);
    fs.fsyncSync(descriptor);
  } finally { fs.closeSync(descriptor); }
  fsyncDirectory(directory);
}

function readEvidenceRecord(file) {
  const stable = readStableFile(file, "CoreML repair evidence", MAX_EVIDENCE_BYTES);
  let evidence;
  try { evidence = JSON.parse(stable.body.toString("utf8")); } catch { fail("CoreML repair evidence is malformed"); }
  if (!stable.body.equals(evidenceBody(evidence))) fail("CoreML repair evidence is not canonical");
  return { body: stable.body, evidence };
}

function releaseTransitionLock(lock, descriptor) {
  try {
    const opened = fs.fstatSync(descriptor, { bigint: true });
    const current = fs.lstatSync(lock, { bigint: true });
    if (!current.isFile() || current.isSymbolicLink() || current.dev !== opened.dev || current.ino !== opened.ino) {
      fail("CoreML repair evidence transition lock changed unexpectedly");
    }
    fs.closeSync(descriptor);
    descriptor = undefined;
    fs.unlinkSync(lock);
    fsyncDirectory(path.dirname(lock));
  } finally {
    if (descriptor != null) fs.closeSync(descriptor);
  }
}

export function finalizeEvidence(file, previous, next) {
  const absolute = path.resolve(file);
  const directory = path.dirname(absolute);
  const lock = evidenceLockPath(absolute);
  let lockDescriptor;
  try {
    lockDescriptor = fs.openSync(lock, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | (fs.constants.O_NOFOLLOW ?? 0), 0o600);
  } catch (error) {
    if (error?.code === "EEXIST") fail("CoreML repair evidence transition is already in progress");
    throw error;
  }
  try {
    const lockBody = Buffer.from(`${JSON.stringify({ operation: "repair-coreml-semantic-object", pid: process.pid })}\n`, "utf8");
    fs.writeFileSync(lockDescriptor, lockBody);
    fs.fchmodSync(lockDescriptor, 0o600);
    fs.fsyncSync(lockDescriptor);
    const current = readEvidenceRecord(absolute);
    if (!current.body.equals(evidenceBody(previous))) fail("CoreML repair evidence changed before finalization");
    if (next.operation !== "repair-coreml-semantic-object" || next.reservation_id !== previous.reservation_id) {
      fail("CoreML repair evidence transition has a different reservation");
    }
    const temporary = path.join(directory, `.${path.basename(absolute)}.${process.pid}.${crypto.randomBytes(16).toString("hex")}.tmp`);
    let descriptor;
    try {
      descriptor = fs.openSync(temporary, "wx", 0o600);
      fs.writeFileSync(descriptor, evidenceBody(next));
      fs.fchmodSync(descriptor, 0o600);
      fs.fsyncSync(descriptor);
      fs.closeSync(descriptor);
      descriptor = undefined;
      fs.renameSync(temporary, absolute);
      fsyncDirectory(directory);
    } finally {
      if (descriptor != null) fs.closeSync(descriptor);
      try { fs.unlinkSync(temporary); } catch (error) { if (error?.code !== "ENOENT") throw error; }
    }
  } finally {
    releaseTransitionLock(lock, lockDescriptor);
  }
}

function artifactEvidence(artifact, now) {
  return {
    ...artifactIdentity(artifact),
    bucket: COREML_REPAIR.bucket,
    key: COREML_REPAIR.key,
    operation: "repair-coreml-semantic-object",
    reservation_id: crypto.randomBytes(16).toString("hex"),
    reserved_at_utc: now(),
    schema_version: 3,
    state: "reserved",
    updated_at_utc: now(),
  };
}
function transition(evidence, state, now, additional = {}) {
  return { ...evidence, ...additional, state, updated_at_utc: now() };
}

function assertValidatorIdentity(validator) {
  const expected = {
    closure: COREML_VALIDATOR_CLOSURE,
    public_source: PUBLIC_SOURCE,
    validator_path: COREML_REPAIR.validatorRelativePath,
    validator_sha256: COREML_VALIDATOR_CLOSURE.files[0].sha256,
  };
  if (!GIT_OID.test(validator?.public_commit ?? "") || !GIT_OID.test(validator?.public_tree ?? "")
      || !canonicalEqual({ closure: validator?.closure, public_source: validator?.public_source, validator_path: validator?.validator_path, validator_sha256: validator?.validator_sha256 }, expected)) {
    fail("CoreML repair validation handoff closure differs from fixed authority");
  }
}

function assertTrustedHandoff(evidence, artifact, state) {
  const identity = artifactIdentity(artifact);
  const handoff = evidence.validation_handoff;
  if (evidence.schema_version !== 3 || evidence.state !== state || evidence.operation !== "repair-coreml-semantic-object"
      || typeof evidence.reservation_id !== "string" || !/^[0-9a-f]{32}$/u.test(evidence.reservation_id)
      || evidence.bucket !== COREML_REPAIR.bucket || evidence.key !== COREML_REPAIR.key
      || !canonicalEqual({ archive: evidence.archive, embedded_manifest: evidence.embedded_manifest, sidecars: evidence.sidecars }, identity)
      || handoff?.schema_version !== 1 || !canonicalEqual(handoff?.artifact_identity, identity)) {
    fail("CoreML repair validation handoff is not trusted");
  }
  assertValidatorIdentity(handoff.public_validator);
}

function productionDependencies() {
  return {
    createR2Request,
    now: () => new Date().toISOString(),
    putImmutableR2Object,
    readStableFile,
    runPublicValidator,
    sha256,
    spawnSync: childProcess.spawnSync,
    writeExclusiveEvidence,
  };
}

export async function runPreflight(options, supplied = {}) {
  const dependencies = { ...productionDependencies(), ...supplied };
  const artifact = loadArtifact(options.archive, dependencies);
  const reserved = artifactEvidence(artifact, dependencies.now);
  dependencies.writeExclusiveEvidence(options.evidenceOut, reserved);
  let validator;
  try {
    validator = dependencies.runPublicValidator(options.publicCtxRepo, artifact.archive, dependencies);
  } catch (error) {
    const failed = transition(reserved, "failed", dependencies.now, { failure: "public-validator-failed" });
    try { finalizeEvidence(options.evidenceOut, reserved, failed); } catch { /* preserve the durable reservation */ }
    throw error;
  }
  const validated = transition(reserved, "validated", dependencies.now, {
    validation_handoff: { artifact_identity: artifactIdentity(artifact), public_validator: validator, schema_version: 1 },
  });
  finalizeEvidence(options.evidenceOut, reserved, validated);
  return { evidence: validated, status: "validated" };
}

export async function beginCredentials(options, supplied = {}) {
  const dependencies = { ...productionDependencies(), ...supplied };
  const artifact = loadArtifact(options.archive, dependencies);
  const validated = readEvidenceRecord(options.evidenceOut).evidence;
  assertTrustedHandoff(validated, artifact, "validated");
  const credentialFetch = transition(validated, "credential-fetch", dependencies.now);
  finalizeEvidence(options.evidenceOut, validated, credentialFetch);
  return { evidence: credentialFetch, status: "credential-fetch" };
}

export async function credentialFailed(options, supplied = {}) {
  const dependencies = { ...productionDependencies(), ...supplied };
  if (!CREDENTIAL_FAILURES.has(options.failure)) fail("credential failure is not fixed and sanitized");
  const credentialFetch = readEvidenceRecord(options.evidenceOut).evidence;
  if (credentialFetch.schema_version !== 3 || credentialFetch.operation !== "repair-coreml-semantic-object"
      || credentialFetch.state !== "credential-fetch") fail("CoreML repair is not fetching credentials");
  const failed = transition(credentialFetch, "failed", dependencies.now, { failure: options.failure });
  finalizeEvidence(options.evidenceOut, credentialFetch, failed);
  return { evidence: failed, status: "failed" };
}

export async function runPublisher(options, environment = process.env, supplied = {}) {
  const dependencies = { ...productionDependencies(), ...supplied };
  const artifact = loadArtifact(options.archive, dependencies);
  const credentialFetch = readEvidenceRecord(options.evidenceOut).evidence;
  assertTrustedHandoff(credentialFetch, artifact, "credential-fetch");
  if (environment.CTX_RELEASE_R2_BUCKET !== COREML_REPAIR.bucket) {
    const failed = transition(credentialFetch, "failed", dependencies.now, { failure: "bucket-mismatch" });
    try { finalizeEvidence(options.evidenceOut, credentialFetch, failed); } catch { /* preserve credential-fetch on interruption */ }
    fail("CoreML semantic object repair bucket differs from fixed authority");
  }
  const publishing = transition(credentialFetch, "publishing", dependencies.now);
  finalizeEvidence(options.evidenceOut, credentialFetch, publishing);
  let state;
  try {
    const request = dependencies.createR2Request(R2_AUTHORITY, environment);
    state = await dependencies.putImmutableR2Object(request, COREML_REPAIR.bucket, {
      body: artifact.archive.body, contentType: "application/x-xz", key: COREML_REPAIR.key,
    });
  } catch (error) {
    const failed = transition(publishing, "failed", dependencies.now, { failure: "r2-write-or-readback-failed" });
    try { finalizeEvidence(options.evidenceOut, publishing, failed); } catch { /* preserve durable publishing evidence */ }
    throw error;
  }
  const published = transition(publishing, state, dependencies.now);
  finalizeEvidence(options.evidenceOut, publishing, published);
  return { evidence: published, status: "published" };
}

export async function run(argv, environment = process.env, dependencies = {}) {
  const { args, command } = parseArgs(argv);
  if (command === "preflight") return runPreflight({ archive: args.get("--archive"), evidenceOut: args.get("--evidence-out"), publicCtxRepo: args.get("--public-ctx-repo") }, dependencies);
  if (command === "begin-credentials") return beginCredentials({ archive: args.get("--archive"), evidenceOut: args.get("--evidence-out") }, dependencies);
  if (command === "credential-failed") return credentialFailed({ evidenceOut: args.get("--evidence-out"), failure: args.get("--failure") }, dependencies);
  return runPublisher({ archive: args.get("--archive"), evidenceOut: args.get("--evidence-out") }, environment, dependencies);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  run(process.argv.slice(2)).then(
    (result) => process.stdout.write(`${JSON.stringify(result)}\n`),
    (error) => { process.stderr.write(`CoreML semantic object repair failed: ${error.message}\n`); process.exitCode = 1; },
  );
}
