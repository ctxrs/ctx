#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import worker from "./src/index.js";
import {
  approvedRelease, assertSameInstaller, readBounded, readReport, sha256, sourceIdentity,
  validateNativeResults, validateUnixResult, verifyCurrentFeed, isUnifiedVersion,
} from "./src/install-deploy-checks.mjs";

const serviceRoot = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(serviceRoot, "../..");
const shellUrl = "https://ctx.rs/install";
const powershellUrl = "https://ctx.rs/install.ps1";

function save(file, value) {
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}

async function rendered() {
  const shell = await worker.fetch(new Request(shellUrl));
  const powershell = await worker.fetch(new Request(powershellUrl));
  if (!shell.ok || !powershell.ok) throw new Error("proposed production routes did not render");
  return { shell: Buffer.from(await shell.arrayBuffer()), powershell: Buffer.from(await powershell.arrayBuffer()) };
}

export async function prepare(directory, releaseEvidence) {
  approvedRelease(releaseEvidence);
  const releaseBytes = readBounded(releaseEvidence, 64 * 1024);
  fs.mkdirSync(directory, { mode: 0o700 }); // Never silently replace a retained candidate.
  const scripts = await rendered();
  fs.writeFileSync(path.join(directory, "install.sh"), scripts.shell, { mode: 0o600 });
  fs.writeFileSync(path.join(directory, "install.ps1"), scripts.powershell, { mode: 0o600 });
  fs.writeFileSync(path.join(directory, "approved-release.json"), releaseBytes, { mode: 0o600 });
  const candidate = {
    schema_version: 1, source_sha256: sourceIdentity(serviceRoot),
    shell_sha256: sha256(scripts.shell), powershell_sha256: sha256(scripts.powershell),
    release_evidence_sha256: sha256(releaseBytes),
  };
  save(path.join(directory, "candidate.json"), candidate);
  process.stdout.write(`Prepared production installers in ${directory}\n`);
}

export async function candidateAt(directory) {
  const candidate = readReport(path.join(directory, "candidate.json"));
  const shell = readBounded(path.join(directory, "install.sh"));
  const powershell = readBounded(path.join(directory, "install.ps1"));
  const releasePath = path.join(directory, "approved-release.json");
  if (candidate.schema_version !== 1 || candidate.source_sha256 !== sourceIdentity(serviceRoot)
      || candidate.shell_sha256 !== sha256(shell) || candidate.powershell_sha256 !== sha256(powershell)
      || candidate.release_evidence_sha256 !== sha256(readBounded(releasePath, 64 * 1024))) {
    throw new Error("candidate scripts or source changed; prepare and qualify again");
  }
  const current = await rendered();
  assertSameInstaller(current.shell, shell);
  assertSameInstaller(current.powershell, powershell, true);
  return { ...candidate, release: approvedRelease(releasePath) };
}

function runLinux(directory, script, phase, expected) {
  const resultPath = path.join(directory, `${phase}-linux.json`);
  const environment = {
    ...process.env, CTX_PUBLIC_CTX_REPO: repositoryRoot, CTX_INSTALL_SMOKE_SCRIPT: script, CTX_INSTALL_SMOKE_RESULT: resultPath,
  };
  if (expected) {
    environment.CTX_INSTALL_SMOKE_EXPECTED_VERSION = expected.version;
    environment.CTX_INSTALL_SMOKE_EXPECTED_CORE_SHA256 = expected.core_sha256;
    if (!isUnifiedVersion(expected.version)) environment.CTX_INSTALL_SMOKE_EXPECTED_PRO_SHA256 = expected.pro_sha256;
    else delete environment.CTX_INSTALL_SMOKE_EXPECTED_PRO_SHA256;
  }
  const run = spawnSync("bash", [path.join(serviceRoot, "tests/install_live_smoke.sh")], {
    cwd: repositoryRoot, env: environment, stdio: "inherit", timeout: 310_000,
  });
  if (run.error || run.status !== 0) {
    throw new Error(`${phase} real installation failed; retained result: ${resultPath}`);
  }
  return validateUnixResult(readReport(resultPath), {
    platform: "linux-x64", installerSha256: sha256(readBounded(script)), version: expected?.version,
    coreSha256: expected?.core_sha256, proSha256: expected && !isUnifiedVersion(expected.version) ? expected.pro_sha256 : undefined,
  });
}

