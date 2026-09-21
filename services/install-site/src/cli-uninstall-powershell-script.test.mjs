import test, { after } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { renderCliInstallPowerShellScript } from "./cli-install-powershell-script.js";
import { CLI_INSTALL_POWERSHELL_PATH_IDENTITY } from "./cli-install-powershell-managed-install.js";
import { renderCliUninstallPowerShellScript } from "./cli-uninstall-powershell-script.js";
import {
  VERIFIED_DAEMON_UNINSTALL_RESULT,
  daemonUninstallResult,
} from "./test/daemon-uninstall-result-fixture.mjs";

function findPowerShell() {
  for (const command of ["pwsh", "powershell"]) {
    const result = spawnSync(
      command,
      ["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"],
      { encoding: "utf8" },
    );
    if (result.status === 0) return command;
  }
  return null;
}

function findWindowsPowerShell51() {
  for (const command of ["powershell.exe", "powershell"]) {
    const result = spawnSync(
      command,
      [
        "-NoProfile",
        "-Command",
        "if ($PSVersionTable.PSEdition -ceq 'Desktop' -and $PSVersionTable.PSVersion.Major -eq 5) { exit 0 } else { exit 1 }",
      ],
      { encoding: "utf8" },
    );
    if (result.status === 0) return command;
  }
  return null;
}

test("Windows hosted uninstaller is marker-bound and preserves Core history", () => {
  const body = renderCliUninstallPowerShellScript({
    installAttemptId: "ia_windows_uninstall_test",
  });

  assert.ok(body.includes(CLI_INSTALL_POWERSHELL_PATH_IDENTITY));
  assert.doesNotMatch(body, /GetFullPath\([^\n)]*\.(?:install_path|helper_path)/);
  assert.equal([...body.matchAll(/Test-ManagedInstallPathIdentity -Candidate/g)].length, 5);
  assert.match(body, /^\[CmdletBinding\(\)\]/);
  assert.match(body, /\[switch\]\$DeleteData/);
  assert.match(body, /\[switch\]\$KeepData/);
  assert.match(body, /uninstall requires an explicit data choice: -DeleteData or -KeepData/);
  assert.match(body, /ctx-hosted-installer/);
  assert.match(body, /managed install marker does not own the requested executable/);
  assert.match(body, /installed executable differs from its managed install marker/);
  assert.match(body, /Get-FileHash -Algorithm SHA256/);
  assert.match(body, /platform -cne "windows-x64"/);
  assert.match(body, /function Invoke-CoreDaemonTeardown/);
  assert.match(body, /"daemon",\s+"disable",\s+"--prepare-uninstall",\s+"--format=json"/);
  assert.match(body, /Assert-CoreDaemonTeardownResult/);
  for (const field of Object.keys(VERIFIED_DAEMON_UNINSTALL_RESULT)) {
    assert.match(body, new RegExp(`"${field}"`));
  }
  assert.match(body, /\$Result\.scope -cne "installation"/);
  assert.match(body, /\$Result\.quiesced_root_count -lt 1/);
  assert.match(body, /\$quiescedRoots\.Count -ne \$Result\.quiesced_root_count/);
  assert.match(body, /\$DataRoot -cnotin \$normalizedRoots/);
  assert.match(body, /\$canonicalRoot -cnotin \$normalizedRoots/);
  assert.match(body, /-not \$Result\.installation_quiescent/);
  assert.match(body, /-not \$Result\.binary_retained/);
  assert.match(body, /--data-root", \$DataRoot, "pro", "uninstall"/);
  assert.match(body, /--delete-data/);
  assert.match(body, /--keep-data/);
  assert.match(body, /canonical_history_preserved/);
  assert.match(body, /predates Local Pro/);
  assert.match(body, /Version\.minor -le 25/i);
  assert.doesNotMatch(body, /Stop-InstalledCtxProcesses|Stop-Process|Get-Process/);
  assert.doesNotMatch(body, /Remove-ManagedUpgradeCoordination|daemon-quiescence-acks/);
  assert.match(body, /function Invoke-HostedUninstallTransaction/);
  assert.match(body, /"daemon_admission_fenced"/);
  assert.match(body, /\$result\.schema_version -ne 2/);
  assert.match(body, /-not \$result\.daemon_admission_fenced/);
  assert.match(body, /"upgrade", "--hosted-transaction", \$Action/);
  assert.match(body, /-Action "uninstall-prepare"/);
  assert.match(body, /-Action "uninstall-arm"/);
  assert.match(body, /-Action "uninstall-commit"/);
  assert.match(body, /\.hosted-install-transaction\.json/);
  assert.match(body, /\.hosted-uninstall-helper\.exe/);
  assert.match(body, /Remove-ManagedFile -Path \$InstallPath/);
  assert.match(body, /Remove-ManagedFile -Path \$MarkerPath/);
  assert.match(body, /Fail "failed to remove \$\{Label\}: \$Path"/);
  assert.doesNotMatch(body, /\$Label:/);
  assert.doesNotMatch(body, /Remove-Item[^\n]*\$DataRoot/i);
  assert.doesNotMatch(body, /Remove-Item[^\n]*-Recurse/i);
  assert.doesNotMatch(body, /CTX_DATA_DIR/);
  assert.match(body, /ctx uninstall complete\. Local ctx history was preserved\./);
  assert.ok(
    body.lastIndexOf("Invoke-CoreDaemonTeardown -Version $version") <
      body.lastIndexOf("$proData = Invoke-ProLifecycle -Version $version"),
  );
  assert.ok(
    body.lastIndexOf("$proData = Invoke-ProLifecycle -Version $version") <
      body.lastIndexOf('-Action "uninstall-commit"'),
  );
  assert.ok(
    body.lastIndexOf("Assert-CoreDaemonTeardownResult -Result $result") <
      body.lastIndexOf('-Action "uninstall-commit"'),
    "Windows must verify exact installation-wide proof before committing leaf removal",
  );
});

test("Windows hosted uninstaller diagnostics are bounded and content-free", () => {
  const body = renderCliUninstallPowerShellScript({
    installAttemptId: "ia_windows_uninstall_test",
  });

  assert.match(body, /event_name = "install_stage"/);
  assert.match(body, /event_version = 1/);
  assert.match(body, /stage = "uninstall"/);
  assert.match(body, /script_family = "powershell"/);
  assert.match(body, /-TimeoutSec 1/);
  assert.match(body, /\$script:installStageDeliveryEnabled = \$false/);
  assert.match(body, /Send-InstallStage -Status "started"/);
  assert.match(body, /Send-InstallStage -Status "completed"/);
  assert.match(body, /Send-InstallStage -Status "failed"/);
  assert.doesNotMatch(
    body.match(/function Send-InstallStage[\s\S]*?^}/m)?.[0] ?? "",
    /^\s+(path|command|error|version|data_choice|user)\s*=/im,
  );
});

