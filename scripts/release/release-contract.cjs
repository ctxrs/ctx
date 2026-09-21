#!/usr/bin/env node
"use strict";

const { assertFrozenBridgePromotion } = require("./frozen-cli-bridge.cjs");

const childProcess = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const http = require("node:http");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const { fileURLToPath } = require("node:url");
const zlib = require("node:zlib");

const ROOT = path.resolve(__dirname, "../..");
const DEFAULT_FUNCTIONS_BASE = "https://cli.ctx.rs/functions/v1";
const DEFAULT_STORAGE_BASE = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/";
const DEFAULT_RELEASES_BASE = "https://cli.ctx.rs/functions/v1/releases";
const semanticContractPath = fs.existsSync(
  path.join(__dirname, "release-contract-semantic.cjs"),
)
  ? path.join(__dirname, "release-contract-semantic.cjs")
  : path.join(process.cwd(), "scripts/release/release-contract-semantic.cjs");
const {
  SEMANTIC_ASSET_LAYOUTS,
  SEMANTIC_AUTHORITY_KEYS,
  SEMANTIC_LAYOUT_BY_KEY,
  canonicalJson,
  decodeCanonicalAuthority,
  parseSemanticAuthorities,
} = require(semanticContractPath).createSemanticContract({
  assertSafeArtifactName,
  assertSha,
  fail,
  layoutPath: path.join(__dirname, "semantic-runtime-layout-v1.json"),
  metadataMatrixKeys,
  requireValue,
});
const candidateManifestContractPath = fs.existsSync(
  path.join(__dirname, "release-candidate-manifest-contract.cjs"),
)
  ? path.join(__dirname, "release-candidate-manifest-contract.cjs")
  : path.join(
    process.cwd(),
    "scripts/release/release-candidate-manifest-contract.cjs",
  );
const {
  CORE_GITHUB_HANDOFF_METADATA_KEY,
  PUBLIC_MANIFEST_AUTHORITY_COMMIT,
  compareCandidateManifestDigestMatrices,
  requireCandidateManifestDigestMatrix,
  verifyCandidateManifestHandoff,
} = require(candidateManifestContractPath);

const MATRIX = [
  { platform: "linux-x64", key: "linux_x64", artifact: "ctx", candidateArtifact: "ctx-linux-x64" },
  { platform: "linux-aarch64", key: "linux_aarch64", artifact: "ctx-linux-aarch64", candidateArtifact: "ctx-linux-aarch64" },
  { platform: "macos-arm64", key: "macos_arm64", artifact: "ctx-macos-arm64", candidateArtifact: "ctx-macos-arm64" },
  { platform: "macos-x64", key: "macos_x64", artifact: "ctx-macos-x64", candidateArtifact: "ctx-macos-x64" },
  { platform: "windows-x64", key: "windows_x64", artifact: "ctx.exe", candidateArtifact: "ctx-windows-x64.exe" },
];
const GITHUB_SEMANTIC_TRANSCODES = [
  { assetId: "linux_x64_cpu", artifact: "ctx-onnxruntime-linux-x64.tar.gz" },
  { assetId: "linux_aarch64_cpu", artifact: "ctx-onnxruntime-linux-aarch64.tar.gz" },
  { assetId: "macos_arm64_cpu", artifact: "ctx-onnxruntime-macos-arm64.tar.gz" },
  { assetId: "macos_x64_cpu", artifact: "ctx-onnxruntime-macos-x64.tar.gz" },
];
const GITHUB_RUNTIME_ARTIFACTS = [
  ...GITHUB_SEMANTIC_TRANSCODES.map((entry) => entry.artifact),
  "ctx-onnxruntime-windows-x64.zip",
];
// The public staging helper's default manifest is the five CLIs, each CLI's
// CycloneDX/notices pair, and the five public ONNX Runtime transports.
const GITHUB_CANDIDATE_ARTIFACTS = [
  ...MATRIX.flatMap((entry) => [
    entry.candidateArtifact,
    `${entry.candidateArtifact}.cdx.json`,
    `${entry.candidateArtifact}.third-party-notices.txt`,
  ]),
  ...GITHUB_RUNTIME_ARTIFACTS,
];
const MATRIX_KEYS = new Set(MATRIX.map((entry) => entry.key));
const MATRIX_PLATFORM_NAMES = new Map(MATRIX.map((entry) => [entry.key, entry.platform]));
const managedPairContractPath = fs.existsSync(
  path.join(__dirname, "release-contract-managed-pair.cjs"),
)
  ? path.join(__dirname, "release-contract-managed-pair.cjs")
  : path.join(process.cwd(), "scripts/release/release-contract-managed-pair.cjs");
const {
  RUNTIME_TRANSPORTS,
  validateManagedPairMatrix,
  validateSupplementaryProfile,
} = require(managedPairContractPath).createManagedPairContract({
  MATRIX,
  MATRIX_KEYS,
  assertSha,
  fail,
  metadataMatrixKeys,
  parseSemanticAuthorities,
  requireCandidateManifestDigestMatrix,
  requireValue,
});

function fail(message) {
  throw new Error(message);
}

