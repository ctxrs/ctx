import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mock, test } from "node:test";
import { materializeReleaseTestSources } from "./current_runtime_fixture.mjs";

const testSourceUrl = materializeReleaseTestSources(import.meta.url);
const authorities = await import(new URL("../release-authority.mjs", testSourceUrl));
const { finalizeManagedPairRelease } = await import(new URL("../release-manifest.mjs", testSourceUrl));
const { projectUnifiedReleaseInputs } = await import(new URL("../unified-release-inputs.mjs", testSourceUrl));
const { compareReleaseVersions, isReleaseVersion, parseReleaseVersion } = await import(new URL("../release-version.cjs", testSourceUrl));
const { canonicalJsonBytes, hashStableFile, loadCandidate, loadInputAuthority,
  loadTargetMatrix, sha256, trustedTargetMatrix, validateOutputPath, verifyEnvelope,
} = await import(new URL("../managed-pair-release-contract.mjs", testSourceUrl));

const matrixPath = fileURLToPath(new URL("../../../contracts/release-targets-v1.json", testSourceUrl));
const matrix = loadTargetMatrix(matrixPath);
const source = "a".repeat(40);
let signer;
function signingFixture() {
  // Generated only during test execution; no private key is stored in the tree.
  signer ??= crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
  const publicKey = signer.publicKey;
  return { privateKey: signer.privateKey.export({ format: "pem", type: "pkcs8" }),
    authority: { channel: "stable", releaseKeyId: "ctx-release-fixture",
      publicKey, publicKeyDigest: sha256(publicKey.export({ format: "der", type: "pkcs1" })) } };
}
function fixture(t, releaseName = "v1.5.0") {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-public-publication-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const artifact = path.join(root, "authored-artifact.txt");
  const bytes = Buffer.from("authored unified fixture; not an executable or release receipt\n");
  fs.writeFileSync(artifact, bytes);
  const candidate = { channel: "stable", release_name: releaseName, rollback_generation: 27 };
  const inputs = projectUnifiedReleaseInputs({ candidate, matrix, sourceCommit: source,
    artifacts: new Map([...matrix.targets.keys()].map((id) => [id, {
      path: artifact, sha256: sha256(bytes), sizeBytes: bytes.length, buildFingerprint: "b".repeat(64),
    }])), handoffDigest: "c".repeat(64), validationDigest: "d".repeat(64),
    validationPolicy: "factory-only-human-override-v1" });
  const { authority, privateKey } = signingFixture();
  const prepared = (name) => ({ authority, candidate, matrix, inputs,
    outputPreflight: validateOutputPath(path.join(root, name)) });
  return { root, artifact, bytes, candidate, inputs, authority, privateKey, prepared };
}

test("public finalizer deterministically signs five envelopes and both legacy filenames", (t) => {
  const f = fixture(t);
  const outputs = ["first", "second"].map((name) =>
    finalizeManagedPairRelease(f.prepared(name), f.privateKey));
  const first = outputs[0].publication;
  assert.equal(first.component_objects.length, 10);
  assert.equal(first.target_manifest_objects.length, 5);
  assert.equal(first.pointer_object, "channels/stable/managed-pair.json");
  for (const entry of [...first.component_objects, ...first.target_manifest_objects, first.release_set_object]) {
    assert.equal(entry.object_key, `sha256/${entry.sha256}/${entry.name}`);
    assert.equal(sha256(fs.readFileSync(entry.path)), entry.sha256);
    assert.ok(entry.path.startsWith(path.join(f.root, "first") + path.sep));
  }
  for (let i = 0; i < 5; i += 1) {
    const bytes = fs.readFileSync(first.target_manifest_objects[i].path);
    assert.deepEqual(bytes, fs.readFileSync(outputs[1].publication.target_manifest_objects[i].path));
    const { payload } = verifyEnvelope(bytes, matrix, f.authority);
    assert.equal(payload.components.core.sha256, payload.components.companion.sha256);
    assert.equal(payload.components.core.build_identity.source_revision, source);
    assert.equal(payload.components.companion.build_identity.source_revision, source);
    const changed = JSON.parse(bytes);
    changed.signature_base64 = Buffer.alloc(256).toString("base64");
    assert.throws(() => verifyEnvelope(canonicalJsonBytes(changed), matrix, f.authority), /signature/);
  }
  assert.deepEqual(fs.readFileSync(first.release_set_object.path),
    fs.readFileSync(outputs[1].publication.release_set_object.path));
  assert.deepEqual(fs.readFileSync(f.artifact), f.bytes);
});

