import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
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