function env(name, fallback = "") {
  const value = process.env[name];
  return value == null || value === "" ? fallback : value;
}

function sha256(buffer) {
  return crypto.createHash("sha256").update(buffer).digest("hex");
}

function parseCandidateManifest(file) {
  let bytes;
  try {
    bytes = fs.readFileSync(file);
  } catch (error) {
    fail(`could not read candidate manifest ${file}: ${error.message}`);
  }
  const entries = new Map();
  const lines = bytes.toString("utf8").split(/\r?\n/);
  for (const [index, line] of lines.entries()) {
    if (line === "" && index === lines.length - 1) {
      continue;
    }
    const match = line.match(/^([0-9a-f]{64}) {2}([^/\\\s]+)$/);
    if (!match) {
      fail(`candidate manifest line ${index + 1} is not lowercase SHA256SUMS format`);
    }
    if (entries.has(match[2])) {
      fail(`candidate manifest repeats artifact ${match[2]}`);
    }
    entries.set(match[2], match[1]);
  }
  const expected = [...GITHUB_CANDIDATE_ARTIFACTS].sort();
  const actual = [...entries.keys()].sort();
  if (actual.join("\n") !== expected.join("\n")) {
    fail(`candidate manifest artifacts must be exactly: ${expected.join(", ")}`);
  }
  return {
    entries,
    sha256: sha256(bytes),
    directory: path.dirname(path.resolve(file)),
  };
}

function verifyCandidateArtifactChecksums(manifest) {
  for (const artifact of GITHUB_CANDIDATE_ARTIFACTS) {
    const candidatePath = path.join(manifest.directory, artifact);
    let candidateBytes;
    try {
      candidateBytes = fs.readFileSync(candidatePath);
    } catch (error) {
      fail(`could not read candidate artifact ${candidatePath}: ${error.message}`);
    }
    if (sha256(candidateBytes) !== manifest.entries.get(artifact)) {
      fail(`candidate artifact does not match manifest digest: ${artifact}`);
    }
  }
}

function compareCandidateManifest(values, manifest) {
  for (const entry of MATRIX) {
    const hostedSha = values[`CTX_RELEASE_SHA256_${entry.key}`].toLowerCase();
    const candidateSha = manifest.entries.get(entry.candidateArtifact);
    if (candidateSha !== hostedSha) {
      fail(`hosted digest for ${entry.platform} does not match candidate manifest artifact ${entry.candidateArtifact}`);
    }
  }
  for (const entry of RUNTIME_TRANSPORTS) {
    const hostedSha = values[`CTX_RELEASE_ONNXRUNTIME_SHA256_${entry.key}`].toLowerCase();
    const candidateSha = manifest.entries.get(entry.artifact);
    if (candidateSha !== hostedSha) {
      fail(
        `hosted ONNX Runtime digest for ${MATRIX_PLATFORM_NAMES.get(entry.key)} `
        + `does not match candidate manifest artifact ${entry.artifact}`,
      );
    }
  }
  verifyCandidateArtifactChecksums(manifest);
  const semantic = parseSemanticAuthorities(values, "stable");
  for (const entry of GITHUB_SEMANTIC_TRANSCODES) {
    const asset = semantic.assets.get(entry.assetId);
    const candidatePath = path.join(manifest.directory, entry.artifact);
    const candidateFiles = inspectSemanticTranscode(asset, candidatePath);
    if (canonicalJson(candidateFiles) !== canonicalJson(asset.files)) {
      fail(
        `hosted semantic file identity does not match GitHub transcode ${entry.artifact}`,
      );
    }
  }
}

function run(command, args, options = {}) {
  return childProcess.execFileSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  }).trim();
}

function git(repo, args) {
  return run("git", ["-C", repo, ...args]);
}

function publicRepo() {
  const configured = env("CTX_PUBLIC_CTX_REPO", ROOT);
  if (!configured) {
    fail("set CTX_PUBLIC_CTX_REPO to the public ctx checkout under release review");
  }
  const repo = path.resolve(configured);
  if (!fs.existsSync(path.join(repo, "Cargo.toml"))) {
    fail(`CTX_PUBLIC_CTX_REPO does not point at a public ctx checkout: ${repo}`);
  }
  return repo;
}

function parseCargoVersion(repo) {
  return require("./release-version.cjs").readCargoVersion(repo);
}

function assertPublicSource(repo, expectedCommit) {
  const actualCommit = git(repo, ["rev-parse", "HEAD"]);
  if (actualCommit !== expectedCommit) {
    fail(`public ctx checkout is at ${actualCommit}, expected ${expectedCommit}`);
  }

  if (env("CTX_PUBLIC_RELEASE_SKIP_WORKTREE_CHECK") !== "1") {
    const status = git(repo, ["status", "--porcelain"]);
    if (status !== "") {
      fail("public ctx checkout must be clean before release promotion");
    }
  }

  if (env("CTX_PUBLIC_RELEASE_SKIP_REMOTE_CHECK") !== "1") {
    const remoteMain = run("git", ["ls-remote", "origin", "refs/heads/main"], { cwd: repo })
      .split(/\s+/)[0];
    if (!remoteMain) {
      fail("could not resolve origin/main for public ctx");
    }
    if (remoteMain !== expectedCommit) {
      fail(`origin/main is ${remoteMain}, expected ${expectedCommit}`);
    }
  }

  return actualCommit;
}

