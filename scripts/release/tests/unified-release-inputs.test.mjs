import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { materializeReleaseTestSources } from "./current_runtime_fixture.mjs";

const testSourceUrl = materializeReleaseTestSources(import.meta.url);
const { loadTargetMatrix, sha256, targetManifest, validateTargetManifest,
  verifyEnvelope } = await import(new URL("../managed-pair-release-contract.mjs", testSourceUrl));
const { loadReleaseAuthorities } = await import(new URL("../release-authority.mjs", testSourceUrl));
const { projectUnifiedReleaseInputs } = await import(new URL("../unified-release-inputs.mjs", testSourceUrl));

const fixtures = fileURLToPath(new URL("./fixtures/unified/", testSourceUrl));
const matrix = loadTargetMatrix(fileURLToPath(new URL("../../../contracts/release-targets-v1.json", testSourceUrl)));
const authority = loadReleaseAuthorities({ registryPath: path.join(fixtures, "authority.json") }).get("stable");
const source = "1".repeat(40);
const candidate = { channel: "stable", release_name: "v1.5.0", rollback_generation: 27 };

function fixtureInputs() {
  const artifact = path.join(fixtures, "artifact.txt");
  const bytes = fs.readFileSync(artifact);
  return { candidate, matrix, sourceCommit: source,
    artifacts: new Map([...matrix.targets.keys()].map((id) => [id, {
      path: artifact, sha256: sha256(bytes), sizeBytes: bytes.length, buildFingerprint: "2".repeat(64),
    }])), handoffDigest: "3".repeat(64), validationDigest: "4".repeat(64),
    validationPolicy: "factory-only-human-override-v1" };
}

test("each old platform envelope verifies with public authority and equal unified bytes", () => {
  for (const id of matrix.targets.keys()) {
    const signed = fs.readFileSync(path.join(fixtures, `${id}.json`));
    const { payload } = verifyEnvelope(signed, matrix, authority);
    const { core, companion } = payload.components;
    assert.equal(core.sha256, sha256(fs.readFileSync(path.join(fixtures, "artifact.txt"))));
    assert.equal(core.sha256, companion.sha256);
    assert.equal(core.size_bytes, companion.size_bytes);
    assert.equal(core.build_identity.source_revision, source);
    assert.equal(companion.build_identity.source_revision, source);
    assert.equal(core.build_identity.build_fingerprint, companion.build_identity.build_fingerprint);
    const altered = JSON.parse(signed);
    const signature = Buffer.from(altered.signature_base64, "base64");
    signature[0] ^= 1;
    altered.signature_base64 = signature.toString("base64");
    assert.throws(() => verifyEnvelope(Buffer.from(JSON.stringify(altered)), matrix, authority), /signature/);
  }
});

test("public constructor binds the one candidate into both fixed roles", () => {
  const inputs = projectUnifiedReleaseInputs(fixtureInputs());
  assert.equal(inputs.acceptedPair.public_source_commit, source);
  assert.equal(inputs.acceptedPair.private_source_commit, source);
  assert.equal(inputs.targets.length, 5);
  for (const item of inputs.targets) {
    const manifest = targetManifest(candidate, matrix, inputs, item, authority);
    validateTargetManifest(manifest, matrix, authority);
    assert.equal(item.components.core.artifactPath, item.components.companion.artifactPath);
    assert.equal(manifest.components.core.sha256, manifest.components.companion.sha256);
    manifest.install_geometry.core_slot = "<install-root>/wrong";
    assert.throws(() => validateTargetManifest(manifest, matrix, authority), /geometry/);
  }
});

test("constructor rejects missing targets, foreign hashes, source and coverage", () => {
  for (const mutate of [
    (value) => value.artifacts.delete("macos-x64"),
    (value) => { value.artifacts.get("windows-x64").sha256 = "f".repeat(64); },
    (value) => { value.sourceCommit = "0".repeat(40); },
    (value) => { value.validationPolicy = "claimed-native-success"; },
  ]) {
    const value = fixtureInputs(); mutate(value);
    assert.throws(() => projectUnifiedReleaseInputs(value));
  }
});

test("old downloader limit rejects a sparse 128MiB+1 candidate before hashing", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-unified-release-test-"));
  try {
    const artifact = path.join(root, "oversize");
    const descriptor = fs.openSync(artifact, "wx");
    fs.ftruncateSync(descriptor, 128 * 1024 * 1024 + 1); fs.closeSync(descriptor);
    const value = fixtureInputs();
    value.artifacts.get("windows-x64").path = artifact;
    assert.throws(() => projectUnifiedReleaseInputs(value), /size|bound/);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("publication resolves the actual CLI package version or workspace inheritance", async () => {
  const { readCargoVersion } = await import(new URL("../release-version.cjs", testSourceUrl));
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-release-version-test-"));
  try {
    fs.mkdirSync(path.join(root, "crates/ctx-cli"), { recursive: true });
    fs.writeFileSync(path.join(root, "Cargo.toml"), '[workspace.package]\nversion = "1.5.0"\n[dependencies]\n');
    const manifest = path.join(root, "crates/ctx-cli/Cargo.toml");
    fs.writeFileSync(manifest, '[package]\nname = "ctx"\nversion.workspace = true\n[dependencies]\n');
    assert.equal(readCargoVersion(root), "1.5.0");
    fs.writeFileSync(manifest, '[package]\nname = "ctx"\nversion = "1.5.1"\n[dependencies]\n');
    assert.equal(readCargoVersion(root), "1.5.1");
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
