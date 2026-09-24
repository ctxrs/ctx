import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { candidateAt, deploymentExitCode, prepare, runDeployment } from "../../deploy.mjs";
import {
  approvedRelease, assertSameInstaller, installerBody, sha256, validateNativeResults,
  validateUnixResult, validateWindowsResult,
} from "../install-deploy-checks.mjs";

const digest = "a".repeat(64);
const otherDigest = "b".repeat(64);
const version = "1.3.3";
const pairs = Object.fromEntries(["linux-x64", "linux-aarch64", "macos-x64", "macos-arm64", "windows-x64"]
  .map((platform) => [platform, { core_sha256: digest, pro_sha256: otherDigest }]));
const candidate = { shell_sha256: digest, powershell_sha256: otherDigest, release: { version, pairs } };

function unixResult(platform = "linux-x64") {
  // Deliberately independent literal expected checks: do not import the production list.
  return {
    schema_version: 1, status: "passed", mode: "candidate", platform,
    installer_sha256: digest, version, core_sha256: digest, pro_sha256: otherDigest,
    elapsed_seconds: 12, checks: {
      core_installed: true, pro_installed: true, managed: true, setup: true,
      daemon: true, path: true, search: true,
    },
  };
}

function windowsResult() {
  return {
    stage: "windows_live_installer", status: "passed", release_authority: false,
    installer_sha256: otherDigest, version,
    shells: ["5.1.20348.1", "7.5.2"].map((powershell) => ({
      powershell, loaded_profile: true, version,
      passed: ["fresh", "managed-reinstall"].map((phase) => ({
        phase, exit_code: 0, core_sha256: digest, pro_sha256: otherDigest,
      })),
    })),
  };
}

test("readback only tolerates the generated request identifier", () => {
  const body = 'install_attempt_id="ia_abcdefgh"\n"$artifact" --ctx-core-managed-pair-apply-v1\n';
  const repeated = body.replace("ia_abcdefgh", "ia_anotherid");
  assertSameInstaller(repeated, body);
  assert.notEqual(sha256(body), sha256(repeated));
  assert.throws(() => assertSameInstaller(body.replace("apply-v1", "apply-v2"), body));
  assert.throws(() => assertSameInstaller(body.replace("$artifact", "$other"), body));
  assert.throws(() => installerBody(body + 'install_attempt_id="ia_secondone"\n'));
  assert.throws(() => installerBody("no request identity"));
  const ps = '$installAttemptId = "ia_abcdefgh"\r\nInvoke-Installer\r\n';
  // Match the response's newline convention without changing it.
  assertSameInstaller(ps.replace("ia_abcdefgh", "ia_anotherid"), ps, true);
  assert.throws(() => assertSameInstaller(ps.replace("Invoke-Installer", "Skip-Installer"), ps, true));
});

test("a zero exit/version check cannot replace usable-install evidence", () => {
  const expected = { platform: "linux-x64", installerSha256: digest, version };
  assert.equal(validateUnixResult(unixResult(), expected).version, version);
  for (const check of ["core_installed", "pro_installed", "managed", "setup", "daemon", "path", "search"]) {
    const result = unixResult();
    result.checks[check] = false;
    assert.throws(() => validateUnixResult(result, expected), new RegExp("installation checks"));
  }
  for (const [field, value] of [
    ["status", "skipped"], ["platform", "macos-x64"], ["version", "1.3.1"],
    ["installer_sha256", otherDigest], ["pro_sha256", ""], ["elapsed_seconds", -1],
  ]) {
    assert.throws(() => validateUnixResult({ ...unixResult(), [field]: value }, expected));
  }
});

test("Windows proof requires both real shell editions and both lifecycle phases", () => {
  const expected = { installerSha256: otherDigest, version };
  validateWindowsResult(windowsResult(), expected);
  const variants = [
    (result) => { result.shells.pop(); },
    (result) => { result.shells[1].powershell = result.shells[0].powershell; },
    (result) => { result.shells[0].loaded_profile = false; },
    (result) => { result.shells[0].passed.pop(); },
    (result) => { result.shells[1].passed[1].exit_code = 1; },
    (result) => { result.shells[1].passed[1].pro_sha256 = digest; },
    (result) => { result.installer_sha256 = digest; },
    (result) => { result.version = "1.3.1"; },
  ];
  for (const mutate of variants) {
    const result = windowsResult();
    mutate(result);
    assert.throws(() => validateWindowsResult(result, expected));
  }
});

