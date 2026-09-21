// Authored buffers exercise the production hosted-object constructor; no upload.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { immutableArtifactObjects } from "../publish-hosted-managed-pair-stable.mjs";

const directory = process.argv[2];
assert.ok(directory, "expected authored artifact directory");
const targets = new Map();
const names = [
  ["linux-x64", "ctx", "ctx-linux-x64", "ctx-pro-linux-x64"],
  ["linux-arm64", "ctx-linux-aarch64", "ctx-linux-aarch64", "ctx-pro-linux-arm64"],
  ["macos-arm64", "ctx-macos-arm64", "ctx-macos-arm64", "ctx-pro-macos-arm64"],
  ["macos-x64", "ctx-macos-x64", "ctx-macos-x64", "ctx-pro-macos-x64"],
  ["windows-x64", "ctx.exe", "ctx-windows-x64.exe", "ctx-pro-windows-x64.exe"],
];
for (const [id, alias, coreName, companionName] of names) {
  const body = fs.readFileSync(path.join(directory, alias));
  const digest = crypto.createHash("sha256").update(body).digest("hex");
  const component = name => ({ artifact: { body }, identity: { object_key: `sha256/${digest}/${name}` } });
  targets.set(id, {
    core: component(coreName), companion: component(companionName),
    manifestRecord: { name: `ctx-managed-pair-${id}.json` },
    manifest: { body: Buffer.from("authored manifest placeholder; not signature evidence\n") },
  });
}
const loaded = { version: "1.5.0", targets };
const objects = immutableArtifactObjects(loaded, null);
const repeated = immutableArtifactObjects(loaded, null);
assert.deepEqual(objects, repeated, "hosted packaging must be deterministic");
const gzipObjects = objects.filter(object => object.key.endsWith(".gz"));
assert.deepEqual(gzipObjects.map(object => path.basename(object.key)).sort(), names.map(([, alias]) => `${alias}.gz`).sort());
for (const object of gzipObjects) {
  const alias = path.basename(object.key).slice(0, -3);
  assert.deepEqual(zlib.gunzipSync(object.body), fs.readFileSync(path.join(directory, alias)));
  assert.equal(object.body.readUInt32LE(4), 0, "gzip mtime must be zero");
  assert.equal(object.body[3] & 8, 0, "gzip must not embed a filename");
  fs.writeFileSync(path.join(directory, `${alias}.gz`), object.body);
}