const powerShell = findPowerShell();
const windowsPowerShell51 = findWindowsPowerShell51();
let forwarderRoot;
after(() => { if (forwarderRoot) rmSync(forwarderRoot, { recursive: true, force: true }); });

function writeFixtureLauncher(installPath) {
  if (process.platform !== "win32") {
    writeFileSync(installPath, '#!/bin/sh\nexec "$CTX_UNINSTALL_FIXTURE_NODE" "$CTX_UNINSTALL_FIXTURE_SCRIPT" "$0" "$@" </dev/null\n');
    chmodSync(installPath, 0o700);
    return;
  }
  if (!forwarderRoot) {
    forwarderRoot = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-forwarder-"));
    const compiler = path.join(forwarderRoot, "compile.ps1");
    writeFileSync(compiler, "param($Source,$Output)\n$ErrorActionPreference='Stop'\nAdd-Type -Path $Source -OutputAssembly $Output -OutputType ConsoleApplication\n");
    const built = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-File", compiler,
      "-Source", fileURLToPath(new URL("./test/hosted-uninstall-forwarder.cs", import.meta.url)),
      "-Output", path.join(forwarderRoot, "forwarder.exe")], { encoding: "utf8", timeout: 60_000 });
    assert.equal(built.status, 0, built.stdout + built.stderr);
  }
  copyFileSync(path.join(forwarderRoot, "forwarder.exe"), installPath);
}

function writeFakeManagedInstall(root, {
  canonicalDataRoot = null,
  daemonInitiallyRunning = true,
} = {}) {
  const installPath = path.join(root, "ctx-test.exe");
  const markerPath = `${installPath}.install.json`;
  const dataRoot = path.join(root, "data");
  const invocationLog = path.join(root, "ctx-invocations.txt");
  const daemonStatePaths = {
    coordination: path.join(dataRoot, "daemon", "upgrade-handoff.json"),
    endpoint: path.join(dataRoot, "daemon", "source-refresh-endpoint.json"),
    ownerLock: path.join(dataRoot, "daemon", "daemon.lock"),
    supervisor: path.join(dataRoot, "daemon", "supervisor.json"),
  };
  mkdirSync(dataRoot);
  if (daemonInitiallyRunning) {
    mkdirSync(path.dirname(daemonStatePaths.endpoint), { recursive: true });
    for (const statePath of Object.values(daemonStatePaths)) {
      writeFileSync(statePath, "owned daemon state\n");
    }
  }
  writeFixtureLauncher(installPath);
  copyFileSync(new URL("./test/hosted-uninstall-fixture.cjs", import.meta.url), path.join(root, "fixture.cjs"));
  writeFileSync(path.join(dataRoot, "history-sentinel"), "preserve Core history\n");
  const digest = createHash("sha256").update(readFileSync(installPath)).digest("hex");
  writeFileSync(
    markerPath,
    JSON.stringify({
      schema_version: 1,
      manager: "ctx-hosted-installer",
      platform: "windows-x64",
      version: "0.26.0",
      sha256: digest,
      install_path: installPath,
    }),
  );
  return {
    root,
    helperPath: path.join(root, ".ctx-test.exe.hosted-uninstall-helper.exe"),
    transactionPath: path.join(root, ".ctx-test.exe.hosted-install-transaction.json"),
    installPath,
    markerPath,
    dataRoot,
    canonicalDataRoot: canonicalDataRoot ?? dataRoot,
    daemonStatePaths,
    invocationLog,
  };
}