test("finalizer rejects a different signing key and frozen bridge construction", (t) => {
  const f = fixture(t);
  const otherKey = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey.export({ format: "pem", type: "pkcs8" });
  assert.throws(() => finalizeManagedPairRelease(f.prepared("wrong-key"), otherKey), /signing key does not match/);
  assert.equal(fs.existsSync(path.join(f.root, "wrong-key/publication.json")), false);
  const frozen = f.prepared("frozen");
  frozen.candidate = { ...f.candidate, release_name: "v1.3.2" };
  assert.throws(() => finalizeManagedPairRelease(frozen, f.privateKey), /use retained B source/);
  assert.equal(fs.existsSync(path.join(f.root, "frozen")), false);
});

test("duplicate JSON and a self-authored matrix cannot replace public authority", (t) => {
  const f = fixture(t);
  const duplicate = path.join(f.root, "duplicate.json");
  fs.writeFileSync(duplicate, '{"schema_version":1,"schema_version":1}');
  assert.throws(() => loadCandidate(duplicate), /duplicate JSON key/);
  assert.throws(() => loadTargetMatrix(duplicate), /duplicate JSON key/);
  assert.throws(() => authorities.loadReleaseAuthorities({ registryPath: duplicate }), /duplicate JSON key/);
  const altered = path.join(f.root, "self-authored-matrix.json");
  fs.writeFileSync(altered, JSON.stringify({ ...matrix.value, invented_rotation: 2 }));
  const digest = sha256(fs.readFileSync(altered));
  const authority = loadInputAuthority();
  assert.throws(() => loadTargetMatrix(altered, digest, authority), /fixed public contract/);
  assert.throws(() => trustedTargetMatrix(authority, digest), /untrusted target matrix/);
  const candidatePath = path.join(f.root, "candidate.json");
  const candidate = { contract: "ctx-managed-pair-release-candidate", schema_version: 1,
    channel: "stable", release_name: "v1.5.0", target_matrix_sha256: matrix.digest, rollback_generation: 27 };
  fs.writeFileSync(candidatePath, JSON.stringify(candidate));
  assert.deepEqual(loadCandidate(candidatePath, authority), candidate);
  for (const mutation of [{ target_matrix_sha256: "0".repeat(64) }, { rollback_generation: 0 },
    { channel: "production" }, { arbitrary_field: true }]) {
    fs.writeFileSync(candidatePath, JSON.stringify({ ...candidate, ...mutation }));
    assert.throws(() => loadCandidate(candidatePath, authority));
  }
  assert.equal(matrix.targets.get("windows-x64").public_rust_target, "x86_64-pc-windows-gnu");
});

test("shared public version parser preserves every released grammar and ordering vector", () => {
  const contract = JSON.parse(fs.readFileSync(new URL("../../../contracts/release-version-v1.json", testSourceUrl)));
  for (const value of contract.valid) {
    assert.equal(isReleaseVersion(value), true); assert.equal(parseReleaseVersion(value).length, 3);
  }
  for (const value of contract.invalid) assert.equal(isReleaseVersion(value), false);
  for (const { left, right, result } of contract.ordering) assert.equal(compareReleaseVersions(left, right), result);
});

test("public authority rejects RSA keys outside verifier bounds and wrong key digests", (t) => {
  const f = fixture(t);
  const registry = (publicKey) => ({ contract: "ctx-managed-pair-release-authority", schema_version: 1,
    channels: ["stable", "staging"].map((id) => ({ id, key_id: "ctx-fixture",
      signature_algorithm: "rsa-pkcs1v15-sha256",
      public_key_der_sha256: sha256(publicKey.export({ format: "der", type: "pkcs1" })),
      public_key_pem: publicKey.export({ format: "pem", type: "pkcs1" }) })) });
  const file = path.join(f.root, "authority.json");
  for (const bits of [2047, 8193]) {
    const modulus = Buffer.alloc(Math.ceil(bits / 8));
    modulus[0] = 1 << ((bits - 1) % 8); modulus[modulus.length - 1] |= 1;
    const publicKey = crypto.createPublicKey({ format: "jwk", key: { kty: "RSA", e: "AQAB", n: modulus.toString("base64url") } });
    fs.writeFileSync(file, JSON.stringify(registry(publicKey)));
    assert.throws(() => authorities.loadReleaseAuthorities({ registryPath: file }), /public key identity/);
  }
  const wrongDigest = registry(f.authority.publicKey);
  wrongDigest.channels[0].public_key_der_sha256 = "0".repeat(64);
  fs.writeFileSync(file, JSON.stringify(wrongDigest));
  assert.throws(() => authorities.loadReleaseAuthorities({ registryPath: file }), /public key identity/);
});

