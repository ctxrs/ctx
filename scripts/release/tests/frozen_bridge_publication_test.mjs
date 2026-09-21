import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";
import childProcess from "node:child_process";
import { promoteCurrentPointer } from "../publish-hosted-managed-pair-stable.mjs";
import { assertCurrentReleaseVersion, assertFrozenBridgePromotion, verifyFrozenBridgeSnapshot } from "../frozen-cli-bridge.cjs";
import { readBoundedResponse } from "../managed-pair-release-io.mjs";

function pointer(version = "1.3.2", extra = {}) {
  return Buffer.from(`${JSON.stringify({
    channel: "stable", contract: "ctx-cli-release-pointer", schema_version: 1, version,
    metadata_object: `releases/stable/${version}/ctx-release-metadata.env`, metadata_sha256: "a".repeat(64),
    signature_object: `releases/stable/${version}/ctx-release-metadata.env.sig`, signature_sha256: "b".repeat(64),
    ...extra,
  })}\n`);
}
test("post-B construction rejects B and a failed frozen readback prevents storage", async (t) => {
  assert.doesNotThrow(() => assertCurrentReleaseVersion("1.3.3"));
  for (const version of ["1.3.1", "1.3.2"]) {
    assert.throws(() => assertCurrentReleaseVersion(version), /use retained B source/u);
    await assert.rejects(assertFrozenBridgePromotion(version), /use retained B source/u);
  }
  t.mock.method(globalThis, "fetch", async () => new Response("unavailable", { status: 503 }));
  await assert.rejects(assertFrozenBridgePromotion("1.4.0"), /frozen bridge readback failed: HTTP 503/u);
  let reads = 0;
  const request = () => { reads += 1; throw new Error("unexpected storage"); };
  await assert.rejects(promoteCurrentPointer(request, { version: "1.4.0" }, pointer("1.4.0")), /frozen bridge readback failed: HTTP 503/u);
  assert.equal(reads, 0);
});

test("current pointer retains frozen original and bounded conditional retry behavior", () => {
  const result = childProcess.spawnSync((process.env.JS_BINARY__NODE_BINARY || process.execPath), ["--experimental-test-module-mocks",
    new URL("./current_feed_publication_test.mjs", import.meta.url).pathname], { encoding: "utf8", env: process.env });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});

test("the shared publication reader cancels an oversized body without trusting content-length", async () => {
  let cancelled = false;
  const response = new Response(new ReadableStream({
    pull(controller) { controller.enqueue(new Uint8Array(5)); },
    cancel() { cancelled = true; },
  }));
  await assert.rejects(readBoundedResponse(response, 4, true, "bridge test"), /streamed body bound/u);
  assert.equal(cancelled, true);
  await assert.rejects(readBoundedResponse(new Response("too-long", { headers: { "content-length": "2" } }), 4, true, "bridge test"), /streamed body bound/u);
});

test("reviewed disposition still requires exact signed bytes, source and frozen pointer", async () => {
  const metadata = Buffer.from(`CTX_RELEASE_VERSION=1.3.2\nCTX_RELEASE_CHANNEL=stable\nCTX_RELEASE_SOURCE_COMMIT=${"c".repeat(40)}\n`);
  const signature = Buffer.from("invalid-signature\n");
  const digest = (body) => crypto.createHash("sha256").update(body).digest("hex");
  const identity = { publicSourceCommit: "c".repeat(40), privateSourceCommit: "d".repeat(40),
    metadataSha256: digest(metadata), signatureSha256: digest(signature), operatorDispositionSha256: "e".repeat(64) };
  const bound = pointer("1.3.2", { metadata_sha256: identity.metadataSha256, signature_sha256: identity.signatureSha256 });
  await assert.rejects(verifyFrozenBridgeSnapshot(null, bound, metadata, signature), /no reviewed disposition/u);
  await assert.rejects(verifyFrozenBridgeSnapshot(identity, bound, Buffer.from("altered"), signature), /byte identity/u);
  await assert.rejects(verifyFrozenBridgeSnapshot(identity, pointer("1.3.3"), metadata, signature), /not frozen/u);
  await assert.rejects(verifyFrozenBridgeSnapshot({ ...identity, publicSourceCommit: "f".repeat(40) }, bound, metadata, signature), /source identity/u);
  await assert.rejects(verifyFrozenBridgeSnapshot(identity, bound, metadata, signature), /signature does not verify/u);
});

test("authored frozen snapshot accepts exact signed bytes and rejects newly signed replacement bytes", async (t) => {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
  // Test-process PUBLIC authority only; no production override or retained private key.
  t.mock.module(new URL("../../../services/install-site/src/cli-install-script.js", import.meta.url).href, {
    namedExports: { CLI_METADATA_PUBLIC_KEY_PEM: publicKey.export({ format: "pem", type: "spki" }) },
  });
  const metadata = Buffer.from(`CTX_RELEASE_VERSION=1.3.2\nCTX_RELEASE_CHANNEL=stable\nCTX_RELEASE_SOURCE_COMMIT=${"c".repeat(40)}\n`);
  const signature = Buffer.from(`${crypto.sign("RSA-SHA256", metadata, privateKey).toString("base64")}\n`);
  const digest = (body) => crypto.createHash("sha256").update(body).digest("hex");
  const identity = { publicSourceCommit: "c".repeat(40), privateSourceCommit: "d".repeat(40),
    metadataSha256: digest(metadata), signatureSha256: digest(signature), operatorDispositionSha256: "e".repeat(64) };
  const bound = pointer("1.3.2", { metadata_sha256: identity.metadataSha256, signature_sha256: identity.signatureSha256 });
  await assert.doesNotReject(verifyFrozenBridgeSnapshot(identity, bound, metadata, signature));
  const replacement = Buffer.concat([metadata, Buffer.from("CHANGED=1\n")]);
  const resigned = Buffer.from(`${crypto.sign("RSA-SHA256", replacement, privateKey).toString("base64")}\n`);
  await assert.rejects(verifyFrozenBridgeSnapshot(identity, bound, replacement, resigned), /byte identity/u);
});