function runRenderedUninstaller(root, install, args, envOverrides = {}, fixturePrelude = "") {
  const script = path.join(root, "uninstall.ps1");
  let body = renderCliUninstallPowerShellScript({
    functionsBase: "http://diagnostics-disabled.invalid",
    installAttemptId: "ia_windows_uninstall_behavior",
  });
  if (process.platform !== "win32") {
    // These authored control-flow fixtures use POSIX files on non-Windows hosts.
    // The real strict drive comparator has separate cross-platform parser tests;
    // native Windows path/ACL cases below always run the untouched renderer.
    body = body.replace(CLI_INSTALL_POWERSHELL_PATH_IDENTITY, `function Test-ManagedInstallPathIdentity([object]$Candidate, [string]$Expected) {
    return ($Candidate -is [string] -and $Candidate -ceq $Expected)
}
`);
  }
  writeFileSync(script, body);
  const wrapper = path.join(root, "invoke-uninstall.ps1");
  writeFileSync(wrapper, `[CmdletBinding()]
param(
    [string]$InstallPath, [string]$MarkerPath, [string]$DataRoot,
    [switch]$DeleteData, [switch]$KeepData, [switch]$NonInteractive, [switch]$Json
)
${fixturePrelude}
try {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not [string]::IsNullOrWhiteSpace($userPath)) {
        $directory = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($InstallPath))
        foreach ($entry in ($userPath -split [regex]::Escape([string][IO.Path]::PathSeparator))) {
            if ([string]::IsNullOrWhiteSpace($entry)) { throw 'fixture requires unchanged user PATH' }
            try { $entryPath = [IO.Path]::GetFullPath($entry.Trim().Trim('"').TrimEnd('\\', '/')) }
            catch { continue }
            if ($entryPath -ieq $directory) { throw 'fixture requires installation outside user PATH' }
        }
    }
    & (Join-Path $PSScriptRoot 'uninstall.ps1') @PSBoundParameters
}
catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }
`);
  return spawnSync(
    powerShell,
    ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", wrapper,
      "-InstallPath", install.installPath, "-MarkerPath", install.markerPath,
      "-DataRoot", install.dataRoot, ...args],
    { encoding: "utf8", timeout: 30_000, env: fixtureEnvironment(install, envOverrides) },
  );
}

function fixtureEnvironment(install, envOverrides = {}) {
  const requestedDaemonResult = envOverrides.CTX_UNINSTALL_FAKE_DAEMON_RESULT ??
    daemonUninstallResult();
  let boundDaemonResult = requestedDaemonResult;
  try {
    const parsedDaemonResult = JSON.parse(requestedDaemonResult);
    if (Object.hasOwn(parsedDaemonResult, "requested_data_root")) {
      parsedDaemonResult.requested_data_root = install.dataRoot;
      parsedDaemonResult.canonical_data_root = install.canonicalDataRoot;
      parsedDaemonResult.quiesced_roots =
        install.canonicalDataRoot === install.dataRoot
          ? [install.dataRoot]
          : [install.canonicalDataRoot, install.dataRoot];
      parsedDaemonResult.quiesced_root_count = parsedDaemonResult.quiesced_roots.length;
    }
    boundDaemonResult = `${JSON.stringify(parsedDaemonResult, null, 2)}\n`;
  } catch {
    // Preserve deliberately malformed fixture text for fail-closed coverage.
  }
  return {
    ...process.env,
    CTX_ANALYTICS_ENABLED: "false",
    CTX_UNINSTALL_FAKE_DAEMON_COORDINATION: install.daemonStatePaths.coordination,
    CTX_UNINSTALL_FAKE_DAEMON_ENDPOINT: install.daemonStatePaths.endpoint,
    CTX_UNINSTALL_FAKE_DAEMON_OWNER_LOCK: install.daemonStatePaths.ownerLock,
    CTX_UNINSTALL_FAKE_DAEMON_REMOVE_STATE: "true",
    CTX_UNINSTALL_FAKE_DAEMON_STATUS: "0",
    CTX_UNINSTALL_FAKE_DAEMON_SUPERVISOR: install.daemonStatePaths.supervisor,
    CTX_UNINSTALL_FAKE_DATA_ROOT: install.dataRoot,
    CTX_UNINSTALL_FAKE_LOG: install.invocationLog,
    ...envOverrides,
    CTX_UNINSTALL_FAKE_DAEMON_RESULT: boundDaemonResult,
    CTX_UNINSTALL_FIXTURE_ROOT: install.root,
    CTX_UNINSTALL_FIXTURE_NODE: process.execPath,
    CTX_UNINSTALL_FIXTURE_SCRIPT: path.join(install.root, "fixture.cjs"),
    NODE_OPTIONS: "",
  };
}

