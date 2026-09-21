import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { CLI_INSTALL_POWERSHELL_NATIVE_PROCESS } from "../cli-install-powershell-native-process.js";
import { CLI_INSTALL_POWERSHELL_PROCESS_HELPERS } from "../cli-install-powershell-process.js";
import { CLI_INSTALL_POWERSHELL_MANAGED_INSTALL } from "../cli-install-powershell-managed-install.js";
import { renderCliInstallPowerShellManagedPairPublication } from "../cli-install-powershell-managed-pair.js";
import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const powershell = ["pwsh", "powershell"].find((command) =>
  spawnSync(command, ["-NoProfile", "-NonInteractive", "-Command", "exit 0"]).status === 0);


test("PowerShell candidate apply uses the native Windows disk root", { skip: !powershell }, () => {
  const body = renderCliInstallPowerShellScript();
  const pairApply = body.match(/function Invoke-ManagedPairApply\([^\n]+\) \{[\s\S]*?^\}/m)?.[0];
  assert.ok(pairApply);
  const command = pairApply + String.raw`
$ErrorActionPreference = 'Stop'
function Protect-ManagedPath { param($Path, [switch]$Directory) }
function Test-Path { return $false }
function Join-Path { param($Path, $ChildPath) return $Path + '/' + $ChildPath }
function Invoke-ExecutableCaptured {
    param($Executable, $Arguments)
    [Console]::Out.WriteLine($Arguments[1])
    return [pscustomobject]@{ExitCode=0;OutputPath='unused';ErrorPath='unused'}
}
function Read-BoundedSuccessReceipt {
    return [pscustomobject]@{Receipt=[pscustomobject]@{schema_version=1;command='managed_pair_apply';ok=$true;status='committed'};Warnings=@()}
}
function Write-SuccessReceiptWarnings {}
$downloadPath='candidate';$pairEnvelopePath='envelope';$pairCompanionPath='companion'
$pairInstallRoot='C:\managed'
$null=Invoke-ManagedPairApply -MarkerSource 'marker' -Required $true
if($pairInstallRoot -cne 'C:\managed'){throw 'changed caller root'}
$pairInstallRoot='\\?\C:\managed'
$null=Invoke-ManagedPairApply -MarkerSource 'marker' -Required $true
`;
  const result = spawnSync(powershell, ["-NoProfile", "-NonInteractive", "-Command", command], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.stdout.trim().split(/\r?\n/), [String.raw`\\?\C:\managed`, String.raw`\\?\C:\managed`]);
});

test("rendered installer captures native stderr without PowerShell redirection", () => {
  const body = renderCliInstallPowerShellScript();
  assert.ok(body.includes(CLI_INSTALL_POWERSHELL_NATIVE_PROCESS));
  assert.match(body, /RedirectStandardOutput = \$commandOutputPath/u);
  assert.match(body, /RedirectStandardError = \$commandErrorPath/u);
  assert.match(body, /\$process\.WaitForExit\(\)/u);
  assert.match(body, /\$commandExitCode = \[int\]\$process\.ExitCode/u);
  assert.match(body, /ProcessError = \$processError/u);
  assert.doesNotMatch(CLI_INSTALL_POWERSHELL_NATIVE_PROCESS, /Start-Process[^\n]*-Wait|& \$Executable/u);
  assert.match(body, /managed lifecycle handoff \(exit code \$\(\$upgradeCommand.ExitCode\)\)/u);
});