test("output and artifact identity reject links and replacement races", (t) => {
  const f = fixture(t);
  assert.throws(() => validateOutputPath(f.root), /must not already exist/);
  const dangling = path.join(f.root, "dangling");
  fs.symlinkSync(path.join(f.root, "absent"), dangling);
  assert.throws(() => validateOutputPath(dangling), /must not already exist/);
  assert.throws(() => hashStableFile(dangling, "fixture", 1024), /symlink|reparse/);
  const retained = `${f.artifact}.retained`;
  assert.throws(() => hashStableFile(f.artifact, "fixture", 1024, () => {
    fs.renameSync(f.artifact, retained); fs.writeFileSync(f.artifact, f.bytes);
  }), /changed while|path changed/);
  fs.unlinkSync(f.artifact); fs.renameSync(retained, f.artifact);
  // Re-project after the deliberate race: the captured source identity changed.
  const clean = fixture(t);
  const parent = path.join(clean.root, "parent"); fs.mkdirSync(parent, { mode: 0o700 });
  const prepared = { ...clean.prepared("unused"), outputPreflight: validateOutputPath(path.join(parent, "release")) };
  assert.throws(() => finalizeManagedPairRelease(prepared, clean.privateKey, {
    afterOutputParentOpen() { fs.renameSync(parent, `${parent}.retained`); fs.mkdirSync(parent, { mode: 0o700 }); },
  }), /output parent path changed/);
  const swapped = clean.prepared("swapped");
  assert.throws(() => finalizeManagedPairRelease(swapped, clean.privateKey, {
    afterOutputDirectoryCreate({ output }) { fs.renameSync(output, `${output}.retained`); fs.symlinkSync(`${output}.retained`, output); },
  }), /output directory was substituted/);
  const tampered = clean.prepared("tampered");
  fs.appendFileSync(clean.artifact, "tamper");
  assert.throws(() => finalizeManagedPairRelease(tampered, clean.privateKey), /changed|differ/);
  assert.equal(fs.existsSync(path.join(clean.root, "tampered/publication.json")), false);
});

test("hosted loader verifies signed publication before accepting candidate artifacts", async (t) => {
  const f = fixture(t);
  // Only Node's test loader substitutes a PUBLIC authority; production has no override.
  mock.module(new URL("../release-authority.mjs", testSourceUrl).href, {
    namedExports: { ...authorities, releaseTrust: (channel) => {
      assert.equal(channel, "stable"); return f.authority;
    } },
  });
  t.after(() => mock.restoreAll());
  const { loadHostedManagedPairPublication } = await import(new URL("../hosted-managed-pair-release.mjs", testSourceUrl));
  const generated = finalizeManagedPairRelease(f.prepared("hosted"), f.privateKey);
  const loaded = loadHostedManagedPairPublication(generated.publicationPath);
  assert.equal(loaded.version, "1.5.0");
  assert.equal(loaded.publicCommit, source); assert.equal(loaded.privateCommit, source);
  const next = fixture(t, "v2.0.0");
  const nextPublication = finalizeManagedPairRelease(next.prepared("hosted-v2"), next.privateKey).publicationPath;
  assert.equal(loadHostedManagedPairPublication(nextPublication).version, "2.0.0");
  for (const releaseName of ["v2.0.0-rc1", "v02.0.0"]) {
    fs.writeFileSync(nextPublication, canonicalJsonBytes({
      ...JSON.parse(fs.readFileSync(nextPublication)), release_name: releaseName,
    }));
    assert.throws(() => loadHostedManagedPairPublication(nextPublication), /exact stable release authority/);
  }
  const publication = generated.publication;
  fs.writeFileSync(generated.publicationPath, canonicalJsonBytes({ ...publication, release_name: "v1.5.1" }));
  assert.throws(() => loadHostedManagedPairPublication(generated.publicationPath), /selected stable release/);
  fs.writeFileSync(generated.publicationPath, canonicalJsonBytes(publication));
  const releaseSet = JSON.parse(fs.readFileSync(publication.release_set_object.path));
  releaseSet.signature_base64 = Buffer.alloc(256).toString("base64");
  const invalidBytes = canonicalJsonBytes(releaseSet);
  const original = fs.readFileSync(publication.release_set_object.path);
  fs.writeFileSync(publication.release_set_object.path, invalidBytes);
  const invalid = structuredClone(publication);
  invalid.release_set_object.sha256 = sha256(invalidBytes);
  invalid.release_set_object.size_bytes = invalidBytes.length;
  invalid.release_set_object.object_key = `sha256/${sha256(invalidBytes)}/${invalid.release_set_object.name}`;
  fs.writeFileSync(generated.publicationPath, canonicalJsonBytes(invalid));
  assert.throws(() => loadHostedManagedPairPublication(generated.publicationPath), /signature/);
  fs.writeFileSync(publication.release_set_object.path, original);
  fs.writeFileSync(generated.publicationPath, canonicalJsonBytes(publication));
  fs.appendFileSync(publication.component_objects[0].path, "tamper");
  assert.throws(() => loadHostedManagedPairPublication(generated.publicationPath), /signed identity/);
});
