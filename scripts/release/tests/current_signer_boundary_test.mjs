import assert from "node:assert/strict";
import childProcess from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { syntheticLoaded, writeRuntimeHandoff } from "./current_runtime_fixture.mjs";
import { loadRuntimeTransportHandoff, renderHostedManagedPairMetadata } from "../hosted-managed-pair-release.mjs";

const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-current-signer."));
try {
  const repo = fileURLToPath(new URL("../../../", import.meta.url));
  const handoffPath = writeRuntimeHandoff(root);
  const handoff = loadRuntimeTransportHandoff(handoffPath, syntheticLoaded());
  const extension = Buffer.from(`CTX_RELEASE_SEMANTIC_SCHEMA_VERSION=1\nCTX_RELEASE_CANDIDATE_MANIFEST_SHA256_windows_x64=${"a".repeat(64)}\nCTX_RELEASE_CORE_GITHUB_HANDOFF_SHA256=${"a".repeat(64)}\n`);
  const extensionPath = path.join(root, "extension.env"); fs.writeFileSync(extensionPath, extension);
  const metadataPath = path.join(root, "metadata.env");
  fs.writeFileSync(metadataPath, Buffer.concat([renderHostedManagedPairMetadata(syntheticLoaded(), "2026-09-05T00:00:00.000Z", handoff), extension]));
  const publication = path.join(root, "publication.json"); fs.writeFileSync(publication, "prebound pair authority\n");
  for (const name of ["semantic", "candidates", "public"]) fs.mkdirSync(path.join(root, name));
  const marker = path.join(root, "signing-used");
  const output = path.join(root, "signature");
  const bin = path.join(root, "bin"); fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "node"), '#!/bin/sh\nexec "$CTX_TEST_REAL_NODE" --experimental-test-module-mocks --import="$CTX_TEST_PREBOUND" "$@"\n', { mode: 0o700 });
  const secretMarker = path.join(root, "secret-used");
  fs.writeFileSync(path.join(bin, "infisical"), '#!/bin/sh\nprintf "secret-use\\n" >>"$CTX_TEST_SECRET_SENTINEL"\necho TEST_SECRET_LOOKUP_SENTINEL >&2\nexit 73\n', { mode: 0o700 });
  const environment = { HOME: root, XDG_CONFIG_HOME: path.join(root, "config"),
    XDG_DATA_HOME: path.join(root, "data"), XDG_CACHE_HOME: path.join(root, "cache"), TMPDIR: root,
    PATH: `${bin}:${process.env.PATH}`, CTX_TEST_REAL_NODE: (process.env.JS_BINARY__NODE_BINARY || process.execPath),
    CTX_TEST_PREBOUND: new URL("./current_signer_prebound.mjs", import.meta.url).href,
    CTX_TEST_SEMANTIC_EXTENSION: extensionPath, CTX_TEST_SIGNING_SENTINEL: marker,
    CTX_TEST_SECRET_SENTINEL: secretMarker,
    CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM: "test sentinel; never used as a real key",
  };
  // Coherent five-runtime inputs reach crypto.sign without any historical archive.
  const signer = (managed = true, legacy = false) => childProcess.spawnSync("bash", [path.join(repo, "scripts/release/ctx_cli_release_metadata_sign.sh"),
    "--metadata", metadataPath, "--out", output,
    ...(managed ? ["--managed-pair-publication", publication, "--runtime-handoff", handoffPath] : []),
    "--public-ctx-repo", path.join(root, "public"),
    "--semantic-artifact-dir", path.join(root, "semantic"), "--candidate-manifest-handoff", path.join(root, "candidates"),
    "--candidate-handoff-sha256", "a".repeat(64), ...(legacy ? ["--allow-legacy-pre-v0260-nonsemantic"] : [])], { env: environment, encoding: "utf8" });
  const control = signer();
  assert.equal(control.status, 1, `${control.stdout}\n${control.stderr}`);
  assert.match(control.stderr, /TEST_SIGNING_KEY_USE_SENTINEL/u);
  assert.equal(fs.readFileSync(marker, "utf8"), "key-use\n"); fs.unlinkSync(marker);
  let shellAttempt = 0;
  const publisherEnvironment = { ...environment };
  delete publisherEnvironment.CTX_CLI_METADATA_SIGNING_PRIVATE_KEY_PEM;
  const publisher = () => childProcess.spawnSync("bash", [path.join(repo, "scripts/release/publish-hosted-managed-pair-stable.sh"),
    "--publication", publication, "--runtime-handoff", handoffPath,
    "--public-ctx-repo", path.join(root, "public"), "--semantic-artifact-dir", path.join(root, "semantic"),
    "--candidate-manifest-handoff", path.join(root, "candidates"), "--candidate-handoff-sha256", "a".repeat(64),
    "--published-at", "2026-09-05T00:00:00.000Z", "--work-dir", path.join(root, `publish-${shellAttempt++}`)],
    { env: publisherEnvironment, encoding: "utf8" });
  const shellControl = publisher();
  assert.equal(shellControl.status, 73, `${shellControl.stdout}\n${shellControl.stderr}`);
  assert.match(shellControl.stderr, /TEST_SECRET_LOOKUP_SENTINEL/u);
  assert.equal(fs.readFileSync(secretMarker, "utf8"), "secret-use\n"); fs.unlinkSync(secretMarker);
  const completeMetadata = fs.readFileSync(metadataPath);
  fs.writeFileSync(metadataPath, completeMetadata.toString().replace(/^CTX_RELEASE_(?:ONNXRUNTIME|MANAGED_PAIR)_.*\n/gmu, ""));
  for (const legacy of [false, true]) {
    const omitted = signer(false, legacy);
    assert.equal(omitted.status, 1, `${omitted.stdout}\n${omitted.stderr}`);
    assert.match(omitted.stderr, /current stable signing requires --managed-pair-publication and --runtime-handoff/u);
    assert.equal(fs.existsSync(marker), false);
  }
  fs.writeFileSync(metadataPath, completeMetadata);
  fs.writeFileSync(metadataPath, completeMetadata.toString().replace("CTX_RELEASE_VERSION=1.3.3", "CTX_RELEASE_VERSION=1.3.2"));
  const bridge = signer();
  assert.equal(bridge.status, 1, `${bridge.stdout}\n${bridge.stderr}`);
  assert.equal(fs.existsSync(marker), false);
  assert.equal(fs.existsSync(output), false);
  assert.ok(!bridge.stderr.includes("TEST_SIGNING_KEY_USE_SENTINEL"));
  process.stdout.write("standalone signer: five current runtimes reach key-use sentinel; B metadata and omitted current authority reject before key use (other authorities prebound)\n");
} finally { fs.rmSync(root, { recursive: true }); }