export function runCandidateFixtures(directory) {
  const reportPath = path.join(directory, "candidate-fixtures.tap");
  const output = fs.openSync(reportPath, "w", 0o600);
  let run;
  try {
    run = spawnSync(process.execPath, ["src/run-node-test-suite.mjs", "--test-reporter=tap", "src/test/cli-install-foss.test.mjs"], {
      cwd: serviceRoot, env: { ...process.env, CTX_REQUIRE_POWERSHELL: "1" },
      stdio: ["ignore", output, output], timeout: 180_000,
    });
  } finally { fs.closeSync(output); }
  if (run.error || run.status !== 0) throw new Error(`unpublished 1.5 fixture checks failed: ${reportPath}`);
  const report = readBounded(reportPath);
  if (!/^# skipped 0$/mu.test(report.toString("utf8"))) throw new Error("candidate fixture checks were skipped");
  return { kind: "unpublished-candidate-fixtures", version: "1.5.0", status: "passed", sha256: sha256(report) };
}

async function readback(directory, candidate) {
  const scripts = {};
  for (const [name, url, powershell] of [
    ["install.sh", shellUrl, false], ["install.ps1", powershellUrl, true],
  ]) {
    const response = await fetch(url, { signal: AbortSignal.timeout(30_000), redirect: "error" });
    if (!response.ok) throw new Error(`live installer readback returned HTTP ${response.status}`);
    const chunks = [];
    let length = 0;
    for await (const chunk of response.body) {
      length += chunk.length;
      if (length > 1024 * 1024) throw new Error("live installer exceeds size limit");
      chunks.push(chunk);
    }
    const bytes = Buffer.concat(chunks);
    fs.writeFileSync(path.join(directory, `live-${name}`), bytes, { mode: 0o600 });
    assertSameInstaller(bytes, readBounded(path.join(directory, name)), powershell);
    scripts[name] = sha256(bytes);
  }
  await candidateAt(directory);
  return { candidate, scripts };
}

export function deploymentExitCode(result) {
  if (Number.isInteger(result.status) && result.status > 0) return result.status;
  return !result.error && !result.signal && result.status === 0 ? 0 : 1;
}

export async function runDeployment({ directory, nativeDirectory, apply }, hooks = {}) {
  const load = hooks.candidateAt ?? candidateAt;
  const install = hooks.runLinux ?? runLinux;
  const native = hooks.validateNativeResults ?? validateNativeResults;
  const record = hooks.save ?? save;
  const result = { schema_version: 1, status: "qualification_started", started_at: new Date().toISOString() };
  record(path.join(directory, "deployment.json"), result);
  let candidate;
  let expected;
  try {
    candidate = await load(directory);
    result.current_feed = await (hooks.verifyCurrentFeed ?? verifyCurrentFeed)(candidate.release);
    result.candidate_fixtures = (hooks.runCandidateFixtures ?? runCandidateFixtures)(directory);
    expected = { version: candidate.release.version, ...candidate.release.pairs["linux-x64"] };
    const local = install(directory, path.join(directory, "install.sh"), "candidate", expected);
    result.candidate = candidate;
    result.native_results = native(nativeDirectory, candidate, local);
    await load(directory); // Do not deploy source edited during qualification.
    await (hooks.verifyCurrentFeed ?? verifyCurrentFeed)(candidate.release);
    result.status = "qualified";
  } catch (error) {
    result.status = "qualification_failed";
    result.error = error.message;
    record(path.join(directory, "deployment.json"), result);
    throw error;
  }
  record(path.join(directory, "deployment.json"), result);
  if (!apply) return result;

  // This deployment still uses the current feed, which may remain 1.4.
  // A later release operator runs live 1.5 checks only after v2 exposure.
  result.status = "activation_started";
  record(path.join(directory, "deployment.json"), result);
  const invoke = hooks.deploy ?? (() => spawnSync(path.join(serviceRoot, "node_modules/.bin/wrangler"),
    ["deploy", "--env="], { cwd: serviceRoot, stdio: "inherit", timeout: 180_000 }));
  const deployment = invoke();
  result.wrangler_exit_code = deploymentExitCode(deployment);
  result.wrangler_error = deployment.error?.code ?? null;
  // Wrangler may activate traffic and then fail. Always inspect the served state.
  try {
    result.readback = await (hooks.readback ?? readback)(directory, candidate);
    result.live_result = install(directory, path.join(directory, "live-install.sh"), "live", expected);
    result.status = result.wrangler_exit_code === 0 ? "passed" : "activation_requires_reconciliation";
  } catch (error) {
    result.status = "activation_requires_reconciliation";
    result.error = error.message;
  }
  record(path.join(directory, "deployment.json"), result);
  if (result.status !== "passed") {
    throw new Error(`deployment requires reconciliation; do not assume traffic was unchanged: ${directory}/deployment.json`);
  }
  return result;
}

async function main(argv) {
  const [action, rawDirectory, flag, rawInput, ...extra] = argv;
  if (!["prepare", "check", "apply"].includes(action) || !rawDirectory
      || extra.length || (action === "prepare" ? flag !== "--release-evidence" || !rawInput
        : flag !== undefined && (flag !== "--native-results" || !rawInput))) {
    throw new Error("usage: deploy.mjs prepare DIR --release-evidence FILE | {check|apply} DIR [--native-results DIR]");
  }
  const directory = path.resolve(rawDirectory);
  if (action === "prepare") return prepare(directory, path.resolve(rawInput));
  if (process.platform !== "linux" || process.arch !== "x64") {
    throw new Error("installer deployment gate must run on the Linux x64 owner");
  }
  if (process.env.CTX_PUBLIC_RELEASE_SKIP_INSTALL_SMOKE === "1"
      || process.env.CTX_INSTALL_LIFECYCLE_CTX_BINARY) {
    throw new Error("fixture execution or skipped installation cannot qualify deployment");
  }
  const result = await runDeployment({
    directory, nativeDirectory: rawInput === undefined ? undefined : path.resolve(rawInput), apply: action === "apply",
  });
  process.stdout.write(`Installer ${action}: ${result.status}; retained evidence: ${directory}\n`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`installer deployment: ${error.message}\n`);
    process.exitCode = 1;
  });
}