function loadHostedInstallerPublicKeyPem() {
  const installer = fs.readFileSync(path.join(ROOT, "services/install-site/src/cli-install-script.js"), "utf8");
  const match = installer.match(/DEFAULT_METADATA_PUBLIC_KEY_PEM\s*=\s*`([\s\S]*?)`;/);
  if (!match) {
    fail("could not locate hosted CLI metadata public key in services/install-site/src/cli-install-script.js");
  }
  return match[1].trim();
}

function loadMetadataVerificationPublicKeyPem(hostedInstallerPublicKeyPem) {
  return hostedInstallerPublicKeyPem;
}

function publicKeyJwk(publicKeyPem, label) {
  try {
    const jwk = crypto.createPublicKey(publicKeyPem).export({ format: "jwk" });
    if (jwk.kty !== "RSA" || !jwk.n || !jwk.e) {
      fail(`${label} metadata public key is not an RSA public key`);
    }
    return jwk;
  } catch (error) {
    fail(`could not parse ${label} metadata public key: ${error.message}`);
  }
}

function assertJwkMatchesPem(actualJwk, expectedPem, actualLabel, expectedLabel) {
  const expectedJwk = publicKeyJwk(expectedPem, expectedLabel);
  if (actualJwk.n !== expectedJwk.n || actualJwk.e !== expectedJwk.e) {
    fail(`${actualLabel} metadata public key does not match ${expectedLabel} metadata public key`);
  }
}

function assertPemKeysMatch(actualPem, expectedPem, actualLabel, expectedLabel) {
  assertJwkMatchesPem(publicKeyJwk(actualPem, actualLabel), expectedPem, actualLabel, expectedLabel);
}

function assertPowerShellInstallerKeyMatches(publicKeyPem) {
  const powershellInstaller = fs.readFileSync(path.join(ROOT, "services/install-site/src/cli-install-powershell-script.js"), "utf8");
  const modulusMatch = powershellInstaller.match(/DEFAULT_METADATA_PUBLIC_KEY_MODULUS_BASE64URL\s*=\s*"([^"]+)"/);
  const exponentMatch = powershellInstaller.match(/DEFAULT_METADATA_PUBLIC_KEY_EXPONENT_BASE64URL\s*=\s*"([^"]+)"/);
  if (!modulusMatch || !exponentMatch) {
    fail("could not locate hosted CLI metadata public key in services/install-site/src/cli-install-powershell-script.js");
  }
  assertJwkMatchesPem(
    { kty: "RSA", n: modulusMatch[1], e: exponentMatch[1] },
    publicKeyPem,
    "hosted PowerShell installer",
    "hosted Unix installer",
  );
}

function loadPublicCliMetadataPublicKeyPem(repo) {
  const candidatePaths = [
    path.join(repo, "crates/ctx-upgrade-engine/src/upgrade/metadata.rs"),
    path.join(repo, "crates/ctx-upgrade-engine/src/upgrade/metadata.rs"),
    path.join(repo, "crates/ctx-cli/src/upgrade/metadata.rs"),
    path.join(repo, "crates/ctx-cli/src/upgrade.rs"),
  ];
  const upgradeSourcePath = candidatePaths.find((candidate) => fs.existsSync(candidate));
  if (!upgradeSourcePath) {
    fail(`could not locate public CLI runtime metadata public key source under ${repo}/crates`);
  }
  const upgradeSource = fs.readFileSync(upgradeSourcePath, "utf8");
  const match = upgradeSource.match(
    /(?:DEFAULT|RELEASE)_METADATA_PUBLIC_KEY_PEM\s*:\s*&str\s*=\s*r#?"([\s\S]*?)"#?;/,
  );
  if (!match) {
    fail(`could not locate public CLI runtime metadata public key in ${upgradeSourcePath}`);
  }
  return match[1].trim();
}

function fetchHttp(url, redirectCount = 0) {
  if (redirectCount > 5) {
    return Promise.reject(new Error(`too many redirects while fetching ${url}`));
  }
  const parsed = new URL(url);
  if (parsed.protocol === "http:" && env("CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL") !== "1") {
    return Promise.reject(new Error(`refusing non-HTTPS URL without CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL=1: ${url}`));
  }
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
    return Promise.reject(new Error(`unsupported URL protocol for ${url}`));
  }
  const client = parsed.protocol === "https:" ? https : http;
  return new Promise((resolve, reject) => {
    const request = client.get(url, (response) => {
      const status = response.statusCode || 0;
      const location = response.headers.location;
      if (status >= 300 && status < 400 && location) {
        response.resume();
        resolve(fetchHttp(new URL(location, url).toString(), redirectCount + 1));
        return;
      }
      if (status < 200 || status >= 300) {
        response.resume();
        reject(new Error(`GET ${url} returned HTTP ${status}`));
        return;
      }
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve(Buffer.concat(chunks)));
    });
    request.on("error", reject);
    request.setTimeout(30_000, () => {
      request.destroy(new Error(`GET ${url} timed out`));
    });
  });
}

