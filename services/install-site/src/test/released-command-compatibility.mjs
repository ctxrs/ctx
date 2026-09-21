// Manual, real-executable negative regression in an isolated test environment:
// node <this file> /path/to/released/linux-x64/ctx
// Exit 1 means incompatible, never publication approval. --expect-rejection
// makes the known 1.3.1 regression exit 0; its report still cannot qualify a
// publication. Current candidate installation belongs to the live smoke owner.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, verify } from "node:crypto";
import { chmodSync, copyFileSync, mkdtempSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CLI_METADATA_PUBLIC_KEY_PEM } from "../cli-install-script.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const coreSha256 = "67cb0eb8c12c49f1f69a83a73ddf9a5c8d8282c3f2c77a53fa541210cb501109";
const operation = "--ctx-core-managed-pair-apply-v1";
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

// Captured from the production v1 feed on 2026-09-07. The detached production
// signature authenticates the exact retained metadata bytes; no test key or
// caller-supplied identity is accepted. The compiled version string is not proof.
export function authenticateReleased131Core(coreBytes, metadata, signature) {
  assert.ok(verify("sha256", metadata, CLI_METADATA_PUBLIC_KEY_PEM,
    Buffer.from(signature.toString().trim(), "base64")), "released metadata signature is invalid");
  const fields = new Map();
  for (const line of metadata.toString().trimEnd().split("\n")) {
    const separator = line.indexOf("=");
    assert.ok(separator > 0, "malformed released metadata");
    const name = line.slice(0, separator);
    assert.ok(!fields.has(name), "duplicate released metadata field");
    fields.set(name, line.slice(separator + 1));
  }
  assert.equal(fields.get("CTX_RELEASE_VERSION"), "1.3.1");
  assert.equal(fields.get("CTX_RELEASE_CHANNEL"), "stable");
  assert.equal(fields.get("CTX_RELEASE_SHA256_linux_x64"), coreSha256);
  assert.equal(fields.get("CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_linux_x64"), coreSha256);
  assert.equal(sha256(coreBytes), coreSha256,
    "requires genuine released 1.3.1 bytes; source builds and fake command support cannot qualify publication");
  return { version: "1.3.1", platform: "linux-x64", core_sha256: coreSha256,
    metadata_sha256: sha256(metadata), signature_sha256: sha256(signature) };
}

export function probeReleased131(binaryPath) {
  assert.equal(process.platform, "linux", "requires native Linux x64 execution");
  assert.equal(process.arch, "x64", "requires native Linux x64 execution");
  const root = mkdtempSync(path.join(tmpdir(), "ctx-released-command-"));
  try {
    // Execute an isolated copy only after authenticating those exact bytes.
    const candidate = path.join(root, "candidate ctx");
    copyFileSync(binaryPath, candidate);
    chmodSync(candidate, 0o700);
    const identity = authenticateReleased131Core(readFileSync(candidate),
      readFileSync(path.join(here, "released-command-compatibility-v1.3.1.env")),
      readFileSync(path.join(here, "released-command-compatibility-v1.3.1.sig")));
    const env = { PATH: "/usr/bin:/bin", NO_COLOR: "1", CTX_DAEMON_AUTOSTART_OFF: "1",
      CTX_DAEMON_ENABLED: "false", CTX_UPGRADE_AUTO: "off", CTX_ANALYTICS_ENABLED: "false",
      CTX_LOCAL_USAGE_ENABLED: "false", CTX_INSTALL_NO_PRO_TRIAL: "1" };
    for (const name of ["HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME",
      "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "CTX_DATA_ROOT"]) {
      env[name] = path.join(root, name);
      mkdirSync(env[name], { mode: 0o700 });
    }
    const installRoot = path.join(root, "install root");
    mkdirSync(installRoot, { mode: 0o700 });
    // Independent literal argv, matching the Rust request's eight positions
    // including argv[0]. Missing input files must never reach installation.
    const result = spawnSync(candidate, [operation, installRoot, "-",
      path.join(root, "envelope.json"), candidate, path.join(root, "ctx-pro"),
      path.join(root, "marker.json")], { cwd: root, env, encoding: "utf8",
      timeout: 15_000, maxBuffer: 16 * 1024 });
    assert.ifError(result.error);
    assert.equal(result.signal, null);
    assert.equal(result.status, 2, "released parser must reject the operation");
    assert.match(result.stderr, /unexpected argument '--ctx-core-managed-pair-apply-v1' found/);
    assert.equal(result.stdout, "");
    return { ...identity, operation, exit_code: result.status,
      stderr: result.stderr, regression_passed: true, compatible: false,
      publication_qualified: false };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [binary, mode, ...extra] = process.argv.slice(2);
    assert.ok(binary && extra.length === 0 && (!mode || mode === "--expect-rejection"),
      "usage: node released-command-compatibility.mjs CORE [--expect-rejection]");
    console.log(JSON.stringify(probeReleased131(binary), null, 2));
    process.exitCode = mode === "--expect-rejection" ? 0 : 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
