import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, writeFileSync, mkdirSync, existsSync, mkdtempSync, rmSync, chmodSync, statSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { runRenderedCliInstaller, readCtxCommands, makeSignedMetadataFixture, sha256, powerShellCommand } from "./cli-install-test-helpers.mjs";
import { renderCliInstallScript } from "../cli-install-script.js";
import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";
import { powerShellReleaseFixture } from "./cli-install-powershell-fixture-helpers.mjs";
import { validSetupReceipt } from "./cli-install-setup-receipt-fixture.mjs";
import { runUninstaller } from "./uninstall-script-harness.mjs";

const args = ["--no-skill", "--no-man", "--no-modify-path"];

test("1.5 fresh install ignores legacy pair projection and trial tokens", () => {
  const fixture = runRenderedCliInstaller({
    releaseVersion: "1.5.0", managedPair: true, installBinPath: "a space-语义/bin",
    args: [...args, "--pro-trial", "--no-pro-trial"],
    env: { CTX_INSTALL_PRO_TRIAL: "1", CTX_INSTALL_NO_PRO_TRIAL: "1" },
  });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.deepEqual(readCtxCommands(fixture.commandLogPath).filter((command) => command.startsWith("setup ")), ["setup --quiet --format json --wait --progress none"]);
    assert.ok(readCtxCommands(fixture.commandLogPath).every((command) => !command.startsWith("--ctx-core-managed-pair-apply-v1")));
    const marker = JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json")));
    assert.notEqual(marker.managed_pair, true);
    assert.equal(existsSync(path.join(path.dirname(fixture.installBin), "libexec/ctx-pro")), false);
    assert.doesNotMatch(readFileSync(fixture.downloadUrlLogPath, "utf8"), /ctx-pro|managed-pair-envelope/);
    assert.doesNotMatch(fixture.result.stderr, /ctx pro|trial|activation/i);
    const rerun = fixture.rerun(args);
    assert.equal(rerun.status, 0, rerun.stderr);
    assert.equal(JSON.parse(readFileSync(path.join(fixture.installBin, "ctx.install.json"))).version, "1.5.0");
  } finally { fixture.cleanup(); }
});

test("current 1.4 feed still installs a signed pair with ordinary setup", () => {
  const fixture = runRenderedCliInstaller({ releaseVersion: "1.4.12", managedPair: true, args });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    assert.match(readFileSync(fixture.downloadUrlLogPath, "utf8"), /ctx-pro/);
    assert.ok(readCtxCommands(fixture.commandLogPath).includes("setup --quiet --format json --wait --progress none"));
    assert.doesNotMatch(fixture.result.stderr, /ctx pro|trial|activation/i);
  } finally { fixture.cleanup(); }
});

test("current installer recovers after unsupported cached commercial setup", () => {
  const fixture = runRenderedCliInstaller({ releaseVersion: "1.5.0", args });
  try {
    assert.equal(fixture.result.status, 0, fixture.result.stderr);
    const binary = path.join(fixture.installBin, "ctx");
    const prior = sha256(readFileSync(binary));
    // Representative cached setup step: 1.5 does not promise to understand old
    // commercial commands. This authored fixture deliberately rejects them.
    const stale = spawnSync(binary, ["pro", "setup"], { env: fixture.childEnv, encoding: "utf8" });
    assert.notEqual(stale.status, 0);
    assert.equal(sha256(readFileSync(binary)), prior);
    const rerun = fixture.rerun(args);
    assert.equal(rerun.status, 0, rerun.stderr);
    assert.equal(sha256(readFileSync(binary)), prior);
  } finally { fixture.cleanup(); }
});

for (const choice of [[], ["--keep-data"], ["--delete-data"]]) {
  test(`1.5 uninstall preserves retained history and legacy data: ${choice[0] ?? "default"}`, () => {
    let preserved;
    const fixture = runUninstaller({ os: "Linux", nativeVersion: "1.5.0", supportsProLifecycle: false,
      args: choice, prepareOwnedArtifacts: ({ homeDir }) => {
        preserved = ["pro/encrypted-index", "search/attribution/segment", "history/snapshot"]
          .map((name) => path.join(homeDir, ".ctx", name));
        for (const file of preserved) {
          mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, "authored sentinel\n");
        }
        return [];
      },
    });
    try {
      if (choice[0] === "--delete-data") {
        assert.notEqual(fixture.status, 0);
        assert.match(fixture.stderr, /legacy derived-data cleanup.*retired/);
        assert.equal(existsSync(fixture.installPath), true);
      } else {
        assert.equal(fixture.status, 0, fixture.stderr);
        assert.equal(existsSync(fixture.installPath), false);
      }
      for (const file of preserved) assert.equal(readFileSync(file, "utf8"), "authored sentinel\n");
      if (existsSync(fixture.nativeLog)) assert.doesNotMatch(readFileSync(fixture.nativeLog, "utf8"), /pro uninstall/);
    } finally { fixture.cleanup(); }
  });
}