test("released 1.3.1 compatibility is selected before execution, not after failure", () => {
  const body = renderCliInstallPowerShellScript();
  assert.match(body, /\$releasedPairInstall = \$managedPair -and \$version -ceq "1\.3\.1"/u);
  assert.match(body, /elseif \(\$managedPair\) \{\s+if \(\$releasedPairInstall\) \{\s+Invoke-HostedInstallTransaction\s+Invoke-ReleasedManagedPairInstall/u);
  assert.match(body, /\} else \{\s+Invoke-ManagedCoreUpgrade\s+if \(\$releasedPairInstall\) \{\s+Invoke-ReleasedManagedPairInstall/u);
  assert.match(body, /-AllowPrettyJson:\(\$version -ceq "1\.3\.1"\)/u);
  const current = body.match(/function Invoke-ManagedPairApply\([^\n]+\) \{[\s\S]*?^\}/m)?.[0];
  assert.ok(current);
  assert.doesNotMatch(current, /ReleasedManagedPair|hosted-pair-install|AllowPrettyJson/u);
});

test("ordinary managed upgrade accepts the CLI's pretty JSON without a version exception", () => {
  const body = renderCliInstallPowerShellScript();
  const upgrade = body.match(/function Invoke-ManagedCoreUpgrade \{[\s\S]*?^\}/m)?.[0];
  assert.ok(upgrade);
  assert.match(upgrade, /Read-BoundedSuccessReceipt[^\n]+-AllowPrettyJson\s*\n/u);
  assert.doesNotMatch(upgrade, /-AllowPrettyJson:/u);
});

test("managed upgrade parses bounded pretty CLI receipts and rejects invalid results", {
  skip: powershell ? false : "PowerShell is not installed",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-upgrade-receipt-"));
  try {
    const helpers = path.join(root, "helpers.ps1");
    writeFileSync(helpers, CLI_INSTALL_POWERSHELL_PROCESS_HELPERS + "\n" + CLI_INSTALL_POWERSHELL_MANAGED_INSTALL);
    const result = spawnSync(powershell, ["-NoProfile", "-NonInteractive", "-File",
      path.join(here, "managed-upgrade-receipt-fixture.ps1"), "-HelperPath", helpers, "-WorkRoot", root],
    { encoding: "utf8", timeout: 30_000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.equal(JSON.parse(result.stdout.trim()).passed, 10);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("rendered Windows child failures emit only bounded guarded captures", () => {
  const body = renderCliInstallPowerShellScript();
  const helperStart = body.indexOf("function Write-BoundedCapturedChildError");
  const helperEnd = body.indexOf("function Invoke-CtxCaptured", helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart);
  const helper = body.slice(helperStart, helperEnd);
  assert.match(helper, /\$maximumCharacters = 8KB/);
  assert.match(helper, /\$maximumLines = 40/);
  assert.match(helper, /\^ctx-command-\[0-9a-f\]\{32\}\\\.err\$/);
  assert.match(helper, /FileAttributes\]::ReparsePoint/);
  assert.match(helper, /authorization\|password\|secret\|token\|credential\|api/);
  assert.match(helper, /Bearer/);
  assert.doesNotMatch(helper, /GetEnvironmentVariables|\$Arguments/);
  const transactionStart = body.indexOf("function Invoke-HostedInstallTransaction");
  const transactionEnd = body.indexOf("function Invoke-ManagedCoreUpgrade", transactionStart);
  const transaction = body.slice(transactionStart, transactionEnd);
  assert.ok(
    transaction.indexOf(
      "Write-BoundedCapturedChildError -ErrorPath $transaction.ErrorPath",
    ) < transaction.indexOf(
      'Fail "ctx could not complete its crash-recoverable hosted install transaction (exit code',
    ),
  );
  assert.equal(transaction.match(/Write-BoundedCapturedChildError/gu)?.length, 1);

  const pair = body.match(/function Invoke-ManagedPairApply\([^\n]+\) \{[\s\S]*?^}/m)?.[0] ?? "";
  assert.ok(
    pair.indexOf(
      "Write-BoundedCapturedChildError -ErrorPath $pairApply.ErrorPath",
    ) < pair.indexOf(
      'Fail "ctx managed-pair installation did not complete (exit code',
    ),
  );
  assert.equal(pair.match(/Write-BoundedCapturedChildError/gu)?.length, 2);
  assert.match(pair, /if \(-not \$pairReceiptValid\) \{\s+if \(\$Required\) \{\s+Write-BoundedCapturedChildError/u);
  const upgrade = body.match(/function Invoke-ManagedCoreUpgrade \{[\s\S]*?^\}/m)?.[0] ?? "";
  assert.match(upgrade, /Write-BoundedCapturedChildError -ErrorPath \$upgradeCommand\.ErrorPath/u);
  assert.equal(upgrade.match(/Write-BoundedCapturedChildError/gu)?.length, 1);
});

test("released pair protocol validates pretty receipts and fails closed", {
  skip: powershell ? false : "PowerShell is not installed",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-pair-compat-"));
  try {
    writeFileSync(path.join(root, "helpers.ps1"), CLI_INSTALL_POWERSHELL_PROCESS_HELPERS +
      "\n" + renderCliInstallPowerShellManagedPairPublication());
    const result = spawnSync(powershell, ["-NoProfile", "-NonInteractive", "-File",
      path.join(here, "released-pair-fixture.ps1"), "-HelperPath", path.join(root, "helpers.ps1"), "-WorkRoot", root],
    { encoding: "utf8", timeout: 30_000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    const receipt = JSON.parse(result.stdout.trim());
    assert.equal(receipt.passed, 13);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("real Windows native process capture on installed PowerShell editions", {
  skip: process.platform === "win32" ? false : "requires native Windows, not a command mock",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-native-capture-"));
  try {
    const helper = path.join(root, "helpers.ps1");
    const executable = path.join(root, "native fixture.exe");
    writeFileSync(helper, CLI_INSTALL_POWERSHELL_NATIVE_PROCESS);
    const compile = path.join(root, "compile.ps1");
    writeFileSync(compile, "param($Source, $Output)\n$ErrorActionPreference = 'Stop'\nAdd-Type -Path $Source -OutputAssembly $Output -OutputType ConsoleApplication\n");
    const built = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-File", compile,
      "-Source", path.join(here, "native-process-fixture.cs"), "-Output", executable],
    { encoding: "utf8", timeout: 60_000 });
    assert.equal(built.status, 0, built.stdout + built.stderr);
    const editions = ["powershell.exe", "pwsh.exe"];
    for (const edition of editions) {
      const result = spawnSync(edition, ["-NoProfile", "-NonInteractive", "-File",
        path.join(here, "native-process-fixture.ps1"), "-HelperPath", helper,
        "-FixtureExecutable", executable, "-WorkRoot", root], { encoding: "utf8", timeout: 60_000 });
      assert.equal(result.status, 0, `${edition}: ${result.stdout}${result.stderr}`);
      assert.equal(JSON.parse(result.stdout.trim()).passed.length, 8);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