async function fetchBytes(url) {
  const parsed = new URL(url);
  if (parsed.protocol === "file:") {
    try {
      return fs.readFileSync(fileURLToPath(parsed));
    } catch (error) {
      fail(`could not read file URL ${url}: ${error.message}`);
    }
  }
  if (parsed.protocol === "https:" || parsed.protocol === "http:") {
    return fetchHttp(url);
  }
  fail(`unsupported URL protocol for ${url}`);
}

function parseEnvMetadata(buffer, label) {
  const values = {};
  const lines = buffer.toString("utf8").split(/\r?\n/);
  for (const [index, rawLine] of lines.entries()) {
    const line = rawLine.endsWith("\r") ? rawLine.slice(0, -1) : rawLine;
    if (line.trim() === "" || /^[ \t]*#/.test(line)) {
      continue;
    }
    const equals = line.indexOf("=");
    if (equals <= 0) {
      fail(`${label} metadata line ${index + 1} is not KEY=value`);
    }
    const key = line.slice(0, equals);
    const value = line.slice(equals + 1);
    if (key !== key.trim() || value !== value.trim()) {
      fail(`${label} metadata line ${index + 1} has unsupported surrounding whitespace`);
    }
    if (!/^[A-Za-z0-9_]+$/.test(key)) {
      fail(`${label} metadata key is invalid: ${key}`);
    }
    if (Object.hasOwn(values, key)) {
      fail(`${label} metadata repeats key: ${key}`);
    }
    values[key] = value;
  }
  return values;
}

function verifySignature(metadataBytes, signatureBytes, publicKeyPem, label) {
  let decoded;
  try {
    decoded = Buffer.from(signatureBytes.toString("utf8").trim(), "base64");
  } catch (error) {
    fail(`${label} metadata signature is not base64: ${error.message}`);
  }
  if (decoded.length === 0) {
    fail(`${label} metadata signature is empty`);
  }
  const ok = crypto.verify(
    "RSA-SHA256",
    metadataBytes,
    { key: publicKeyPem, padding: crypto.constants.RSA_PKCS1_PADDING },
    decoded,
  );
  if (!ok) {
    fail(`${label} metadata signature verification failed`);
  }
}

async function loadSignedMetadata(url, label, publicKeyPem) {
  const signatureUrl = env(
    label === "stable"
      ? "CTX_PUBLIC_RELEASE_STABLE_METADATA_SIGNATURE_URL"
      : "CTX_PUBLIC_RELEASE_VERSIONED_METADATA_SIGNATURE_URL",
    `${url}.sig`,
  );
  const [metadataBytes, signatureBytes] = await Promise.all([
    fetchBytes(url),
    fetchBytes(signatureUrl),
  ]);
  verifySignature(metadataBytes, signatureBytes, publicKeyPem, label);
  return {
    label,
    url,
    signature_url: signatureUrl,
    sha256: sha256(metadataBytes),
    signature_sha256: sha256(signatureBytes),
    values: parseEnvMetadata(metadataBytes, label),
  };
}

function requireValue(values, key, label) {
  if (!Object.hasOwn(values, key) || values[key] === "") {
    fail(`${label} metadata missing ${key}`);
  }
  return values[key];
}

function requireTrueFlag(values, key, label) {
  const value = requireValue(values, key, label);
  if (value !== "true" && value !== "false") {
    fail(`${label} metadata ${key} must be true or false`);
  }
  if (value !== "true") {
    fail(`${label} metadata ${key} must be true`);
  }
}

function assertSha(value, field) {
  if (!/^[0-9a-fA-F]{64}$/.test(value)) {
    fail(`${field} is not a SHA-256 hex digest`);
  }
  if (/^0{64}$/.test(value)) {
    fail(`${field} is a placeholder digest`);
  }
}

function assertSafeArtifactName(value, field) {
  if (value.includes("/") || value.includes("\\") || value.includes("..") || value.trim() === "") {
    fail(`${field} is not a safe artifact name: ${value}`);
  }
}

function metadataMatrixKeys(values, prefix) {
  return Object.keys(values)
    .filter((key) => key.startsWith(prefix))
    .map((key) => key.slice(prefix.length))
    .sort();
}

