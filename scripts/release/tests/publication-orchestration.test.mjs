import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { SIGNING_KEY_ENVIRONMENT_VARIABLES, SECRET_STORE_AUTH_ENVIRONMENT_VARIABLES } from "../release-signing-boundary.mjs";

const root = fileURLToPath(new URL("../../../", import.meta.url));
function fixture(t) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-public-orchestration-"));
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  for (const name of ["home", "config", "data", "cache", "bin", "semantic", "handoff"]) fs.mkdirSync(path.join(scratch, name));
  const env = { PATH: process.env.PATH, HOME: path.join(scratch, "home"),
    XDG_CONFIG_HOME: path.join(scratch, "config"), XDG_DATA_HOME: path.join(scratch, "data"),
    XDG_CACHE_HOME: path.join(scratch, "cache"), TMPDIR: scratch };
  return { scratch, env };
}

async function exitsBeforeReadingKey(args, env) {
  const child = spawn((process.env.JS_BINARY__NODE_BINARY || process.execPath), args, { env, stdio: ["pipe", "ignore", "pipe"] });
  let stderr = "";
  child.stderr.setEncoding("utf8"); child.stderr.on("data", (chunk) => { stderr += chunk; });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { child.kill("SIGKILL"); reject(new Error("signer waited for key stdin before rejecting input")); }, 4000);
    child.on("error", (error) => { clearTimeout(timer); reject(error); });
    child.on("close", (status) => { clearTimeout(timer); resolve({ status, stderr }); });
    // Leave stdin open and empty: deterministic validation must finish first.
  });
}

function twoCheckoutFixture(t) {
  const { scratch, env } = fixture(t);
  Object.assign(env, { GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: os.devNull,
    TEST_VALIDATOR_CALLS: path.join(scratch, "validator-calls.jsonl") });
  const executor = path.join(scratch, "executor");
  const candidateRepo = path.join(scratch, "candidate-source");
  const factory = path.join(scratch, "factory");
  const handoff = path.join(scratch, "handoff");
  const write = (file, bytes) => {
    fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, bytes);
  };
  const git = (repo, ...args) => {
    const result = spawnSync("git", ["-C", repo, ...args], { env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr); return result.stdout.trim();
  };
  const commit = (repo) => {
    git(repo, "add", ".");
    git(repo, "-c", "user.name=Release fixture", "-c", "user.email=release@example.invalid",
      "-c", "commit.gpgsign=false", "commit", "-m", "authored release fixture");
    return git(repo, "rev-parse", "HEAD");
  };
  const validators = ["scripts/release/released-source-continuity.py",
    "scripts/release/seal-linux-factory-candidate.py", "scripts/release-sbom.py"];
  for (const [repo, version] of [[executor, "2.0.5"], [candidateRepo, "1.6.5"]]) {
    write(path.join(repo, "Cargo.toml"), `[workspace.package]\nversion = "${version}"\n`);
    write(path.join(repo, "crates/ctx-cli/Cargo.toml"), '[package]\nname = "ctx"\nversion.workspace = true\n');
    git(repo, "init", "-b", "main");
  }
  for (const name of ["release-manifest.mjs", "unified-release-inputs.mjs", "managed-pair-release-contract.mjs",
    "managed-pair-release-io.mjs", "release-authority.mjs", "release-signing-boundary.mjs",
    "release-version.cjs", "frozen-cli-bridge.cjs"]) {
    const relative = path.join("scripts/release", name);
    write(path.join(executor, relative), fs.readFileSync(path.join(root, relative)));
  }
  for (const name of ["ctx-managed-pair-release-authority-v1.json", "release-targets-v1.json", "release-version-v1.json"]) {
    write(path.join(executor, "contracts", name), fs.readFileSync(path.join(root, "contracts", name)));
  }
  // Substitute only validators to isolate checkout ownership. These tiny files
  // are not qualified release artifacts; real continuity has offline Git tests.
  for (const relative of validators) {
    write(path.join(candidateRepo, relative), 'raise SystemExit("candidate-owned validator must not execute")\n');
    write(path.join(executor, relative), `import json, os, sys
from pathlib import Path
with open(os.environ["TEST_VALIDATOR_CALLS"], "a") as output:
    output.write(json.dumps([str(Path(__file__).resolve()), *sys.argv[1:]]) + "\\n")
if Path(__file__).name == "released-source-continuity.py" and os.environ.get("TEST_REJECT_GATE"):
    raise SystemExit("executor-owned continuity rejected fixture")
if Path(__file__).name == "release-sbom.py":
    print(sys.argv[sys.argv.index("--expected-handoff-sha256") + 1])
`);
  }
  const executorCommit = commit(executor);
  let sourceCommit = commit(candidateRepo);
  const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
  const matrixPath = path.join(executor, "contracts/release-targets-v1.json");
  const candidatePath = path.join(scratch, "candidate.json");
  const outputDir = path.join(scratch, "output");
  const inputs = (overrides = {}) => {
    const files = [];
    for (const name of ["ctx-linux-aarch64", "ctx", "ctx-macos-arm64", "ctx-macos-x64", "ctx.exe"]) {
      for (const [file, bytes] of [[name, `authored ${name} executable fixture`], [`${name}.candidate.json`, "{}"]]) {
        write(path.join(factory, file), bytes);
        files.push({ file, sha256: digest(bytes), size_bytes: Buffer.byteLength(bytes) });
      }
    }
    const factoryBytes = JSON.stringify({ source_commit: overrides.factorySource ?? sourceCommit,
      version: overrides.factoryVersion ?? "1.6.5", files });
    write(path.join(factory, "ctx-release-factory.json"), factoryBytes);
    const validation = JSON.stringify({ validation_policy: "native-receipts-required-v1" });
    write(path.join(handoff, "release-validation.json"), validation);
    const authority = JSON.stringify({ source_commit: overrides.handoffSource ?? sourceCommit,
      factory_manifest: { sha256: digest(factoryBytes) }, validation: { sha256: digest(validation) } });
    write(path.join(handoff, "ctx-core-github-handoff.json"), authority);
    write(candidatePath, JSON.stringify({ contract: "ctx-managed-pair-release-candidate", schema_version: 1,
      channel: "stable", release_name: overrides.releaseName ?? "v1.6.5", rollback_generation: 27,
      target_matrix_sha256: digest(fs.readFileSync(matrixPath)) }));
    return [path.join(executor, "scripts/release/release-manifest.mjs"), "--candidate", candidatePath,
      "--output-dir", outputDir, "--factory-dir", factory, "--candidate-manifest-handoff", handoff,
      "--candidate-handoff-sha256", digest(authority), "--target-matrix", matrixPath];
  };
  return { env, executor, executorCommit, candidateRepo, sourceCommit, outputDir, inputs, validators,
    calls: () => fs.readFileSync(env.TEST_VALIDATOR_CALLS, "utf8").trim().split("\n").map((line) => JSON.parse(line)),
    changeSourceVersion: () => {
      write(path.join(candidateRepo, "Cargo.toml"), '[workspace.package]\nversion = "1.6.6"\n');
      sourceCommit = commit(candidateRepo);
    },
  };
}

