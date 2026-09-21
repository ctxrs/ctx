import assert from "node:assert/strict";
import childProcess from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { putImmutableR2Object } from "../core-r2.mjs";
import {
  beginCredentials,
  COREML_REPAIR,
  COREML_VALIDATOR_CLOSURE,
  credentialFailed,
  finalizeEvidence,
  runPreflight,
  runPublicValidator,
  runPublisher,
  writeExclusiveEvidence,
} from "../repair-coreml-semantic-object.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const ARCHIVE_PATH = path.join("/synthetic", COREML_REPAIR.archiveBasename);
const ARCHIVE_BODY = Buffer.from("synthetic CoreML archive bytes\n", "utf8");

function canonicalJson(value) {
  const sort = (candidate) => Array.isArray(candidate) ? candidate.map(sort)
    : candidate != null && typeof candidate === "object"
      ? Object.fromEntries(Object.keys(candidate).sort().map((key) => [key, sort(candidate[key])]))
      : candidate;
  return `${JSON.stringify(sort(value))}\n`;
}

function assetSidecar(extraAsset = {}) {
  return Buffer.from(canonicalJson({
    asset: {
      archive_format: "tar.xz",
      archive_path_prefix: COREML_REPAIR.archiveBasename.replace(/\.tar\.xz$/u, ""),
      archive_sha256: COREML_REPAIR.archiveSha256,
      artifact: COREML_REPAIR.archiveBasename,
      backend: "coreml",
      files: [{ path: "manifest.json", sha256: COREML_REPAIR.manifestSha256, size: 1 }],
      max_expanded_bytes: 2147483648,
      max_files: 4096,
      platform: "macos-arm64",
      role: "accelerator",
      version: "1.0.0",
      ...extraAsset,
    },
    id: "apple_coreml",
  }));
}

function validator() {
  return {
    closure: COREML_VALIDATOR_CLOSURE,
    public_commit: "b".repeat(40),
    public_source: "clean-explicit-public-ctx-git-worktree-snapshot",
    public_tree: "c".repeat(40),
    validator_path: COREML_REPAIR.validatorRelativePath,
    validator_sha256: COREML_VALIDATOR_CLOSURE.files[0].sha256,
  };
}

function artifactDependencies(overrides = {}) {
  const files = new Map([
    [ARCHIVE_PATH, { absolute: ARCHIVE_PATH, body: ARCHIVE_BODY, sizeBytes: COREML_REPAIR.archiveSizeBytes }],
    [ARCHIVE_PATH + ".sha256", { absolute: ARCHIVE_PATH + ".sha256", body: Buffer.from(`${COREML_REPAIR.archiveSha256}  ${COREML_REPAIR.archiveBasename}\n`), sizeBytes: 131 }],
    [ARCHIVE_PATH + ".asset.json", { absolute: ARCHIVE_PATH + ".asset.json", body: assetSidecar(), sizeBytes: assetSidecar().length }],
  ]);
  for (const [suffix, value] of Object.entries(overrides.files ?? {})) {
    files.set(ARCHIVE_PATH + suffix, { ...files.get(ARCHIVE_PATH + suffix), ...value });
  }
  const dependencyOverrides = { ...overrides };
  delete dependencyOverrides.files;
  return {
    now: () => "2026-08-26T12:00:00.000Z",
    readStableFile(file) {
      const value = files.get(file);
      if (value == null) throw new Error(`unexpected synthetic file: ${file}`);
      return value;
    },
    runPublicValidator() { return validator(); },
    sha256(body) {
      return body.equals(ARCHIVE_BODY) ? COREML_REPAIR.archiveSha256 : crypto.createHash("sha256").update(body).digest("hex");
    },
    ...dependencyOverrides,
  };
}

function environment(bucket = COREML_REPAIR.bucket) {
  return {
    CTX_RELEASE_R2_ACCESS_KEY_ID: "synthetic-access",
    CTX_RELEASE_R2_BUCKET: bucket,
    CTX_RELEASE_R2_ENDPOINT: "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
    CTX_RELEASE_R2_SECRET_ACCESS_KEY: "synthetic-secret",
  };
}

function withTemporaryDirectory(prefix, callback) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  try {
    const result = callback(root);
    if (result != null && typeof result.then === "function") return result.finally(() => fs.rmSync(root, { force: true, recursive: true }));
    fs.rmSync(root, { force: true, recursive: true });
    return result;
  } catch (error) {
    fs.rmSync(root, { force: true, recursive: true });
    throw error;
  }
}