function assertExactMatrix(values, label, artifactPrefix, shaPrefix, matrixLabel) {
  const expected = [...MATRIX_KEYS].sort();
  for (const prefix of [artifactPrefix, shaPrefix]) {
    const actual = metadataMatrixKeys(values, prefix);
    const missing = expected.filter((key) => !actual.includes(key));
    const unexpected = actual.filter((key) => !MATRIX_KEYS.has(key));
    if (missing.length > 0 || unexpected.length > 0) {
      const details = [];
      if (missing.length > 0) {
        details.push(`missing ${missing.map((key) => MATRIX_PLATFORM_NAMES.get(key) || key).join(", ")}`);
      }
      if (unexpected.length > 0) {
        details.push(`unexpected ${unexpected.join(", ")}`);
      }
      fail(`${label} metadata has wrong ${matrixLabel} matrix for ${prefix}: ${details.join("; ")}`);
    }
  }

  const artifactKeys = metadataMatrixKeys(values, artifactPrefix);
  const shaKeys = metadataMatrixKeys(values, shaPrefix);
  if (artifactKeys.join(" ") !== shaKeys.join(" ")) {
    fail(
      `${label} metadata artifact and checksum platform keys differ: artifacts ${artifactKeys.join(" ") || "<none>"}; checksums ${shaKeys.join(" ") || "<none>"}`,
    );
  }
}

function validateMetadata(metadata, expected) {
  const { values, label } = metadata;
  const schema = requireValue(values, "CTX_RELEASE_SCHEMA_VERSION", label);
  if (schema !== "1") {
    fail(`${label} metadata has unsupported schema: ${schema}`);
  }
  const version = requireValue(values, "CTX_RELEASE_VERSION", label);
  if (version !== expected.version) {
    fail(`${label} metadata version is ${version}, expected ${expected.version}`);
  }
  const channel = values.CTX_RELEASE_CHANNEL || expected.channel;
  if (channel !== expected.channel) {
    fail(`${label} metadata channel is ${channel}, expected ${expected.channel}`);
  }
  const sourceCommit = requireValue(values, "CTX_RELEASE_SOURCE_COMMIT", label);
  if (sourceCommit !== expected.sourceCommit) {
    fail(`${label} metadata source commit is ${sourceCommit}, expected ${expected.sourceCommit}`);
  }
  requireTrueFlag(values, "CTX_RELEASE_SELF_UPGRADE_ALLOWED", label);
  requireTrueFlag(values, "CTX_RELEASE_AUTO_UPGRADE_ALLOWED", label);
  const baseUrl = requireValue(values, "CTX_RELEASE_BASE_URL", label);
  if (!baseUrl.startsWith("https://") && env("CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL") !== "1") {
    fail(`${label} metadata base URL must be HTTPS`);
  }
  if (!baseUrl.startsWith(DEFAULT_STORAGE_BASE) && env("CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL") !== "1") {
    fail(`${label} metadata base URL must be under ${DEFAULT_STORAGE_BASE}`);
  }
  const expectedBaseUrl = `${DEFAULT_STORAGE_BASE}${expected.channel}/${expected.version}`;
  if (
    env("CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL") !== "1" &&
    baseUrl.replace(/\/+$/, "") !== expectedBaseUrl
  ) {
    fail(`${label} metadata base URL is ${baseUrl}, expected ${expectedBaseUrl}`);
  }

  assertExactMatrix(values, label, "CTX_RELEASE_ARTIFACT_", "CTX_RELEASE_SHA256_", "hosted CLI");
  validateManagedPairMatrix(values, label);
  const supplementaryProfile = validateSupplementaryProfile(values, label);
  for (const entry of MATRIX) {
    const artifact = requireValue(values, `CTX_RELEASE_ARTIFACT_${entry.key}`, label);
    const checksum = requireValue(values, `CTX_RELEASE_SHA256_${entry.key}`, label);
    if (artifact !== entry.artifact) {
      fail(`${label} metadata artifact for ${entry.platform} is ${artifact}, expected ${entry.artifact}`);
    }
    assertSafeArtifactName(artifact, `${label} artifact for ${entry.platform}`);
    assertSha(checksum, `${label} checksum for ${entry.platform}`);
  }
  return supplementaryProfile;
}

function compareStableAndVersioned(stable, versioned) {
  const sharedKeys = [
    "CTX_RELEASE_SCHEMA_VERSION",
    "CTX_RELEASE_CHANNEL",
    "CTX_RELEASE_VERSION",
    "CTX_RELEASE_BASE_URL",
    "CTX_RELEASE_SOURCE_COMMIT",
    "CTX_RELEASE_SELF_UPGRADE_ALLOWED",
    "CTX_RELEASE_AUTO_UPGRADE_ALLOWED",
  ];
  sharedKeys.push(
    "CTX_RELEASE_ONNXRUNTIME_VERSION",
    CORE_GITHUB_HANDOFF_METADATA_KEY,
    "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION",
    "CTX_RELEASE_SEMANTIC_ASSETS",
  );
  for (const key of sharedKeys) {
    if ((stable.values[key] || "") !== (versioned.values[key] || "")) {
      fail(`stable metadata ${key} does not match versioned metadata`);
    }
  }
  for (const entry of MATRIX) {
    for (const prefix of ["CTX_RELEASE_ARTIFACT_", "CTX_RELEASE_SHA256_"]) {
      const key = `${prefix}${entry.key}`;
      if (stable.values[key] !== versioned.values[key]) {
        fail(`stable metadata ${key} does not match versioned metadata`);
      }
    }
    for (const field of [
      "ENVELOPE",
      "CORE_OBJECT",
      "CORE_SHA256",
      "COMPANION_OBJECT",
      "COMPANION_SHA256",
    ]) {
      const key = `CTX_RELEASE_MANAGED_PAIR_${field}_${entry.key}`;
      if (stable.values[key] !== versioned.values[key]) {
        fail(`stable metadata ${key} does not match versioned metadata`);
      }
    }
  }
  compareCandidateManifestDigestMatrices(stable.values, versioned.values);
  for (const key of SEMANTIC_AUTHORITY_KEYS) {
    const field = `CTX_RELEASE_SEMANTIC_AUTHORITY_${key}`;
    if (stable.values[field] !== versioned.values[field]) {
      fail(`stable metadata ${field} does not match versioned metadata`);
    }
  }
  for (const entry of RUNTIME_TRANSPORTS) {
    for (const prefix of [
      "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_",
      "CTX_RELEASE_ONNXRUNTIME_SHA256_",
    ]) {
      const key = `${prefix}${entry.key}`;
      if (stable.values[key] !== versioned.values[key]) {
        fail(`stable metadata ${key} does not match versioned metadata`);
      }
    }
  }
}