const transactionArgs = (install, action) => ["upgrade", "--hosted-transaction", `uninstall-${action}`,
  "--install-path", install.installPath, ...(action === "prepare" ? ["--attempt-id", "ia_windows_uninstall_behavior"] : [])];
const transactionCall = (install, action) => ({ actor: action === "prepare" ? install.installPath : install.helperPath,
  args: transactionArgs(install, action) });
const installedCall = (install, args) => ({ actor: install.installPath, args });
const invocationRows = (install) => readFileSync(install.invocationLog, "utf8").trim().split(/\r?\n/).map(JSON.parse);

test("uninstaller fixture launcher preserves argv, streams, EOF and nonzero exit", () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-argv space-"));
  try {
    const install = writeFakeManagedInstall(root);
    const script = path.join(root, "echo.cjs");
    writeFileSync(script, 'const fs = require("node:fs");\nfs.writeFileSync(1, JSON.stringify({args:process.argv.slice(3),eof:fs.readFileSync(0).length}));\nfs.writeFileSync(2,"fixture stderr\\n");\nprocess.exitCode=89;\n');
    const args = ["", "two words", 'quote"inside', "C:\\space dir\\", 'backslash\\"quote'];
    const result = spawnSync(install.installPath, args, { encoding: "utf8", timeout: 15_000,
      input: "must not reach child", env: { ...fixtureEnvironment(install), CTX_UNINSTALL_FIXTURE_SCRIPT: script } });
    assert.equal(result.status, 89, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), { args, eof: 0 });
    assert.equal(result.stderr, "fixture stderr\n");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("uninstaller fixture binds real bytes and requires arm before recoverable commit", () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-state-"));
  try {
    const install = writeFakeManagedInstall(root);
    const options = { encoding: "utf8", timeout: 15_000, env: fixtureEnvironment(install) };
    const call = (action) => spawnSync(transactionCall(install, action).actor, transactionArgs(install, action), options);
    const digest = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
    assert.equal(spawnSync(install.installPath, ["unknown"], options).status, 91);
    assert.equal(spawnSync(install.installPath, transactionArgs(install, "arm"), options).status, 91);
    assert.equal(existsSync(install.transactionPath), false);
    const receipt = { schema_version: 2, command: "hosted_uninstall_transaction", ok: true, status: "prepared",
      daemon_admission_fenced: true, attempt_id: "ia_windows_uninstall_behavior", install_path: install.installPath,
      helper_path: install.helperPath, binary_sha256: digest(install.installPath), marker_sha256: digest(install.markerPath) };
    const prepared = call("prepare");
    assert.equal(prepared.status, 0, prepared.stderr);
    assert.deepEqual(JSON.parse(prepared.stdout), receipt);
    assert.equal(digest(install.helperPath), receipt.binary_sha256);
    const retry = transactionArgs(install, "prepare");
    retry[6] = "ia_new_attempt";
    assert.deepEqual(JSON.parse(spawnSync(install.installPath, retry, options).stdout), receipt);
    assert.equal(call("commit").status, 96);
    for (const file of [install.installPath, install.markerPath]) {
      const original = readFileSync(file);
      writeFileSync(file, Buffer.concat([original, Buffer.from("changed")]));
      assert.equal(call("arm").status, 91);
      assert.equal(existsSync(file), true);
      writeFileSync(file, original);
    }
    assert.equal(call("arm").status, 0);
    rmSync(install.installPath); // Resume the already-armed missing-binary branch.
    const committed = call("commit");
    assert.equal(committed.status, 0, committed.stderr);
    assert.deepEqual(JSON.parse(committed.stdout), { ...receipt, status: "committed" });
    assert.equal(existsSync(install.markerPath), false);
    assert.equal(existsSync(install.transactionPath), false);
    assert.equal(existsSync(install.helperPath), true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

for (const patch of [{ schema_version: 1 }, { ok: "true" }, { daemon_admission_fenced: false },
  { daemon_admission_fenced: "true" }, { extra: true }, { marker_sha256: null },
  { status: "committed" }, { helper_path: "wrong-helper.exe" }, { install_path: "wrong-install.exe" }]) {
  test(`Windows uninstaller rejects transaction receipt ${JSON.stringify(patch)}`, { skip: !powerShell }, () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-receipt-"));
    try {
      const install = writeFakeManagedInstall(root);
      const result = runRenderedUninstaller(root, install, ["-KeepData", "-Json"],
        { CTX_UNINSTALL_FAKE_RECEIPT_PATCH: JSON.stringify(patch) });
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /ctx returned (invalid|mismatched) hosted uninstall transaction proof/);
      assert.equal(existsSync(install.installPath), true);
      assert.equal(existsSync(install.markerPath), true);
      assert.deepEqual(invocationRows(install), [installedCall(install, ["--version"]), transactionCall(install, "prepare")]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test(
  "complete rendered Windows installer parses as Windows PowerShell 5.1",
  { skip: windowsPowerShell51 ? false : "Windows PowerShell 5.1 is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-install-site-windows-ps51-parser-"));
    try {
      const body = renderCliInstallPowerShellScript();
      assert.equal(Buffer.from(body, "utf8").findIndex((byte) => byte > 0x7f), -1);
      const script = path.join(root, "install.ps1");
      writeFileSync(script, body, "ascii");
      const command = [
        "$tokens=$null;$errors=$null;",
        `[System.Management.Automation.Language.Parser]::ParseFile('${script.replaceAll("'", "''")}',[ref]$tokens,[ref]$errors)|Out-Null;`,
        "if($errors.Count){$errors|ForEach-Object{[Console]::Error.WriteLine($_)};exit 1}",
      ].join("");
      const result = spawnSync(
        windowsPowerShell51,
        ["-NoProfile", "-NonInteractive", "-Command", command],
        { encoding: "utf8" },
      );
      assert.equal(result.status, 0, result.stderr);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "rendered Windows hosted install and uninstall scripts parse as PowerShell",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-install-site-ps-parser-"));
    try {
      for (const [name, body] of [
        ["install.ps1", renderCliInstallPowerShellScript()],
        ["uninstall.ps1", renderCliUninstallPowerShellScript()],
      ]) {
        const script = path.join(root, name);
        writeFileSync(script, body);
        const command = [
          "$tokens=$null;$errors=$null;",
          `[System.Management.Automation.Language.Parser]::ParseFile('${script.replaceAll("'", "''")}',[ref]$tokens,[ref]$errors)|Out-Null;`,
          "if($errors.Count){$errors|ForEach-Object{[Console]::Error.WriteLine($_)};exit 1}",
        ].join("");
        const result = spawnSync(
          powerShell,
          ["-NoProfile", "-NonInteractive", "-Command", command],
          { encoding: "utf8" },
        );
        assert.equal(result.status, 0, `${name}: ${result.stderr}`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

for (const [mode, args] of [
  ["interactive", []],
  ["noninteractive JSON", ["-NonInteractive", "-Json"]],
]) {
  test(
    `Windows hosted uninstaller rejects an omitted ${mode} data choice before mutation`,
    { skip: powerShell ? false : "PowerShell is not installed" },
    () => {
      const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-choice-"));
      try {
        const install = writeFakeManagedInstall(root);
        const result = runRenderedUninstaller(root, install, args);
        assert.notEqual(result.status, 0, result.stdout);
        assert.match(
          result.stderr,
          /uninstall requires an explicit data choice: -DeleteData or -KeepData/,
        );
        assert.equal(existsSync(install.installPath), true);
        assert.equal(existsSync(install.markerPath), true);
        assert.deepEqual(invocationRows(install), [installedCall(install, ["--version"])]);
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    },
  );
}

test(
  "Windows hosted uninstaller retains the binary when Core daemon teardown fails",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-daemon-failure-"));
    try {
      const install = writeFakeManagedInstall(root);
      const result = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-KeepData"],
        { CTX_UNINSTALL_FAKE_DAEMON_STATUS: "89" },
      );
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /Core daemon teardown failed with status 89/);
      assert.equal(existsSync(install.installPath), true);
      assert.equal(existsSync(install.markerPath), true);
      assert.deepEqual(
        invocationRows(install),
        [
          installedCall(install, ["--version"]),
          transactionCall(install, "prepare"),
          installedCall(install, ["--data-root", install.dataRoot, "daemon", "disable", "--prepare-uninstall", "--format=json"]),
        ],
      );
      assert.equal(JSON.parse(readFileSync(install.transactionPath)).phase, "prepared");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "Windows hosted uninstaller retries safely after interrupted Core teardown",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-daemon-retry-"));
    try {
      const install = writeFakeManagedInstall(root);
      const interrupted = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-DeleteData"],
        { CTX_UNINSTALL_FAKE_DAEMON_STATUS: "89" },
      );
      assert.notEqual(interrupted.status, 0);
      const retried = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-DeleteData"],
      );
      assert.equal(retried.status, 0, retried.stderr);
      assert.equal(existsSync(install.installPath), false);
      assert.equal(existsSync(install.markerPath), false);
      assert.deepEqual(
        invocationRows(install),
        [
          installedCall(install, ["--version"]),
          transactionCall(install, "prepare"),
          installedCall(install, ["--data-root", install.dataRoot, "daemon", "disable", "--prepare-uninstall", "--format=json"]),
          transactionCall(install, "commit"),
          installedCall(install, ["--version"]),
          transactionCall(install, "prepare"),
          installedCall(install, ["--data-root", install.dataRoot, "daemon", "disable", "--prepare-uninstall", "--format=json"]),
          installedCall(install, ["pro", "uninstall", "--help"]),
          installedCall(install, ["--data-root", install.dataRoot, "pro", "uninstall", "--delete-data", "--json"]),
          transactionCall(install, "arm"),
          transactionCall(install, "commit"),
        ],
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "Windows hosted uninstaller accepts verified teardown with no running daemon",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-no-daemon-"));
    try {
      const install = writeFakeManagedInstall(root, { daemonInitiallyRunning: false });
      const result = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-KeepData"],
      );
      assert.equal(result.status, 0, result.stderr);
      assert.equal(existsSync(install.installPath), false);
      assert.equal(existsSync(install.markerPath), false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "Windows custom-root uninstall accepts only installation-wide canonical-root proof",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-custom-root-"));
    try {
      const install = writeFakeManagedInstall(root, {
        canonicalDataRoot: path.join(root, "canonical-root"),
      });
      const result = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-KeepData"],
      );
      assert.equal(result.status, 0, result.stderr);
      assert.equal(existsSync(install.installPath), false);
      assert.equal(existsSync(install.markerPath), false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "Windows hosted uninstaller rejects success while supervisor residue remains",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-supervisor-residue-"));
    try {
      const install = writeFakeManagedInstall(root);
      const result = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-KeepData"],
        {
          CTX_UNINSTALL_FAKE_DAEMON_REMOVE_STATE: "false",
          CTX_UNINSTALL_FAKE_DAEMON_RESULT: daemonUninstallResult({
            supervisor_removed: false,
          }),
        },
      );
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /Core daemon teardown did not prove complete cleanup/);
      assert.equal(existsSync(install.installPath), true);
      assert.equal(existsSync(install.daemonStatePaths.supervisor), true);
      assert.deepEqual(
        invocationRows(install),
        [
          installedCall(install, ["--version"]),
          transactionCall(install, "prepare"),
          installedCall(install, ["--data-root", install.dataRoot, "daemon", "disable", "--prepare-uninstall", "--format=json"]),
        ],
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "Windows hosted uninstaller rejects untyped Core daemon success",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-untyped-daemon-"));
    try {
      const install = writeFakeManagedInstall(root);
      const result = runRenderedUninstaller(
        root,
        install,
        ["-NonInteractive", "-Json", "-KeepData"],
        {
          CTX_UNINSTALL_FAKE_DAEMON_RESULT: daemonUninstallResult({ ok: "true" }),
        },
      );
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /typed lifecycle proof/);
      assert.equal(existsSync(install.installPath), true);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

for (const [switchName, cliChoice, localProData] of [
  ["-DeleteData", "--delete-data", "deleted"],
  ["-KeepData", "--keep-data", "preserved"],
]) {
  test(
    `Windows hosted uninstaller executes the explicit ${switchName} lifecycle`,
    { skip: powerShell ? false : "PowerShell is not installed" },
    () => {
      const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-behavior-"));
      try {
        const install = writeFakeManagedInstall(root);
        const result = runRenderedUninstaller(root, install, [
          "-NonInteractive",
          "-Json",
          switchName,
        ]);
        assert.equal(result.status, 0, result.stderr);
        const output = JSON.parse(result.stdout);
        assert.equal(output.uninstalled, true);
        assert.equal(output.canonical_history_preserved, true);
        assert.equal(output.local_pro_data, localProData);
        assert.equal(existsSync(install.installPath), false);
        assert.equal(existsSync(install.markerPath), false);
        const invocations = invocationRows(install);
        assert.deepEqual(invocations, [
          installedCall(install, ["--version"]),
          transactionCall(install, "prepare"),
          installedCall(install, ["--data-root", install.dataRoot, "daemon", "disable", "--prepare-uninstall", "--format=json"]),
          installedCall(install, ["pro", "uninstall", "--help"]),
          installedCall(install, ["--data-root", install.dataRoot, "pro", "uninstall", cliChoice, "--json"]),
          transactionCall(install, "arm"),
          transactionCall(install, "commit"),
        ]);
        assert.equal(readFileSync(path.join(install.dataRoot, "history-sentinel"), "utf8"), "preserve Core history\n");
        assert.equal(existsSync(install.helperPath), false);
        assert.equal(existsSync(install.transactionPath), false);
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    },
  );
}

test(
  "Windows hosted uninstaller is idempotent after verified Core and Pro cleanup",
  { skip: powerShell ? false : "PowerShell is not installed" },
  () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-ps-idempotent-"));
    try {
      const install = writeFakeManagedInstall(root);
      const args = ["-NonInteractive", "-Json", "-KeepData"];
      const first = runRenderedUninstaller(root, install, args);
      assert.equal(first.status, 0, first.stderr);
      const invocationCount = readFileSync(install.invocationLog, "utf8")
        .trim()
        .split(/\r?\n/).length;

      const second = runRenderedUninstaller(root, install, args);
      assert.equal(second.status, 0, second.stderr);
      const output = JSON.parse(second.stdout);
      assert.equal(output.uninstalled, true);
      assert.equal(output.already_uninstalled, true);
      assert.equal(
        readFileSync(install.invocationLog, "utf8").trim().split(/\r?\n/).length,
        invocationCount,
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

for (const choice of [[], ["-KeepData"], ["-DeleteData"]]) {
  test(`Windows 1.5 uninstall preserves all history and inert data: ${choice[0] ?? "default"}`, {
    skip: powerShell ? false : "PowerShell is not installed",
  }, () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-foss-"));
    try {
      const install = writeFakeManagedInstall(root);
      const marker = JSON.parse(readFileSync(install.markerPath, "utf8"));
      marker.version = "1.5.0";
      marker.install_path = path.toNamespacedPath(install.installPath);
      writeFileSync(install.markerPath, JSON.stringify(marker));
      const sentinels = ["pro/encrypted-index", "search/attribution/segment"].map((name) => path.join(install.dataRoot, name));
      for (const file of sentinels) {
        mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, "authored sentinel\n");
      }
      const result = runRenderedUninstaller(root, install, ["-NonInteractive", "-Json", ...choice], {
        CTX_UNINSTALL_FAKE_VERSION: "1.5.0",
      });
      if (choice[0] === "-DeleteData") {
        assert.notEqual(result.status, 0);
        assert.match(result.stderr, /legacy derived-data cleanup.*retired/);
        assert.equal(existsSync(install.installPath), true);
        assert.deepEqual(invocationRows(install), [installedCall(install, ["--version"])]);
      } else {
        assert.equal(result.status, 0, result.stderr);
        const output = JSON.parse(result.stdout);
        assert.equal(output.uninstalled, true);
        assert.equal(output.canonical_history_preserved, true);
        assert.equal(Object.hasOwn(output, "local_pro_data"), false);
        assert.equal(existsSync(install.installPath), false);
        assert.ok(invocationRows(install).every((row) => !row.args.includes("pro")));
      }
      for (const file of sentinels) assert.equal(readFileSync(file, "utf8"), "authored sentinel\n");
      assert.equal(readFileSync(path.join(install.dataRoot, "history-sentinel"), "utf8"), "preserve Core history\n");
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
}

// Author the same private Windows descriptors as native preparation. This is
// fixture setup only; the production recovery preflight never modifies ACLs.
function protectRecoveryFixture(install, extra = "") {
  const script = path.join(install.root, "fixture-acl.ps1");
  writeFileSync(script, `param([string]$Root)
$ErrorActionPreference = 'Stop'
$user = [Security.Principal.WindowsIdentity]::GetCurrent().User
$system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
foreach ($file in @($Root, (Join-Path $Root 'ctx-test.exe'), (Join-Path $Root 'ctx-test.exe.install.json'),
    (Join-Path $Root '.ctx-test.exe.hosted-uninstall-helper.exe'), (Join-Path $Root '.ctx-test.exe.hosted-install-transaction.json'))) {
    if (-not (Test-Path -LiteralPath $file)) { continue }
    $directory = (Get-Item -LiteralPath $file).PSIsContainer
    $acl = if ($directory) { [Security.AccessControl.DirectorySecurity]::new() } else { [Security.AccessControl.FileSecurity]::new() }
    $acl.SetOwner($user); $acl.SetAccessRuleProtection($true, $false)
    $flags = if ($directory) { [Security.AccessControl.InheritanceFlags]3 } else { [Security.AccessControl.InheritanceFlags]0 }
    foreach ($sid in @($user, $system)) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, [Security.AccessControl.FileSystemRights]::FullControl,
            $flags, [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow))
    }
    Set-Acl -LiteralPath $file -AclObject $acl
}
${extra}
`);
  const result = spawnSync(powerShell, ["-NoProfile", "-NonInteractive", "-File", script, "-Root", install.root], { encoding: "utf8", timeout: 30000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
}

for (const removed of [[], ["installPath"], ["installPath", "markerPath"]]) {
  for (const choice of [[], ["-KeepData"], ["-DeleteData"]]) {
    test(`Windows 1.5 ordinary request with verbatim native paths, armed missing ${removed.join(",") || "nothing"}: ${choice[0] ?? "default"}`, {
      skip: powerShell && process.platform === "win32" ? false : "native Windows ownership checks required",
    }, () => {
      const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-armed-foss-"));
      try {
        const install = writeFakeManagedInstall(root);
        const marker = JSON.parse(readFileSync(install.markerPath, "utf8"));
        marker.version = "1.5.0";
        marker.install_path = path.toNamespacedPath(install.installPath);
        writeFileSync(install.markerPath, JSON.stringify(marker));
        const env = { CTX_UNINSTALL_FAKE_VERSION: "1.5.0", CTX_UNINSTALL_FIXTURE_VERBATIM_PATHS: "1" };
        const options = { env: fixtureEnvironment(install, env), encoding: "utf8", timeout: 15000 };
        for (const action of ["prepare", "arm"]) {
          const call = transactionCall(install, action);
          const result = spawnSync(call.actor, call.args, options);
          assert.equal(result.status, 0, result.stderr);
          const receipt = JSON.parse(result.stdout);
          assert.equal(receipt.install_path, path.toNamespacedPath(install.installPath));
          assert.equal(receipt.helper_path, path.toNamespacedPath(install.helperPath));
        }
        assert.notEqual(install.installPath, path.toNamespacedPath(install.installPath));
        const journal = JSON.parse(readFileSync(install.transactionPath, "utf8"));
        assert.equal(journal.install_path, path.toNamespacedPath(install.installPath));
        for (const name of removed) rmSync(install[name]);
        protectRecoveryFixture(install);
        const files = [install.installPath, install.markerPath, install.transactionPath, install.helperPath];
        const snapshot = files.map((file) => existsSync(file) ? readFileSync(file) : null);
        const before = invocationRows(install).length;
        const result = runRenderedUninstaller(root, install, ["-NonInteractive", "-Json", ...choice], env);
        if (choice[0] === "-DeleteData") {
          assert.notEqual(result.status, 0);
          assert.match(result.stderr, /legacy derived-data cleanup.*retired/);
          assert.deepEqual(files.map((file) => existsSync(file) ? readFileSync(file) : null), snapshot);
          assert.deepEqual(invocationRows(install).slice(before), [{ actor: install.helperPath, args: ["--version"] }]);
        } else {
          assert.equal(result.status, 0, result.stderr);
          for (const file of files) assert.equal(existsSync(file), false);
          assert.ok(invocationRows(install).slice(before).some((row) => row.args.includes("uninstall-commit")));
        }
        assert.equal(readFileSync(path.join(install.dataRoot, "history-sentinel"), "utf8"), "preserve Core history\n");
      } finally { rmSync(root, { recursive: true, force: true }); }
    });
  }
}

for (const unsafe of ["journal-access", "helper-access", "directory-access", "journal-owner", "directory-alias", "helper-digest"]) {
  test(`Windows recovery rejects ${unsafe} before helper execution`, {
    skip: powerShell && process.platform === "win32" ? false : "native Windows ownership checks required",
  }, () => {
    const root = mkdtempSync(path.join(tmpdir(), "ctx-uninstall-unsafe-"));
    let alias;
    try {
      const install = writeFakeManagedInstall(root);
      const env = { CTX_UNINSTALL_FAKE_VERSION: "1.5.0" };
      for (const action of ["prepare", "arm"]) {
        const call = transactionCall(install, action);
        const result = spawnSync(call.actor, call.args, { env: fixtureEnvironment(install, env), encoding: "utf8", timeout: 15000 });
        assert.equal(result.status, 0, result.stderr);
      }
      let extra = "";
      if (unsafe.endsWith("-access")) {
        const name = unsafe === "journal-access" ? ".ctx-test.exe.hosted-install-transaction.json" :
          unsafe === "helper-access" ? ".ctx-test.exe.hosted-uninstall-helper.exe" : ".";
        extra = `$file = Join-Path $Root '${name}'
$acl = Get-Acl -LiteralPath $file
$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-1-0'), 'Write', 'Allow'))
Set-Acl -LiteralPath $file -AclObject $acl`;
      }
      protectRecoveryFixture(install, extra);
      let prelude = "";
      if (unsafe === "journal-owner") {
        // Foreign ownership is an injected descriptor observation: setting an
        // arbitrary other account as owner needs privileges the tests do not ask for.
        prelude = `function Get-Acl([string]$LiteralPath) {
    $acl = Microsoft.PowerShell.Security\\Get-Acl -LiteralPath $LiteralPath
    if ($LiteralPath.EndsWith('.hosted-install-transaction.json')) {
        $acl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-21-1-2-3-9999'))
    }
    return $acl
}`;
      }
      let requested = install;
      if (unsafe === "directory-alias") {
        alias = root + "-alias"; symlinkSync(root, alias, "junction");
        requested = { ...install, installPath: path.join(alias, "ctx-test.exe"), markerPath: path.join(alias, "ctx-test.exe.install.json") };
      }
      if (unsafe === "helper-digest") writeFileSync(install.helperPath, Buffer.concat([readFileSync(install.helperPath), Buffer.from("altered")]));
      const files = [install.installPath, install.markerPath, install.transactionPath, install.helperPath];
      const snapshot = files.map((file) => readFileSync(file));
      const invocations = readFileSync(install.invocationLog);
      const result = runRenderedUninstaller(root, requested, ["-DeleteData", "-NonInteractive", "-Json"], env, prelude);
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /unsafe|reparse|canonical|recorded executable identity/i);
      assert.deepEqual(readFileSync(install.invocationLog), invocations, "untrusted helper must never execute");
      assert.deepEqual(files.map((file) => readFileSync(file)), snapshot, "rejection preserves remaining files");
    } finally {
      if (alias) rmSync(alias, { recursive: true, force: true });
      rmSync(root, { recursive: true, force: true });
    }
  });
}