function createGitValidator(root) {
  for (const file of COREML_VALIDATOR_CLOSURE.files) {
    const destination = path.join(root, file.path);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.writeFileSync(destination, "wrong pinned bytes\n");
  }
  for (const argv of [["init", "-q"], ["config", "user.email", "test@example.invalid"], ["config", "user.name", "Test"], ["add", "scripts"], ["commit", "-qm", "validator"]]) {
    const result = childProcess.spawnSync("git", argv, { cwd: root, encoding: "utf8", env: { PATH: process.env.PATH, HOME: root, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null" } });
    assert.equal(result.status, 0, result.stderr);
  }
}

function scriptedR2(responses) {
  const calls = [];
  return {
    calls,
    async request(method, bucket, key, body = Buffer.alloc(0), headers = {}) {
      calls.push({ body: Buffer.from(body), bucket, headers, key, method });
      const next = responses.shift();
      assert.notEqual(next, undefined, "unexpected R2 request");
      assert.equal(next.method, method);
      return new Response(next.body ?? null, { status: next.status });
    },
  };
}

test("pins exactly the four accepted validator bytes and closure", () => {
  assert.deepEqual(COREML_VALIDATOR_CLOSURE, {
    files: [
      { path: "scripts/semantic-release-assets.py", sha256: "d5b154c2b04a7fee4cb3e10425f4d60d972771b6faca095bbb87c78d97f4be38" },
      { path: "scripts/semantic_release_assets/__init__.py", sha256: "db8a0c4c76473cb5843c0646d428de4f86bbea5cff21624e1a54087ca9e09f81" },
      { path: "scripts/semantic_release_assets/common.py", sha256: "5b7c6b2e9f134e4f0115a41929b5d2250ac50e1593d84081512d4c383ca6f97d" },
      { path: "scripts/semantic_release_assets/contracts.py", sha256: "a5948c17ef55f7ce2dfc621a56bd97ab436fcdcdec157296b6c318e93b974ccf" },
    ],
    sha256: "e4ff84760632654232f33bf76c7102a3e7a90e8b2f8bdcf8c9f5d90a641331d8",
  });
});

test("rejects a committed validator checkout with a mismatched fixed file", () => {
  withTemporaryDirectory("ctx-coreml-validator-pin-test.", (root) => {
    createGitValidator(root);
    assert.throws(() => runPublicValidator(root, { absolute: ARCHIVE_PATH }, {
      sha256: (body) => crypto.createHash("sha256").update(body).digest("hex"),
      spawnSync: childProcess.spawnSync,
    }), /closure identity differs/u);
  });
});

test("preflight records the exact sidecar identities in the validated handoff", async () => {
  await withTemporaryDirectory("ctx-coreml-evidence-test.", async (root) => {
    const evidence = path.join(root, "repair.json");
    const result = await runPreflight({ archive: ARCHIVE_PATH, evidenceOut: evidence, publicCtxRepo: "/not-used" }, artifactDependencies());
    assert.equal(result.status, "validated");
    assert.equal(result.evidence.validation_handoff.artifact_identity.sidecars.asset_json.sha256, crypto.createHash("sha256").update(assetSidecar()).digest("hex"));
    assert.equal(fs.statSync(evidence).mode & 0o777, 0o600);
  });
});

test("full canonical evidence CAS rejects a changed field with the same reservation and state", () => {
  withTemporaryDirectory("ctx-coreml-cas-test.", (root) => {
    const evidence = path.join(root, "repair.json");
    const initial = { operation: "repair-coreml-semantic-object", reservation_id: "a", state: "reserved", unchanged: "first" };
    writeExclusiveEvidence(evidence, initial);
    fs.writeFileSync(evidence, `${JSON.stringify({ ...initial, unchanged: "changed" }, null, 2)}\n`, { mode: 0o600 });
    assert.throws(() => finalizeEvidence(evidence, initial, { ...initial, state: "validated" }), /changed before finalization/u);
  });
});

test("held sibling transition lock refuses concurrent finalization", () => {
  withTemporaryDirectory("ctx-coreml-lock-test.", (root) => {
    const evidence = path.join(root, "repair.json");
    const initial = { operation: "repair-coreml-semantic-object", reservation_id: "a", state: "reserved" };
    writeExclusiveEvidence(evidence, initial);
    fs.writeFileSync(`${evidence}.transition.lock`, "held\n", { flag: "wx", mode: 0o600 });
    assert.throws(() => finalizeEvidence(evidence, initial, { ...initial, state: "validated" }), /already in progress/u);
    assert.deepEqual(JSON.parse(fs.readFileSync(evidence, "utf8")), initial);
  });
});

test("publisher requires credential-fetch and rechecks changed sidecar identity before R2 credentials", async () => {
  await withTemporaryDirectory("ctx-coreml-publisher-boundary-test.", async (root) => {
    const evidence = path.join(root, "repair.json");
    await runPreflight({ archive: ARCHIVE_PATH, evidenceOut: evidence, publicCtxRepo: "/not-used" }, artifactDependencies());
    let credentialsUsed = false;
    const remote = artifactDependencies({ createR2Request() { credentialsUsed = true; throw new Error("unexpected credentials"); } });
    await assert.rejects(() => runPublisher({ archive: ARCHIVE_PATH, evidenceOut: evidence }, environment(), remote), /not trusted/u);
    assert.equal(credentialsUsed, false);
    await beginCredentials({ archive: ARCHIVE_PATH, evidenceOut: evidence }, artifactDependencies());
    const changed = assetSidecar({ repair_note: "changed after preflight" });
    await assert.rejects(() => runPublisher({ archive: ARCHIVE_PATH, evidenceOut: evidence }, environment(), artifactDependencies({
      files: { ".asset.json": { body: changed, sizeBytes: changed.length } },
      createR2Request() { credentialsUsed = true; throw new Error("unexpected credentials"); },
    })), /not trusted/u);
    assert.equal(credentialsUsed, false);
    assert.equal(JSON.parse(fs.readFileSync(evidence, "utf8")).state, "credential-fetch");
  });
});

test("publisher transitions to publishing before creating an R2 request and preserves failed state", async () => {
  await withTemporaryDirectory("ctx-coreml-publisher-state-test.", async (root) => {
    const evidence = path.join(root, "repair.json");
    await runPreflight({ archive: ARCHIVE_PATH, evidenceOut: evidence, publicCtxRepo: "/not-used" }, artifactDependencies());
    await beginCredentials({ archive: ARCHIVE_PATH, evidenceOut: evidence }, artifactDependencies());
    let stateAtCredentialUse;
    await assert.rejects(() => runPublisher({ archive: ARCHIVE_PATH, evidenceOut: evidence }, environment(), artifactDependencies({
      createR2Request() {
        stateAtCredentialUse = JSON.parse(fs.readFileSync(evidence, "utf8")).state;
        return () => undefined;
      },
      putImmutableR2Object() { return Promise.reject(new Error("readback missing")); },
    })), /readback missing/u);
    assert.equal(stateAtCredentialUse, "publishing");
    assert.deepEqual({ state: JSON.parse(fs.readFileSync(evidence, "utf8")).state, failure: JSON.parse(fs.readFileSync(evidence, "utf8")).failure }, { state: "failed", failure: "r2-write-or-readback-failed" });
  });
});

test("bucket mismatch is recorded before R2 request construction", async () => {
  await withTemporaryDirectory("ctx-coreml-bucket-test.", async (root) => {
    const evidence = path.join(root, "repair.json");
    await runPreflight({ archive: ARCHIVE_PATH, evidenceOut: evidence, publicCtxRepo: "/not-used" }, artifactDependencies());
    await beginCredentials({ archive: ARCHIVE_PATH, evidenceOut: evidence }, artifactDependencies());
    let credentialsUsed = false;
    await assert.rejects(() => runPublisher({ archive: ARCHIVE_PATH, evidenceOut: evidence }, environment("wrong-bucket"), artifactDependencies({
      createR2Request() { credentialsUsed = true; throw new Error("unexpected credentials"); },
    })), /bucket differs/u);
    assert.equal(credentialsUsed, false);
    assert.deepEqual({ state: JSON.parse(fs.readFileSync(evidence, "utf8")).state, failure: JSON.parse(fs.readFileSync(evidence, "utf8")).failure }, { state: "failed", failure: "bucket-mismatch" });
  });
});

test("credential failure command accepts only credential-fetch and fixed failure names", async () => {
  await withTemporaryDirectory("ctx-coreml-credential-failure-test.", async (root) => {
    const evidence = path.join(root, "repair.json");
    await runPreflight({ archive: ARCHIVE_PATH, evidenceOut: evidence, publicCtxRepo: "/not-used" }, artifactDependencies());
    await assert.rejects(() => credentialFailed({ evidenceOut: evidence, failure: "access-key-fetch-failed" }, artifactDependencies()), /not fetching credentials/u);
    await beginCredentials({ archive: ARCHIVE_PATH, evidenceOut: evidence }, artifactDependencies());
    await credentialFailed({ evidenceOut: evidence, failure: "access-key-fetch-failed" }, artifactDependencies());
    assert.deepEqual({ state: JSON.parse(fs.readFileSync(evidence, "utf8")).state, failure: JSON.parse(fs.readFileSync(evidence, "utf8")).failure }, { state: "failed", failure: "access-key-fetch-failed" });
  });
});

test("immutable R2 helper permits only exact existing bytes", async () => {
  const transport = scriptedR2([
    { method: "GET", status: 404 }, { method: "PUT", status: 412 }, { method: "GET", status: 200, body: ARCHIVE_BODY },
  ]);
  assert.equal(await putImmutableR2Object(transport.request, COREML_REPAIR.bucket, {
    body: ARCHIVE_BODY, contentType: "application/x-xz", key: COREML_REPAIR.key,
  }), "raced-identical");
  assert.deepEqual(transport.calls.map((call) => call.method), ["GET", "PUT", "GET"]);
});

function expectedWrapperOrder(failure) {
  const secrets = ["CTX_RELEASE_R2_ACCESS_KEY_ID", "CTX_RELEASE_R2_SECRET_ACCESS_KEY", "CTX_RELEASE_R2_ENDPOINT", "CTX_RELEASE_R2_BUCKET"];
  if (failure === "bucket-mismatch") return ["preflight", "begin-credentials", ...secrets, "credential-failed"];
  const name = {
    "access-key-fetch-failed": secrets[0],
    "secret-key-fetch-failed": secrets[1],
    "endpoint-fetch-failed": secrets[2],
    "bucket-fetch-failed": secrets[3],
  }[failure];
  return ["preflight", "begin-credentials", ...secrets.slice(0, secrets.indexOf(name) + 1), "credential-failed"];
}

for (const failure of ["access-key-fetch-failed", "secret-key-fetch-failed", "endpoint-fetch-failed", "bucket-fetch-failed", "bucket-mismatch"]) {
  test(`wrapper stops after ${failure} and durably records the sanitized failure`, () => {
    withTemporaryDirectory("ctx-coreml-wrapper-failure-test.", (root) => {
      const bin = path.join(root, "bin");
      const archive = path.join(root, COREML_REPAIR.archiveBasename);
      const evidence = path.join(root, "evidence.json");
      const order = path.join(root, "order");
      fs.mkdirSync(bin, { recursive: true });
      fs.writeFileSync(archive, "fixture\n");
      fs.writeFileSync(path.join(bin, "node"), [
        "#!/usr/bin/env bash", "set -euo pipefail", "command=\"$2\"", "evidence=''", "failure=''",
        "for ((i=1; i <= $#; i++)); do current=\"${!i}\"; if [[ \"$current\" == --evidence-out ]]; then j=$((i + 1)); evidence=\"${!j}\"; fi; if [[ \"$current\" == --failure ]]; then j=$((i + 1)); failure=\"${!j}\"; fi; done",
        "test -n \"$evidence\"; order=\"$(dirname \"$evidence\")/order\"; test -z \"${INFISICAL_TOKEN:-}\"; echo \"$command\" >>\"$order\"",
        "case \"$command\" in preflight) printf '{\\\"state\\\":\\\"validated\\\"}\\n' >\"$evidence\" ;; begin-credentials) printf '{\\\"state\\\":\\\"credential-fetch\\\"}\\n' >\"$evidence\" ;; credential-failed) printf '{\\\"state\\\":\\\"failed\\\",\\\"failure\\\":\\\"%s\\\"}\\n' \"$failure\" >\"$evidence\" ;; publish) printf '{\\\"state\\\":\\\"created\\\"}\\n' >\"$evidence\" ;; *) exit 91 ;; esac",
        "",
      ].join("\n"), { mode: 0o755 });
      fs.writeFileSync(path.join(bin, "infisical"), [
        "#!/usr/bin/env bash", "set -euo pipefail", "test \"$1\" = secrets && test \"$2\" = get", "echo \"$3\" >>\"$ORDER\"",
        "if [[ \"${FAIL_SECRET:-}\" == \"$3\" ]]; then exit 44; fi",
        "case \"$3\" in CTX_RELEASE_R2_ACCESS_KEY_ID) printf access ;; CTX_RELEASE_R2_SECRET_ACCESS_KEY) printf secret ;; CTX_RELEASE_R2_ENDPOINT) printf https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com ;; CTX_RELEASE_R2_BUCKET) printf \"${BUCKET_VALUE:-ctx-releases-prod}\" ;; *) exit 92 ;; esac",
        "",
      ].join("\n"), { mode: 0o755 });
      const secretByFailure = {
        "access-key-fetch-failed": "CTX_RELEASE_R2_ACCESS_KEY_ID",
        "secret-key-fetch-failed": "CTX_RELEASE_R2_SECRET_ACCESS_KEY",
        "endpoint-fetch-failed": "CTX_RELEASE_R2_ENDPOINT",
        "bucket-fetch-failed": "CTX_RELEASE_R2_BUCKET",
        "bucket-mismatch": "",
      };
      const result = childProcess.spawnSync("bash", [path.join(ROOT, "scripts/release/repair-coreml-semantic-object.sh"), "--archive", archive, "--evidence-out", evidence, "--public-ctx-repo", root], {
        encoding: "utf8",
        env: { HOME: root, XDG_CONFIG_HOME: path.join(root, "config"), XDG_DATA_HOME: path.join(root, "data"), XDG_CACHE_HOME: path.join(root, "cache"), TMPDIR: root, BUCKET_VALUE: failure === "bucket-mismatch" ? "wrong-bucket" : "ctx-releases-prod", FAIL_SECRET: secretByFailure[failure], INFISICAL_TOKEN: "secret-store-token", ORDER: order, PATH: `${bin}:${process.env.PATH}` },
      });
      assert.notEqual(result.status, 0, result.stderr);
      assert.deepEqual(fs.readFileSync(order, "utf8").trim().split("\n"), expectedWrapperOrder(failure));
      assert.deepEqual(JSON.parse(fs.readFileSync(evidence, "utf8")), { state: "failed", failure });
    });
  });
}

test("wrapper fetches exactly four secrets only after durable credential-fetch and then publishes", () => {
  withTemporaryDirectory("ctx-coreml-wrapper-success-test.", (root) => {
    const bin = path.join(root, "bin");
    const archive = path.join(root, COREML_REPAIR.archiveBasename);
    const evidence = path.join(root, "evidence.json");
    const order = path.join(root, "order");
    fs.mkdirSync(bin, { recursive: true });
    fs.writeFileSync(archive, "fixture\n");
    fs.writeFileSync(path.join(bin, "node"), [
      "#!/usr/bin/env bash", "set -euo pipefail", "command=\"$2\"; evidence=\"${6:-}\"; for ((i=1; i <= $#; i++)); do if [[ \"${!i}\" == --evidence-out ]]; then j=$((i + 1)); evidence=\"${!j}\"; fi; done", "test -z \"${INFISICAL_TOKEN:-}\"; echo \"$command\" >>\"$(dirname \"$evidence\")/order\"", "case \"$command\" in preflight) printf '{\\\"state\\\":\\\"validated\\\"}\\n' >\"$evidence\" ;; begin-credentials) printf '{\\\"state\\\":\\\"credential-fetch\\\"}\\n' >\"$evidence\" ;; publish) printf '{\\\"state\\\":\\\"created\\\"}\\n' >\"$evidence\" ;; *) exit 91 ;; esac", "",
    ].join("\n"), { mode: 0o755 });
    fs.writeFileSync(path.join(bin, "infisical"), [
      "#!/usr/bin/env bash", "set -euo pipefail", "echo \"$3\" >>\"$ORDER\"", "case \"$3\" in CTX_RELEASE_R2_ACCESS_KEY_ID) printf access ;; CTX_RELEASE_R2_SECRET_ACCESS_KEY) printf secret ;; CTX_RELEASE_R2_ENDPOINT) printf https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com ;; CTX_RELEASE_R2_BUCKET) printf ctx-releases-prod ;; *) exit 92 ;; esac", "",
    ].join("\n"), { mode: 0o755 });
    const result = childProcess.spawnSync("bash", [path.join(ROOT, "scripts/release/repair-coreml-semantic-object.sh"), "--archive", archive, "--evidence-out", evidence, "--public-ctx-repo", root], {
      encoding: "utf8", env: { HOME: root, XDG_CONFIG_HOME: path.join(root, "config"), XDG_DATA_HOME: path.join(root, "data"), XDG_CACHE_HOME: path.join(root, "cache"), TMPDIR: root, INFISICAL_TOKEN: "secret-store-token", ORDER: order, PATH: `${bin}:${process.env.PATH}` },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(fs.readFileSync(order, "utf8").trim().split("\n"), ["preflight", "begin-credentials", "CTX_RELEASE_R2_ACCESS_KEY_ID", "CTX_RELEASE_R2_SECRET_ACCESS_KEY", "CTX_RELEASE_R2_ENDPOINT", "CTX_RELEASE_R2_BUCKET", "publish"]);
    assert.deepEqual(JSON.parse(fs.readFileSync(evidence, "utf8")), { state: "created" });
  });
});