function artifactUrl(baseUrl, artifact) {
  const normalized = baseUrl.endsWith("/") ? baseUrl : `${baseUrl}/`;
  return new URL(artifact, normalized).toString();
}

function canRunLinuxArtifact(platform) {
  if (process.platform !== "linux") {
    return false;
  }
  if (platform === "linux-x64") {
    return process.arch === "x64";
  }
  if (platform === "linux-aarch64") {
    return process.arch === "arm64";
  }
  return false;
}

function runLinuxVersionCheck(entry, bytes, expectedVersion) {
  if (!canRunLinuxArtifact(entry.platform)) {
    return { status: "skipped", reason: `${process.platform}-${process.arch}` };
  }
  if (env("CTX_PUBLIC_RELEASE_SKIP_EXECUTE_LINUX") === "1") {
    return { status: "skipped", reason: "CTX_PUBLIC_RELEASE_SKIP_EXECUTE_LINUX=1" };
  }
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-public-cli-release-"));
  const binaryPath = path.join(tempDir, "ctx");
  try {
    fs.writeFileSync(binaryPath, bytes, { mode: 0o755 });
    const result = childProcess.spawnSync(binaryPath, ["--version"], {
      encoding: "utf8",
      env: { ...process.env, HOME: tempDir },
      timeout: 10_000,
    });
    if (result.status !== 0) {
      fail(`${entry.platform} artifact --version failed: ${result.stderr || result.stdout}`);
    }
    const stdout = result.stdout.trim();
    const expected = `ctx ${expectedVersion}`;
    if (stdout !== expected) {
      fail(`${entry.platform} artifact --version reported ${stdout}, expected ${expected}`);
    }
    return { status: "passed", stdout };
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}

async function verifyGzipTransport(baseUrl, entry, artifact, expectedSha, expectedBytes) {
  const gzipArtifact = `${artifact}.gz`;
  const gzipUrl = artifactUrl(baseUrl, gzipArtifact);
  const gzipBytes = await fetchBytes(gzipUrl);
  let restored;
  try {
    restored = zlib.gunzipSync(gzipBytes);
  } catch (error) {
    fail(`live gzip transport is invalid for ${entry.platform}: ${error.message}`);
  }
  const restoredSha = sha256(restored);
  if (restoredSha !== expectedSha) {
    fail(`live gzip transport checksum mismatch for ${entry.platform}: expected ${expectedSha}, got ${restoredSha}`);
  }
  if (!restored.equals(expectedBytes)) {
    fail(`live gzip transport bytes differ from raw artifact for ${entry.platform}`);
  }
  return {
    artifact: gzipArtifact,
    sha256: sha256(gzipBytes),
    byte_length: gzipBytes.length,
    uncompressed_byte_length: restored.length,
  };
}

function inspectSemanticArchive(asset, bytes) {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-semantic-runtime-readback-"));
  const archivePath = path.join(tempDir, asset.artifact);
  try {
    fs.writeFileSync(archivePath, bytes, { mode: 0o600 });
    const result = childProcess.spawnSync(
      "python3",
      [
        path.join(__dirname, "semantic_runtime_metadata.py"),
        "inspect",
        "--artifact",
        asset.artifact,
        "--archive",
        archivePath,
      ],
      {
        encoding: "utf8",
        timeout: 120_000,
      },
    );
    if (result.error) {
      fail(`could not inspect live semantic artifact ${asset.artifact}: ${result.error.message}`);
    }
    if (result.status !== 0) {
      fail(
        `could not inspect live semantic artifact ${asset.artifact}: `
        + `${(result.stderr || result.stdout).trim()}`,
      );
    }
    return result.stdout.trim();
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}

function inspectSemanticTranscode(asset, archivePath) {
  const result = childProcess.spawnSync(
    "python3",
    [
      path.join(__dirname, "semantic_runtime_metadata.py"),
      "inspect-github-transcode",
      "--artifact",
      asset.artifact,
      "--archive",
      archivePath,
    ],
    {
      encoding: "utf8",
      timeout: 120_000,
    },
  );
  if (result.error) {
    fail(
      `could not inspect candidate semantic transcode ${archivePath}: `
      + result.error.message,
    );
  }
  if (result.status !== 0) {
    fail(
      `could not inspect candidate semantic transcode ${archivePath}: `
      + `${(result.stderr || result.stdout).trim()}`,
    );
  }
  return decodeCanonicalAuthority(
    result.stdout.trim(),
    `candidate semantic transcode ${archivePath}`,
  );
}

async function verifyLiveArtifacts(values, expectedVersion) {
  const baseUrl = requireValue(values, "CTX_RELEASE_BASE_URL", "stable");
  const artifacts = [];
  const semantic = parseSemanticAuthorities(values, "stable");
  const { authorities } = semantic;
  const semanticAssets = [...semantic.assets.entries()];
  const hostedEntries = [
    ...MATRIX.map((entry) => ({
      component: "cli",
      entry,
      artifact: values[`CTX_RELEASE_ARTIFACT_${entry.key}`],
      expectedSha: values[`CTX_RELEASE_SHA256_${entry.key}`].toLowerCase(),
    })),
    ...RUNTIME_TRANSPORTS.map((entry) => ({
      component: "onnxruntime-compatibility",
      artifact: values[`CTX_RELEASE_ONNXRUNTIME_ARTIFACT_${entry.key}`],
      expectedSha: values[`CTX_RELEASE_ONNXRUNTIME_SHA256_${entry.key}`].toLowerCase(),
    })),
    ...semanticAssets.map(([assetId, asset]) => ({
      component: `semantic-${asset.role}`,
      assetId,
      asset,
      artifact: asset.artifact,
      expectedSha: asset.archive_sha256,
    })),
  ];
  for (const hosted of hostedEntries) {
    const { component, entry, assetId, asset, artifact, expectedSha } = hosted;
    const url = artifactUrl(baseUrl, artifact);
    const bytes = await fetchBytes(url);
    const liveSha = sha256(bytes);
    if (liveSha !== expectedSha) {
      const identity = entry?.platform || artifact;
      fail(`live ${component} artifact checksum mismatch for ${identity}: expected ${expectedSha}, got ${liveSha}`);
    }
    const result = {
      component,
      artifact,
      sha256: liveSha,
      byte_length: bytes.length,
    };
    if (entry) {
      result.platform = entry.platform;
    }
    if (component === "cli" && entry.platform.startsWith("linux-")) {
      result.version_check = runLinuxVersionCheck(entry, bytes, expectedVersion);
    }
    if (component === "cli") {
      result.gzip_transport = await verifyGzipTransport(baseUrl, entry, artifact, expectedSha, bytes);
    }
    if (asset) {
      const inspected = inspectSemanticArchive(asset, bytes);
      const expectedRecord = Buffer.from(canonicalJson(asset), "utf8").toString("base64");
      if (inspected !== expectedRecord) {
        fail(`live semantic file manifest mismatch for ${artifact}`);
      }
      result.target_backends = [...authorities.values()]
        .filter((authority) => authority.asset_ids.includes(assetId))
        .map((authority) => `${authority.target}/${authority.backend}`);
      result.files = asset.files;
    }
    artifacts.push(result);
  }
  return artifacts;
}

function defaultStableMetadataUrl(functionsBase, channel) {
  const base = channel === "stable" ? functionsBase.replace(/\/v1$/u, "/v2") : functionsBase;
  return `${base}/releases/${channel}/ctx-release-metadata.env`;
}

function assertProductionMetadataUrl(label, url, expectedUrl) {
  if (env("CTX_PUBLIC_RELEASE_ALLOW_CUSTOM_BASE_URL") === "1") {
    return;
  }
  const normalized = url.replace(/\/+$/, "");
  if (normalized !== expectedUrl) {
    fail(`${label} metadata URL is ${url}, expected ${expectedUrl}`);
  }
}

function writeEvidence(evidence) {
  const outputPath = env(
    "CTX_PUBLIC_RELEASE_EVIDENCE_PATH",
    path.join(ROOT, "target/ctx-artifacts/public-cli-release-contract/public-cli-release-contract.json"),
  );
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, `${JSON.stringify(evidence, null, 2)}\n`);
  return outputPath;
}

async function main() {
  const repo = publicRepo();
  const version = env("CTX_PUBLIC_RELEASE_VERSION") || parseCargoVersion(repo);
  const sourceCommit = env("CTX_PUBLIC_RELEASE_SOURCE_COMMIT") || git(repo, ["rev-parse", "HEAD"]);
  const channel = env("CTX_PUBLIC_RELEASE_CHANNEL", "stable");
  const functionsBase = env("CTX_PUBLIC_RELEASE_FUNCTIONS_BASE", DEFAULT_FUNCTIONS_BASE).replace(/\/+$/, "");
  const stableUrl = env("CTX_PUBLIC_RELEASE_STABLE_METADATA_URL", defaultStableMetadataUrl(functionsBase, channel));
  const versionedUrl = env(
    "CTX_PUBLIC_RELEASE_VERSIONED_METADATA_URL",
    `${functionsBase}/releases/${channel}/${version}/ctx-release-metadata.env`,
  );
  const hostedInstallerPublicKeyPem = loadHostedInstallerPublicKeyPem();
  assertPowerShellInstallerKeyMatches(hostedInstallerPublicKeyPem);
  const publicCliPublicKeyPem = loadPublicCliMetadataPublicKeyPem(repo);
  assertPemKeysMatch(publicCliPublicKeyPem, hostedInstallerPublicKeyPem, "public CLI runtime", "hosted Unix installer");
  const publicKeyPem = loadMetadataVerificationPublicKeyPem(hostedInstallerPublicKeyPem);
  assertProductionMetadataUrl("stable", stableUrl, defaultStableMetadataUrl(DEFAULT_FUNCTIONS_BASE, channel));
  assertProductionMetadataUrl(
    "versioned",
    versionedUrl,
    `${DEFAULT_RELEASES_BASE}/${channel}/${version}/ctx-release-metadata.env`,
  );

  assertPublicSource(repo, sourceCommit);
  const [stable, versioned] = await Promise.all([
    loadSignedMetadata(stableUrl, "stable", publicKeyPem),
    loadSignedMetadata(versionedUrl, "versioned", publicKeyPem),
  ]);

  const expected = { version, sourceCommit, channel };
  const stableProfile = validateMetadata(stable, expected);
  const versionedProfile = validateMetadata(versioned, expected);
  if (stableProfile !== versionedProfile) {
    fail("stable and versioned metadata select different supplementary profiles");
  }
  compareStableAndVersioned(stable, versioned);
  const frozenBridge = channel === "stable" ? await assertFrozenBridgePromotion(version) : null;
  const candidateManifestHandoff = env("CTX_PUBLIC_RELEASE_CANDIDATE_MANIFEST");
  if (!candidateManifestHandoff) {
    fail(
      "set CTX_PUBLIC_RELEASE_CANDIDATE_MANIFEST to the staged candidate manifest authority handoff",
    );
  }
  const candidateManifestDigests = verifyCandidateManifestHandoff({
    values: stable.values,
    label: "signed stable",
    handoffDir: candidateManifestHandoff,
    publicRepo: repo,
    python: env("CTX_PUBLIC_RELEASE_PYTHON", "python3"),
    environment: process.env,
  });
  const releaseSumsPath = env("CTX_PUBLIC_RELEASE_SHA256SUMS");
  if (!releaseSumsPath) {
    fail("set CTX_PUBLIC_RELEASE_SHA256SUMS to the staged GitHub release SHA256SUMS");
  }
  const releaseSums = parseCandidateManifest(releaseSumsPath);
  compareCandidateManifest(stable.values, releaseSums);
  const artifacts = await verifyLiveArtifacts(stable.values, version);

  const evidence = {
    schema_version: 1,
    kind: "public-cli-release-contract",
    status: "passed",
    checked_at: new Date().toISOString(),
    release: {
      channel,
      version,
      source_commit: sourceCommit,
    },
    public_source: {
      repo: "ctxrs/ctx",
      commit: sourceCommit,
      worktree_clean_checked: env("CTX_PUBLIC_RELEASE_SKIP_WORKTREE_CHECK") !== "1",
      remote_main_checked: env("CTX_PUBLIC_RELEASE_SKIP_REMOTE_CHECK") !== "1",
    },
    metadata: {
      frozen_bridge: frozenBridge,
      supplementary_profile: stableProfile,
      // Project already validated signed pair identities for the live installer
      // consumer. This does not independently discover or authenticate a feed.
      managed_pair: Object.fromEntries(MATRIX.map((entry) => [entry.platform, {
        core_sha256: stable.values[`CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_${entry.key}`],
        pro_sha256: stable.values[`CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_${entry.key}`],
      }])),
      stable: {
        url: stable.url,
        sha256: stable.sha256,
        signature_sha256: stable.signature_sha256,
        signature_verified: true,
      },
      versioned: {
        url: versioned.url,
        sha256: versioned.sha256,
        signature_sha256: versioned.signature_sha256,
        signature_verified: true,
      },
    },
    candidate_manifests: {
      core_github_handoff_sha256:
        stable.values[CORE_GITHUB_HANDOFF_METADATA_KEY],
      digests: candidateManifestDigests,
      public_manifest_authority_commit: PUBLIC_MANIFEST_AUTHORITY_COMMIT,
      windows_public_verifier: "scripts/release-sbom.py verify-release",
    },
    github_release_sha256s: {
      sha256: releaseSums.sha256,
    },
    hosted_matrix: artifacts,
    validation: {
      construction: JSON.parse(fs.readFileSync(path.join(candidateManifestHandoff, "release-validation.json"), "utf8")),
      publication_readback: "passed",
      installer_native_execution: "not_run",
      stock_1_4_upgrade: "not_run",
    },
  };

  const evidencePath = writeEvidence(evidence);
  console.log(`public ctx release contract ok: ${evidencePath}`);
}

main().catch((error) => {
  console.error(`public ctx release contract failed: ${error.message}`);
  process.exit(1);
});