test("omitted Windows evidence is not_run while Linux proof remains mandatory", () => {
  for (const directory of [undefined, null]) {
    const results = validateNativeResults(directory, candidate, unixResult());
    assert.deepEqual(results["windows-x64"], { status: "not_run", reason: "not_supplied" });
    assert.deepEqual(results["linux-x64"], unixResult());
    assert.throws(() => validateNativeResults(directory, candidate, undefined), /installation checks/);
    const incomplete = unixResult();
    incomplete.checks.search = false;
    assert.throws(() => validateNativeResults(directory, candidate, incomplete), /installation checks/);
    assert.throws(() => validateNativeResults(directory, candidate, { ...unixResult(), core_sha256: otherDigest }), /installation checks/);
  }
});

test("an explicit Windows directory requires complete evidence against the approved pair", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-deploy-native-"));
  try {
    assert.throws(() => validateNativeResults(directory, candidate, unixResult()));
    for (const report of ["not json", JSON.stringify({ status: "not_run" }), JSON.stringify({ ...windowsResult(), status: "failed" })]) {
      fs.writeFileSync(path.join(directory, "windows-x64.json"), report);
      assert.throws(() => validateNativeResults(directory, candidate, unixResult()));
    }
    fs.writeFileSync(path.join(directory, "windows-x64.json"), JSON.stringify(windowsResult()));
    assert.deepEqual(validateNativeResults(directory, candidate, unixResult())["windows-x64"], windowsResult());
    const wrong = { ...candidate, release: { version: "1.3.4", pairs } };
    assert.throws(() => validateNativeResults(directory, wrong, unixResult()));
    const changed = structuredClone(candidate);
    changed.release.pairs["windows-x64"].pro_sha256 = digest;
    assert.throws(() => validateNativeResults(directory, changed, unixResult()));
    fs.unlinkSync(path.join(directory, "windows-x64.json"));
    assert.throws(() => validateNativeResults(directory, candidate, unixResult()));
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("deployment CLI accepts omitted Windows evidence but rejects incomplete options", {
  skip: process.platform !== "linux" || process.arch !== "x64",
}, () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-deploy-argv-"));
  try {
    const script = new URL("../../deploy.mjs", import.meta.url);
    for (const action of ["check", "apply"]) {
      for (const options of [[], ["--native-results", directory]]) {
        // Empty candidate stops before network, fixtures, installation or activation.
        const result = spawnSync(process.execPath, [script.pathname, action, directory, ...options], {
          encoding: "utf8", timeout: 10_000,
        });
        assert.equal(result.error, undefined);
        assert.equal(result.status, 1);
        assert.match(result.stderr, /candidate\.json/);
        assert.doesNotMatch(result.stderr, /usage:/);
      }
      for (const options of [["--native-results"], ["--native-results", ""], ["--unknown", directory], ["--native-results", directory, "extra"]]) {
        const result = spawnSync(process.execPath, [script.pathname, action, directory, ...options], {
          encoding: "utf8", timeout: 10_000,
        });
        assert.equal(result.status, 1);
        assert.match(result.stderr, /usage:/);
      }
    }
    const missingRelease = spawnSync(process.execPath, [script.pathname, "prepare", directory], {
      encoding: "utf8", timeout: 10_000,
    });
    assert.equal(missingRelease.status, 1);
    assert.match(missingRelease.stderr, /usage:/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

function releaseEvidence() {
  return {
    schema_version: 1, kind: "public-cli-release-contract", status: "passed",
    release: { channel: "stable", version, source_commit: "c".repeat(40) },
    public_source: { commit: "c".repeat(40), worktree_clean_checked: true, remote_main_checked: true },
    metadata: {
      stable: { signature_verified: true, sha256: digest },
      versioned: { signature_verified: true, sha256: digest }, managed_pair: pairs,
    },
  };
}

test("approved release input requires signed pair identities for every platform", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-approved-release-"));
  const file = path.join(directory, "release.json");
  const evidence = releaseEvidence();
  try {
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.equal(approvedRelease(file).version, version);
    evidence.public_source.remote_main_checked = false;
    evidence.public_source.release_tag_checked = true;
    evidence.public_source.release_tag = `v${version}`;
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.equal(approvedRelease(file).version, version);
    evidence.public_source.release_tag = "v1.4.12";
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /passed approved/);
    evidence.public_source.remote_main_checked = true;
    evidence.public_source.release_tag_checked = false;
    evidence.metadata.managed_pair = { "linux-x64": pairs["linux-x64"] };
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /missing the signed pair/);
    evidence.status = "skipped";
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /passed approved/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("published current-feed evidence requires exact tag and readback, without factory claims", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-current-installer-feed-"));
  const file = path.join(directory, "release.json");
  const evidence = releaseEvidence();
  evidence.kind = "public-cli-current-installer-feed";
  evidence.public_source.remote_main_checked = false;
  evidence.public_source.release_tag_checked = true;
  evidence.public_source.release_tag = `v${version}`;
  evidence.current_feed_asset_sha256s = { sha256: digest };
  evidence.validation = { construction: "not_run", publication_readback: "passed" };
  try {
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.equal(approvedRelease(file).version, version);
    evidence.public_source.release_tag_checked = false;
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /passed approved/);
    evidence.public_source.release_tag_checked = true;
    evidence.validation.publication_readback = "not_run";
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /passed approved/);
    evidence.validation.publication_readback = "passed";
    evidence.validation.construction = "passed";
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /passed approved/);
    evidence.validation.construction = "not_run";
    evidence.candidate_manifests = {};
    fs.writeFileSync(file, JSON.stringify(evidence));
    assert.throws(() => approvedRelease(file), /passed approved/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("actual retained candidate rejects script and release evidence mutations", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-prepared-installer-"));
  try {
    const release = path.join(directory, "release.json");
    fs.writeFileSync(release, JSON.stringify(releaseEvidence()));
    const retained = path.join(directory, "candidate");
    await prepare(retained, release);
    assert.equal((await candidateAt(retained)).release.version, version);
    for (const file of ["install.sh", "install.ps1", "approved-release.json"]) {
      const target = path.join(retained, file);
      const original = fs.readFileSync(target);
      fs.appendFileSync(target, "\nchanged");
      await assert.rejects(candidateAt(retained), /candidate scripts or source changed/);
      fs.writeFileSync(target, original);
    }
    const candidateFile = path.join(retained, "candidate.json");
    const record = JSON.parse(fs.readFileSync(candidateFile));
    record.source_sha256 = otherDigest;
    fs.writeFileSync(candidateFile, JSON.stringify(record));
    await assert.rejects(candidateAt(retained), /candidate scripts or source changed/);
    await assert.rejects(prepare(retained, release), /EEXIST/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("Wrangler launch, timeout and signal errors cannot be hidden by zero status", () => {
  for (const [result, expected] of [
    [{ status: 0 }, 0], [{ status: 0, error: new Error("timeout") }, 1],
    [{ status: 7, error: new Error("failed") }, 7], [{ status: null }, 1],
    [{ status: 0, signal: "SIGTERM" }, 1], [{}, 1], [{ status: "0" }, 1], [{ status: -1 }, 1],
  ]) assert.equal(deploymentExitCode(result), expected);
});

function execution(overrides = {}) {
  const calls = [];
  const records = [];
  return {
    calls, records,
    hooks: {
      verifyCurrentFeed: async () => { calls.push("feed"); return { version, signature_verified: true }; },
      runCandidateFixtures: () => { calls.push("fixtures"); return { kind: "unpublished-candidate-fixtures", status: "passed" }; },
      candidateAt: async () => { calls.push("candidate"); return candidate; },
      runLinux: (_directory, _script, phase) => { calls.push(phase); return unixResult(); },
      validateNativeResults: () => { calls.push("native"); return { checked: true }; },
      save: (_file, value) => records.push(structuredClone(value)),
      deploy: () => { calls.push("deploy"); return { status: 0 }; },
      readback: async () => { calls.push("readback"); return { scripts: {} }; },
      ...overrides,
    },
  };
}
const inputs = { directory: "/retained", nativeDirectory: "/native", apply: true };

test("check and apply can omit Windows while retaining Linux, fixtures and both feed checks", async () => {
  for (const apply of [false, true]) {
    const { hooks, calls, records } = execution({ validateNativeResults });
    const result = await runDeployment({ directory: inputs.directory, apply }, hooks);
    assert.equal(result.status, apply ? "passed" : "qualified");
    assert.deepEqual(result.native_results["windows-x64"], { status: "not_run", reason: "not_supplied" });
    assert.deepEqual(result.native_results["linux-x64"], unixResult());
    assert.deepEqual(records.at(-1).native_results, result.native_results);
    assert.deepEqual(calls, ["candidate", "feed", "fixtures", "candidate", "candidate", "feed",
      ...(apply ? ["deploy", "readback", "live"] : [])]);
  }
});

test("omitting Windows cannot waive failed Linux or the second current-feed check", async () => {
  const withoutWindows = { directory: inputs.directory, apply: true };
  const linux = execution({
    validateNativeResults,
    runLinux: () => ({ ...unixResult(), status: "failed" }),
  });
  await assert.rejects(runDeployment(withoutWindows, linux.hooks), /installation checks/);
  assert.ok(!linux.calls.includes("deploy"));
  assert.equal(linux.records.at(-1).status, "qualification_failed");
  let reads = 0;
  const feed = execution({
    validateNativeResults,
    verifyCurrentFeed: async () => {
      if (++reads === 2) throw new Error("current feed changed during qualification");
      return { version, signature_verified: true };
    },
  });
  await assert.rejects(runDeployment(withoutWindows, feed.hooks), /current feed changed/);
  assert.equal(reads, 2);
  assert.ok(!feed.calls.includes("deploy"));
  assert.equal(feed.records.at(-1).status, "qualification_failed");
});

test("a supplied bad Windows receipt blocks activation through the real validator", async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-deploy-bad-windows-"));
  try {
    fs.writeFileSync(path.join(directory, "windows-x64.json"), JSON.stringify({ ...windowsResult(), status: "failed" }));
    const { hooks, calls, records } = execution({ validateNativeResults });
    await assert.rejects(runDeployment({ ...inputs, nativeDirectory: directory }, hooks), /Windows installation result/);
    assert.ok(!calls.includes("deploy"));
    assert.equal(records.at(-1).status, "qualification_failed");
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test("incompatible real installer stops before deployment", async () => {
  const { hooks, calls, records } = execution({
    runLinux: () => { throw new Error("released binary rejects unknown operation"); },
  });
  await assert.rejects(runDeployment(inputs, hooks), /unknown operation/);
  assert.deepEqual(calls, ["candidate", "feed", "fixtures"]);
  assert.equal(records.at(-1).status, "qualification_failed");
});

test("a failed requalification replaces previous success evidence", async () => {
  for (const failAt of ["candidateAt", "runLinux", "validateNativeResults"]) {
    const { hooks, records } = execution();
    await runDeployment(inputs, hooks);
    assert.equal(records.at(-1).status, "passed");
    hooks[failAt] = () => { throw new Error("new attempt failed"); };
    await assert.rejects(runDeployment(inputs, hooks), /new attempt failed/);
    assert.equal(records.at(-1).status, "qualification_failed");
    assert.equal(records.at(-1).error, "new attempt failed");
  }
});

test("source drift during native checks prevents activation", async () => {
  let loads = 0;
  const { hooks, calls } = execution({
    candidateAt: async () => {
      if (loads++) throw new Error("candidate scripts or source changed");
      return candidate;
    },
  });
  await assert.rejects(runDeployment(inputs, hooks), /source changed/);
  assert.ok(!calls.includes("deploy"));
});

test("check mode never activates; apply verifies installation after readback", async () => {
  const first = execution();
  assert.equal((await runDeployment({ ...inputs, apply: false }, first.hooks)).status, "qualified");
  assert.ok(!first.calls.includes("deploy"));
  const second = execution();
  assert.equal((await runDeployment(inputs, second.hooks)).status, "passed");
  assert.deepEqual(second.calls, ["candidate", "feed", "fixtures", "candidate", "native", "candidate", "feed", "deploy", "readback", "live"]);
});

test("Wrangler failure after activation still triggers readback and cannot report success", async () => {
  const { hooks, calls, records } = execution({ deploy: () => ({ status: 1 }) });
  await assert.rejects(runDeployment(inputs, hooks), /do not assume traffic was unchanged/);
  assert.ok(calls.includes("readback"));
  assert.ok(calls.includes("live"));
  assert.equal(records.at(-1).status, "activation_requires_reconciliation");
  assert.equal(records.at(-1).wrangler_exit_code, 1);
});

test("successful HTTP delivery with failing live installation is not deployment success", async () => {
  const { hooks, records } = execution({
    runLinux: (_directory, _script, phase) => {
      if (phase === "live") throw new Error("setup failed");
      return unixResult();
    },
  });
  await assert.rejects(runDeployment(inputs, hooks), /requires reconciliation/);
  assert.equal(records.at(-1).error, "setup failed");
});

test("1.5 live proof requires one real binary and does not invent a Pro digest", () => {
  const unix = unixResult();
  unix.version = "1.5.0";
  unix.pro_sha256 = null;
  delete unix.checks.pro_installed;
  unix.checks.single_binary = true;
  validateUnixResult(unix, { platform: "linux-x64", installerSha256: digest, version: "1.5.0" });
  assert.throws(() => validateUnixResult({ ...unix, pro_sha256: digest }, {
    platform: "linux-x64", installerSha256: digest, version: "1.5.0",
  }));
  const windows = windowsResult();
  windows.version = "1.5.0";
  for (const shell of windows.shells) {
    shell.version = "1.5.0";
    for (const phase of shell.passed) { phase.pro_sha256 = null; phase.single_binary = true; }
  }
  validateWindowsResult(windows, { installerSha256: otherDigest, version: "1.5.0" });
  windows.shells[0].passed[0].single_binary = false;
  assert.throws(() => validateWindowsResult(windows, { installerSha256: otherDigest, version: "1.5.0" }));
});

test("current-feed mismatch and failed unpublished fixtures block deployment", async () => {
  for (const operation of ["verifyCurrentFeed", "runCandidateFixtures"]) {
    for (const nativeDirectory of [inputs.nativeDirectory, undefined]) {
      const { hooks, calls, records } = execution({ [operation]: () => { throw new Error("unproved input"); } });
      await assert.rejects(runDeployment({ ...inputs, nativeDirectory }, hooks), /unproved input/);
      assert.equal(calls.includes("deploy"), false);
      assert.equal(records.at(-1).status, "qualification_failed");
    }
  }
});

test("current-feed signature/hash check accepts an exact signed fixture and rejects stale proof", async () => {
  const { verifyCurrentFeed } = await import("../install-deploy-checks.mjs");
  const metadata = fs.readFileSync(new URL("./released-command-compatibility-v1.3.1.env", import.meta.url));
  const signature = fs.readFileSync(new URL("./released-command-compatibility-v1.3.1.sig", import.meta.url));
  const fields = Object.fromEntries(metadata.toString().trim().split(/\r?\n/u).map((line) => {
    const i = line.indexOf("="); return [line.slice(0, i), line.slice(i + 1)];
  }));
  const release = { version: fields.CTX_RELEASE_VERSION, source_commit: fields.CTX_RELEASE_SOURCE_COMMIT, metadata_sha256: sha256(metadata) };
  const fetcher = async (url) => new Response(url.endsWith(".sig") ? signature : metadata);
  assert.equal((await verifyCurrentFeed(release, fetcher)).signature_verified, true);
  await assert.rejects(verifyCurrentFeed({ ...release, metadata_sha256: "0".repeat(64) }, fetcher), /differs/);
  await assert.rejects(verifyCurrentFeed({ ...release, version: "1.5.0" }, fetcher), /differs/);
  await assert.rejects(verifyCurrentFeed(release, async () => new Response("tampered")), /differs/);
});