test("help omits retired trial controls and rendering has no activation code", () => {
  const shell = renderCliInstallScript();
  const help = shell.slice(shell.indexOf("usage()"), shell.indexOf("\nfail()"));
  assert.match(help, /^usage\(\) \{[\s\S]*\nUSAGE\n\}/);
  assert.doesNotMatch(help, /\b(?:pro|trial|activation)\b|CTX_INSTALL_(?:NO_)?PRO_TRIAL/i);
  for (const body of [shell, renderCliInstallPowerShellScript()]) {
    assert.doesNotMatch(body, /pro\.ctx\.rs|trial_started|pro_setup_requested|proSetupRequested|setup.*--pro(?:\s|$)/m);
  }
});

for (const pendingRecovery of [null, "missing", "mismatch"]) {
test(`PowerShell 1.5 metadata preparation and ${pendingRecovery ?? "fresh"} recovery`, {
  skip: !powerShellCommand && process.env.CTX_REQUIRE_POWERSHELL !== "1" ? "PowerShell unavailable" : false,
}, () => {
  assert.ok(powerShellCommand, "deployment fixtures require PowerShell execution; unavailable is not a pass");
  const root = mkdtempSync(path.join(tmpdir(), "ctx-foss-metadata-"));
  try {
    const core = Buffer.from("authored unified binary fixture; never executed\n");
    const digest = sha256(core);
    const base = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.5.0";
    const metadata = ["CTX_RELEASE_SCHEMA_VERSION=1", "CTX_RELEASE_CHANNEL=stable", "CTX_RELEASE_VERSION=1.5.0",
      `CTX_RELEASE_BASE_URL=${base}`, "CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe", `CTX_RELEASE_SHA256_windows_x64=${digest}`,
      "CTX_RELEASE_MANAGED_PAIR_ENVELOPE_windows_x64=unused-legacy-envelope.json",
      `CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_windows_x64=sha256/${digest}/ctx.exe`,
      `CTX_RELEASE_MANAGED_PAIR_CORE_SHA256_windows_x64=${digest}`,
      `CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_windows_x64=sha256/${digest}/ctx-pro.exe`,
      `CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_windows_x64=${digest}`, ""].join("\n");
    const signed = makeSignedMetadataFixture(metadata);
    const source = path.join(root, "source"); mkdirSync(source);
    writeFileSync(path.join(source, "metadata.env"), metadata);
    writeFileSync(path.join(source, "metadata.sig"), signed.signatureBase64);
    writeFileSync(path.join(source, "core"), core);
    const feed = "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env";
    writeFileSync(path.join(source, "routes.json"), JSON.stringify([
      { uri: feed, file: "metadata.env" }, { uri: feed + ".sig", file: "metadata.sig" },
      { uri: base + "/ctx.exe", file: "core" },
    ]));
    const script = path.join(root, "case.ps1");
    writeFileSync(script, powerShellReleaseFixture({ metadataPublicKeyModulusBase64Url: signed.publicKeyModulusBase64Url,
      metadataPublicKeyExponentBase64Url: signed.publicKeyExponentBase64Url }, pendingRecovery));
    const work = path.join(root, "work"); mkdirSync(work);
    const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File", script,
      "-WorkRoot", work, "-SourceRoot", source], { encoding: "utf8", timeout: 30000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), { version: "1.5.0", core: digest, pro: null, managed_pair: false });
    assert.equal(existsSync(path.join(work, "installation")), pendingRecovery !== null);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

}

for (const version of ["1.4.12", "1.5.0"]) {
  for (const [name, receipt, summary] of [
    ["sessions", { indexedSessions: 12, indexedItems: 34 }, "Found 12 sessions"],
    ["records", { indexedSessions: 0, indexedItems: 34 }, "Found 34 records"],
    ["unavailable", { initialized: false, mode: "unavailable", indexedSessions: null, indexedItems: null }, null],
  ]) {
    test(`${version} ordinary Core setup summary: ${name}`, () => {
      const fixture = runRenderedCliInstaller({ releaseVersion: version, managedPair: true, args,
        env: { CTX_FAKE_SETUP_RECEIPT: validSetupReceipt(receipt) },
      });
      try {
        assert.equal(fixture.result.status, 0, fixture.result.stderr);
        if (summary) assert.ok(fixture.result.stderr.includes(summary), fixture.result.stderr);
        else assert.doesNotMatch(fixture.result.stderr, /Found .* (sessions|records)/);
        assert.doesNotMatch(fixture.result.stderr, /unbound|parameter not set/i);
      } finally { fixture.cleanup(); }
    });
  }
}

for (const phase of ["journal_prepared", "armed", "binary_removed", "marker_removed"]) {
  for (const choice of [[], ["--keep-data"], ["--delete-data"]]) {
    test(`1.5 interrupted uninstall ${phase}: ${choice[0] ?? "default"}`, () => {
      const fixture = runUninstaller({ os: "Linux", nativeVersion: "1.5.0", args: ["--keep-data"],
        supportsProLifecycle: false, env: { CTX_TEST_HOSTED_UNINSTALL_FAULT: phase },
      });
      try {
        assert.notEqual(fixture.status, 0);
        const files = [fixture.installPath, fixture.markerPath, fixture.transactionPath, fixture.helperPath];
        const snapshot = files.map((file) => existsSync(file) ? readFileSync(file) : null);
        const retried = fixture.rerun(choice, { CTX_TEST_HOSTED_UNINSTALL_FAULT: "" });
        if (choice[0] === "--delete-data") {
          assert.notEqual(retried.status, 0);
          assert.match(retried.stderr, /legacy derived-data cleanup.*retired/);
          assert.deepEqual(files.map((file) => existsSync(file) ? readFileSync(file) : null), snapshot);
        } else {
          assert.equal(retried.status, 0, retried.stderr);
          for (const file of files) assert.equal(existsSync(file), false);
        }
      } finally { fixture.cleanup(); }
    });
  }
  test(`1.4 interrupted uninstall ${phase} retains legacy delete-data recovery`, () => {
    const fixture = runUninstaller({ os: "Linux", nativeVersion: "1.4.12", args: ["--keep-data"],
      env: { CTX_TEST_HOSTED_UNINSTALL_FAULT: phase },
    });
    try {
      assert.notEqual(fixture.status, 0);
      const retried = fixture.rerun(["--delete-data"], { CTX_TEST_HOSTED_UNINSTALL_FAULT: "" });
      assert.equal(retried.status, 0, retried.stderr);
      assert.equal(existsSync(fixture.transactionPath), false);
    } finally { fixture.cleanup(); }
  });
}

for (const unsafe of ["journal-mode", "helper-mode", "directory-mode", "journal-owner", "helper-owner", "directory-owner", "directory-alias", "helper-digest"]) {
  test(`interrupted uninstall rejects ${unsafe} before helper execution`, () => {
    const fixture = runUninstaller({ os: "Linux", nativeVersion: "1.5.0", args: ["--keep-data"],
      supportsProLifecycle: false, env: { CTX_TEST_HOSTED_UNINSTALL_FAULT: "armed" },
    });
    try {
      assert.notEqual(fixture.status, 0);
      const directory = path.dirname(fixture.installPath);
      const env = { CTX_TEST_HOSTED_UNINSTALL_FAULT: "" };
      if (unsafe === "journal-mode") chmodSync(fixture.transactionPath, 0o644);
      if (unsafe === "helper-mode") chmodSync(fixture.helperPath, 0o755);
      if (unsafe === "directory-mode") chmodSync(directory, 0o777);
      if (unsafe.endsWith("-owner")) {
        env.CTX_TEST_UNSAFE_OWNER_PATH = unsafe === "journal-owner" ? fixture.transactionPath :
          unsafe === "helper-owner" ? fixture.helperPath : directory;
        // An unprivileged fixture cannot chown to another account. Supply the
        // foreign UID as the platform stat observation; all other stats are real.
        const stat = path.join(fixture.sandboxDir, "stubs", "stat");
        writeFileSync(stat, '#!/bin/sh\nif [ "$2" = "%u" ] && [ "$3" = "$CTX_TEST_UNSAFE_OWNER_PATH" ]; then printf "99999999\\n"; else exec /usr/bin/stat "$@"; fi\n');
        chmodSync(stat, 0o755);
      }
      if (unsafe === "directory-alias") {
        const alias = path.join(fixture.sandboxDir, "alias-bin");
        symlinkSync(directory, alias, "dir");
        env.CTX_UNINSTALL_INSTALL_PATH = path.join(alias, "ctx");
        env.CTX_UNINSTALL_MARKER_PATH = path.join(alias, "ctx.install.json");
      }
      if (unsafe === "helper-digest") writeFileSync(fixture.helperPath, readFileSync(fixture.helperPath, "utf8") + "\n# altered helper\n");
      const files = [fixture.installPath, fixture.markerPath, fixture.transactionPath, fixture.helperPath];
      const snapshot = files.map((file) => [readFileSync(file), statSync(file).mode]);
      const executions = readFileSync(fixture.executionLog);
      const result = fixture.rerun(["--delete-data"], env);
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /canonical owner-controlled|other accounts to write|owner-private|file ownership|recorded executable identity/);
      assert.deepEqual(readFileSync(fixture.executionLog), executions, "untrusted helper must never execute");
      assert.deepEqual(files.map((file) => [readFileSync(file), statSync(file).mode]), snapshot, "rejection must not remove or repair files");
    } finally { fixture.cleanup(); }
  });
}
