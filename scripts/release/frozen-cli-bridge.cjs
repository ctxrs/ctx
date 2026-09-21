"use strict";

const crypto = require("node:crypto");
const { compareReleaseVersions } = require("./release-version.cjs");

const BRIDGE_VERSION = "1.3.2";
// Exact published B identity, authorized by the September 9 operator waiver in
// docs/exec-plans/release132-then140/bridge-operator-disposition.md. Its digest
// records that decision; it is not proof that native bootstrap checks passed.
const FROZEN_BRIDGE_IDENTITY = { publicSourceCommit: "22ae223ebb4a0c909861e3a7e1e98d4bd5b2d523", privateSourceCommit: "41f74364c70d9e5839b9b06c4a81aab5d1eeb7c9", metadataSha256: "5d93ced35050a41d23a84e71a4e7057cea8fe99fa4f5d65cc7bb023affd6054a", signatureSha256: "98e6c85408563240c81ea0535d6cf2b82649ce394ab6f26ee19b6b70fde7b6b0", operatorDispositionSha256: "ec1caa1a3b1273c31e8bc8e5029a3def2f751b2257a4112be4dfbd8d23eaf08a" };

function assertCurrentReleaseVersion(version) {
  if (compareReleaseVersions(version, BRIDGE_VERSION) <= 0) {
    throw new Error(`current release source requires a version after ${BRIDGE_VERSION}; use retained B source for bridge construction`);
  }
}

function assertFrozenIdentity(identity) {
  if (identity == null) throw new Error("frozen bridge 1.3.2 has no reviewed disposition; future stable promotion is blocked");
  const fields = {
    publicSourceCommit: 40, privateSourceCommit: 40, metadataSha256: 64,
    signatureSha256: 64, operatorDispositionSha256: 64,
  };
  if (Object.keys(identity).sort().join("\0") !== Object.keys(fields).sort().join("\0")
      || Object.entries(fields).some(([key, length]) => typeof identity[key] !== "string"
        || identity[key].length !== length || !/^[0-9a-f]+$/u.test(identity[key])
        || /^0+$/u.test(identity[key]))) {
    throw new Error("frozen bridge reviewed identity is incomplete");
  }
}

async function verifyFrozenBridgeSnapshot(identity, pointerBytes, metadata, signature) {
  assertFrozenIdentity(identity);
  const digest = (body) => crypto.createHash("sha256").update(body).digest("hex");
  if (digest(metadata) !== identity.metadataSha256 || digest(signature) !== identity.signatureSha256) {
    throw new Error("frozen bridge signed byte identity differs from reviewed disposition");
  }
  let pointer;
  try { pointer = JSON.parse(pointerBytes.toString("utf8")); } catch {
    throw new Error("frozen bridge pointer is invalid");
  }
  const metadataObject = `releases/stable/${BRIDGE_VERSION}/ctx-release-metadata.env`;
  const expected = {
    channel: "stable", contract: "ctx-cli-release-pointer", metadata_object: metadataObject,
    metadata_sha256: identity.metadataSha256, schema_version: 1,
    signature_object: `${metadataObject}.sig`, signature_sha256: identity.signatureSha256,
    version: BRIDGE_VERSION,
  };
  if (pointer == null || typeof pointer !== "object" || Array.isArray(pointer)
      || Object.keys(pointer).sort().join("\0") !== Object.keys(expected).sort().join("\0")
      || Object.entries(expected).some(([key, value]) => pointer[key] !== value)) {
    throw new Error("original feed is not frozen to the reviewed bridge");
  }
  const values = new Map();
  for (const line of metadata.toString("utf8").trimEnd().split("\n")) {
    const match = /^(CTX_[A-Z0-9_a-z]+)=([^\r\n]*)$/u.exec(line);
    if (match == null || values.has(match[1])) throw new Error("frozen bridge metadata is invalid");
    values.set(match[1], match[2]);
  }
  if (values.get("CTX_RELEASE_VERSION") !== BRIDGE_VERSION
      || values.get("CTX_RELEASE_CHANNEL") !== "stable"
      || values.get("CTX_RELEASE_SOURCE_COMMIT") !== identity.publicSourceCommit) {
    throw new Error("frozen bridge source identity differs from reviewed disposition");
  }
  const encoded = signature.toString("utf8");
  const bytes = Buffer.from(encoded.trim(), "base64");
  const { CLI_METADATA_PUBLIC_KEY_PEM } = await import("../../services/install-site/src/cli-install-script.js");
  if (!/^[A-Za-z0-9+/]+={0,2}\n$/u.test(encoded)
      || bytes.toString("base64") !== encoded.trim()
      || !crypto.verify("RSA-SHA256", metadata, CLI_METADATA_PUBLIC_KEY_PEM, bytes)) {
    throw new Error("frozen bridge signature does not verify with installer trust");
  }
}

async function assertFrozenBridgePromotion(version) {
  assertCurrentReleaseVersion(version);
  assertFrozenIdentity(FROZEN_BRIDGE_IDENTITY);
  const { readBoundedResponse } = await import("./managed-pair-release-io.mjs");
  const base = "https://cli.ctx.rs/functions/v1/releases/stable";
  const read = async (url, maximum) => {
    const response = await fetch(url, { redirect: "error", headers: { "accept-encoding": "identity" } });
    if (!response.ok) throw new Error(`frozen bridge readback failed: HTTP ${response.status}`);
    return (await readBoundedResponse(response, maximum, true, "frozen bridge readback")).body;
  };
  const pointer = await read(`${base}/current.json`, 4096);
  const metadata = await read(`${base}/${BRIDGE_VERSION}/ctx-release-metadata.env`, 128 * 1024);
  const signature = await read(`${base}/${BRIDGE_VERSION}/ctx-release-metadata.env.sig`, 16 * 1024);
  await verifyFrozenBridgeSnapshot(FROZEN_BRIDGE_IDENTITY, pointer, metadata, signature);
  return { version: BRIDGE_VERSION, ...FROZEN_BRIDGE_IDENTITY };
}

module.exports = { BRIDGE_VERSION, assertCurrentReleaseVersion, assertFrozenBridgePromotion, verifyFrozenBridgeSnapshot };