test("explicit candidate checkout owns identity while executor owns all validators", (t) => {
  const f = twoCheckoutFixture(t);
  const args = [...f.inputs(), "--public-ctx-repo", f.candidateRepo, "--preflight-only"];
  const result = spawnSync(process.env.JS_BINARY__NODE_BINARY || process.execPath, args, { env: f.env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const receipt = JSON.parse(result.stdout);
  assert.equal(receipt.status, "prepared"); assert.equal(receipt.source_commit, f.sourceCommit);
  assert.notEqual(receipt.source_commit, f.executorCommit);
  assert.equal(receipt.targets, 5); assert.equal(receipt.components, 10);
  const calls = f.calls();
  assert.deepEqual(calls.map((call) => call[0]), f.validators.map((relative) => path.join(f.executor, relative)));
  assert.deepEqual(calls[0].slice(1), ["--public-repo", f.candidateRepo, "--source-commit", f.sourceCommit]);
  assert.equal(calls[1].at(-1), f.sourceCommit);
  assert.equal(fs.existsSync(f.outputDir), false);
});

test("executor gate and candidate evidence mismatches reject before signing key stdin", async (t) => {
  const f = twoCheckoutFixture(t);
  const explicit = ["--public-ctx-repo", f.candidateRepo];
  const gate = await exitsBeforeReadingKey([...f.inputs(), ...explicit], { ...f.env, TEST_REJECT_GATE: "1" });
  assert.notEqual(gate.status, 0); assert.match(gate.stderr, /executor-owned continuity rejected fixture/);
  assert.equal(f.calls().length, 1);
  for (const [overrides, message] of [
    [{ factorySource: f.executorCommit }, /exact unified/],
    [{ handoffSource: f.executorCommit }, /factory and staged public handoff differ/],
    [{ factoryVersion: "2.0.5", releaseName: "v2.0.5" }, /exact unified/],
    [{ releaseName: "v1.6.6" }, /exact unified/],
  ]) {
    const failed = await exitsBeforeReadingKey([...f.inputs(overrides), ...explicit], f.env);
    assert.notEqual(failed.status, 0); assert.match(failed.stderr, message);
  }
  // Omitted option retains the executor checkout as the default source.
  const defaulted = await exitsBeforeReadingKey(f.inputs(), f.env);
  assert.notEqual(defaulted.status, 0); assert.match(defaulted.stderr, /exact unified/);
  const lastGate = f.calls().filter((call) => call[0].endsWith("released-source-continuity.py")).at(-1);
  assert.deepEqual(lastGate.slice(1), ["--public-repo", f.executor, "--source-commit", f.executorCommit]);
  fs.writeFileSync(path.join(f.candidateRepo, "untracked"), "dirty candidate");
  const dirty = await exitsBeforeReadingKey([...f.inputs(), ...explicit], f.env);
  assert.notEqual(dirty.status, 0); assert.match(dirty.stderr, /public release source checkout is dirty/);
  fs.unlinkSync(path.join(f.candidateRepo, "untracked"));
  f.changeSourceVersion();
  const version = await exitsBeforeReadingKey([...f.inputs(), ...explicit], f.env);
  assert.notEqual(version.status, 0); assert.match(version.stderr, /exact unified/);
  assert.equal(fs.existsSync(f.outputDir), false);
});

test("actual public signer rejects malformed input and inherited authority before key stdin", async (t) => {
  const { scratch, env } = fixture(t);
  const candidate = path.join(scratch, "candidate.json");
  fs.writeFileSync(candidate, '{"schema_version":1,"schema_version":1}');
  const args = [path.join(root, "scripts/release/release-manifest.mjs"),
    "--candidate", candidate, "--factory-dir", scratch,
    "--candidate-manifest-handoff", path.join(scratch, "handoff"),
    "--candidate-handoff-sha256", "a".repeat(64), "--target-matrix", path.join(root, "contracts/release-targets-v1.json"),
    "--output-dir", path.join(scratch, "output")];
  const invalid = await exitsBeforeReadingKey(args, env);
  assert.notEqual(invalid.status, 0); assert.match(invalid.stderr, /duplicate JSON key/);
  for (const name of [...SIGNING_KEY_ENVIRONMENT_VARIABLES, ...SECRET_STORE_AUTH_ENVIRONMENT_VARIABLES, "R2_ACCESS_KEY_ID"]) {
    const inherited = await exitsBeforeReadingKey(args, { ...env, [name]: "authored-forbidden-fixture" });
    assert.notEqual(inherited.status, 0); assert.match(inherited.stderr, /inherited forbidden authority/);
  }
  assert.equal(fs.existsSync(path.join(scratch, "output")), false);
});

test("actual hosted wrapper preflights before secret lookup or publication", (t) => {
  const { scratch, env } = fixture(t);
  const bin = path.join(scratch, "bin");
  // Only preflight is substituted. Any signing, publication, or secret lookup fails.
  fs.writeFileSync(path.join(bin, "node"), `#!/usr/bin/env bash
set -eu
[[ "$1" == scripts/release/publish-hosted-managed-pair-stable.mjs && "$2" == prepare ]]
printf 'prepare\\n' >> "$TEST_CALLS"
[[ "\${TEST_PREPARE_FAIL:-0}" == 0 ]] || exit 23
while [[ $# -gt 0 ]]; do
  if [[ "$1" == --metadata-out ]]; then printf 'authored fixture\\n' > "$2"; break; fi
  shift
done
`, { mode: 0o700 });
  fs.writeFileSync(path.join(bin, "infisical"), '#!/usr/bin/env bash\nprintf "forbidden\\n" >> "$TEST_SECRETS"\nexit 91\n', { mode: 0o700 });
  for (const name of ["publication.json", "runtime.json"]) fs.writeFileSync(path.join(scratch, name), "{}");
  const calls = path.join(scratch, "calls"); const secrets = path.join(scratch, "secrets");
  const args = [path.join(root, "scripts/release/publish-hosted-managed-pair-stable.sh"),
    "--publication", path.join(scratch, "publication.json"), "--runtime-handoff", path.join(scratch, "runtime.json"),
    "--semantic-artifact-dir", path.join(scratch, "semantic"), "--candidate-manifest-handoff", path.join(scratch, "handoff"),
    "--candidate-handoff-sha256", "a".repeat(64), "--public-ctx-repo", root,
    "--published-at", "2026-09-20T00:00:00Z"];
  const run = (extra, fail) => spawnSync("bash", [...args, "--work-dir", path.join(scratch, fail ? "failed" : "preflight"), ...extra], {
    encoding: "utf8", env: { ...env, PATH: `${bin}:${env.PATH}`, TEST_CALLS: calls, TEST_SECRETS: secrets,
      TEST_PREPARE_FAIL: fail ? "1" : "0" },
  });
  const passed = run(["--preflight-only"], false);
  assert.equal(passed.status, 0, passed.stderr);
  const failed = run([], true);
  assert.equal(failed.status, 23, failed.stderr);
  assert.equal(fs.readFileSync(calls, "utf8"), "prepare\nprepare\n");
  assert.equal(fs.existsSync(secrets), false);
});
