import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const fixturePath = path.join(here, "hosted-windows-install-fixture.ps1");
const wrapperPath = path.resolve(here, "../../tests/install_live_smoke.ps1");
const fixture = fs.readFileSync(fixturePath, "utf8");
const wrapper = fs.readFileSync(wrapperPath, "utf8");
const powershell = ["pwsh", "powershell"].find((command) =>
  spawnSync(command, ["-NoProfile", "-NonInteractive", "-Command", "exit 0"], { timeout: 10_000 }).status === 0);
const quote = (text) => `'${text.replaceAll("'", "''")}'`;
const hash = (text) => createHash("sha256").update(text).digest("hex");

test("live gate requires both shells and approved readback, never feed substitution", () => {
  assert.match(wrapper, /Invoke-LiveFixture \$ps5 '5\.1\.\*'/);
  assert.match(wrapper, /Invoke-LiveFixture \$ps7 \$runtime\.version/);
  assert.match(wrapper, /if \(-not \(Test-Path -LiteralPath \$Shell -PathType Leaf\)\) \{ throw/);
  assert.match(wrapper, /Get-FileHash -LiteralPath \$InstallerPath -Algorithm SHA256/);
  assert.match(wrapper, /-cne \$ExpectedInstallerSha256/);
  assert.doesNotMatch(wrapper, /Join-Path \$files 'install\.ps1'|Invoke-WebRequest|https:|Skip|DryRun/);
  assert.match(wrapper, /\[Console\]::OpenStandardInput\(\).ReadByte\(\) -ne -1/);
  assert.match(wrapper, /\$capture = Invoke-ExecutableCaptured \$Shell \$arguments/);
  assert.match(wrapper, /\$tempRoot = \$root/);
  assert.doesNotMatch(wrapper, /Start-Process|taskkill|WaitForExit/);
  assert.match(wrapper, /\$acl.SetOwner\(\$owner\); \$acl.SetAccessRuleProtection\(\$true, \$false\)/);
  assert.match(wrapper, /\$env:TEMP = \$root; \$env:TMP = \$root/);
  assert.match(wrapper, /\$env:TEMP = \$oldTemp; \$env:TMP = \$oldTmp/);
  assert.ok(wrapper.indexOf("$script:status = [int]$capture.ExitCode") < wrapper.indexOf("foreach ($capturePath"));
  assert.match(wrapper, /if \(\$status -eq 0\) \{ \$status = 1 \}/);
  assert.doesNotMatch(wrapper, /Test-NativeProfile|Set-ItemProperty|\$env:(USERPROFILE|APPDATA|LOCALAPPDATA)\s*=/);
  assert.match(fixture, /\. \$ProfilePath\s+Test-LoadedUserProfile/);
  assert.doesNotMatch(fixture, /MetadataPath|['"]-Metadata['"]/);
  assert.ok(fixture.indexOf("$status = [int]$result.ExitCode") < fixture.indexOf("$detail ="));
  assert.match(fixture, /\$detail.Length -gt 8192/);
  assert.match(fixture, /\[Console\]::Error.WriteLine\(\$detail\)/);
});

function runScript(body, files = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-live-fixture-contract-"));
  try {
    for (const [name, text] of Object.entries(files)) fs.writeFileSync(path.join(root, name), text);
    const script = path.join(root, "case.ps1");
    fs.writeFileSync(script, `$ErrorActionPreference = 'Stop'\nSet-StrictMode -Version Latest\n$root = ${quote(root)}\n${body}`);
    return spawnSync(powershell, ["-NoProfile", "-NonInteractive", "-File", script], {
      encoding: "utf8", input: "", timeout: 30_000,
    });
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}

test("PowerShell validator rejects wrong identity, nonzero, missing phases and profile", {
  skip: powershell ? false : "PowerShell unavailable; native live gate remains mandatory",
}, () => {
  const result = runScript(`
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(${quote(wrapperPath)}, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'wrapper syntax' }
$function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Assert-LiveResult' }, $true)
. ([ScriptBlock]::Create($function.Extent.Text))
$ExpectedVersion = '1.3.3'; $ExpectedCoreSha256 = 'a' * 64; $ExpectedProSha256 = 'b' * 64
foreach ($case in @('valid', 'version', 'core', 'pro', 'exit', 'missing', 'order', 'shell', 'profile')) {
    $result = [pscustomobject]@{ powershell = '5.1.0'; version = '1.3.3'; loaded_profile = $true; passed = @(
        [pscustomobject]@{ phase = 'fresh'; exit_code = 0; core_sha256 = $ExpectedCoreSha256; pro_sha256 = $ExpectedProSha256 },
        [pscustomobject]@{ phase = 'managed-reinstall'; exit_code = 0; core_sha256 = $ExpectedCoreSha256; pro_sha256 = $ExpectedProSha256 }) }
    switch ($case) {
        version { $result.version = '1.3.1' }; core { $result.passed[0].core_sha256 = 'c' * 64 }
        pro { $result.passed[1].pro_sha256 = 'c' * 64 }; exit { $result.passed[1].exit_code = 7 }
        missing { $result.passed = @($result.passed[0]) }; order { $result.passed[0].phase = 'managed-reinstall' }
        shell { $result.powershell = '7.6.5' }; profile { $result.loaded_profile = $false }
    }
    $accepted = $false
    try { Assert-LiveResult $result '5.1.*'; $accepted = $true } catch {}
    if ($accepted -ne ($case -ceq 'valid')) { throw ('result verdict: ' + $case) }
}
Write-Output 'validator:9'
`);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.match(result.stdout, /validator:9/);
});

test("fixture uses ordinary discovery and restores poisoned environment on success and failures", {
  skip: powershell ? false : "PowerShell unavailable; native live gate remains mandatory",
}, () => {
  const controls = ["CTX_RELEASE_METADATA_URL", "CTX_RELEASE_METADATA_SIGNATURE_URL", "CTX_UPGRADE_FUNCTIONS_BASE",
    "CTX_UPGRADE_CHANNEL", "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL"];
  const helper = `function Invoke-ExecutableCaptured([string]$Executable, [string[]]$Arguments) {
    if ($Arguments -contains '-Metadata') { throw 'explicit metadata leaked' }
    foreach ($name in $global:controls) {
        if (-not [string]::IsNullOrEmpty([Environment]::GetEnvironmentVariable($name, 'Process'))) { throw 'feed override leaked' }
    }
    if ($env:CTX_INSTALL_SEMANTIC -cne '0' -or $env:CTX_INSTALL_NO_PRO_TRIAL -cne '1') { throw 'feature controls leaked' }
    $bin = $Arguments[[Array]::IndexOf($Arguments, '-BinDir') + 1]
    $null = New-Item -ItemType Directory -Force -Path $bin
    $installation = Split-Path -Parent $bin
    $libexec = Join-Path $installation 'libexec'; $share = Join-Path $installation 'share/ctx'
    $null = New-Item -ItemType Directory -Force -Path $libexec, $share
    $coreText = if ($global:case -ceq 'identity') { 'wrong' } else { 'core' }
    [IO.File]::WriteAllBytes((Join-Path $bin 'ctx.exe'), [Text.Encoding]::UTF8.GetBytes($coreText))
    [IO.File]::WriteAllBytes((Join-Path $libexec 'ctx-pro.exe'), [Text.Encoding]::UTF8.GetBytes('pro'))
    [IO.File]::WriteAllText((Join-Path $bin 'ctx.exe.install.json'), '{"version":"1.3.3","sha256":"${hash('core')}"}')
    [IO.File]::WriteAllText((Join-Path $share 'managed-pair-state.json'), '{}')
    $out = Join-Path $tempRoot 'mock.out'; $err = Join-Path $tempRoot 'mock.err'
    [IO.File]::WriteAllText($out, ''); [IO.File]::WriteAllText($err, 'inert failure')
    $code = if ($global:case -ceq 'exit') { 7 } else { 0 }
    return [pscustomobject]@{ Started = ($global:case -cne 'missing'); ExitCode = $code; OutputPath = $out; ErrorPath = $err }
}
`;
  const result = runScript(`
$env:OS = 'Windows_NT'
$global:controls = @(${controls.map(quote).join(', ')})
foreach ($name in $controls) { [Environment]::SetEnvironmentVariable($name, 'inert-poison', 'Process') }
$env:CTX_INSTALL_SEMANTIC = 'inert-semantic'; $env:CTX_INSTALL_NO_PRO_TRIAL = 'inert-trial'
foreach ($case in @('valid', 'identity', 'exit', 'missing')) {
    $global:case = $case
    & ${quote(fixturePath)} -InstallerPath (Join-Path $root 'never-executed.ps1') -HelperPath (Join-Path $root 'helper.ps1') -ProfilePath (Join-Path $root 'profile.ps1') -ExpectedVersion '1.3.3' -ExpectedCoreSha256 '${hash('core')}' -ExpectedProSha256 '${hash('pro')}' -WorkRoot $root
    $observed = $LASTEXITCODE
    $expected = switch ($case) { valid { 0 }; exit { 7 }; default { 1 } }
    if ($observed -ne $expected) { throw ('fixture exit mismatch: ' + $case + ':' + $observed) }
    foreach ($name in $controls) {
        if ([Environment]::GetEnvironmentVariable($name, 'Process') -cne 'inert-poison') { throw 'feed environment not restored' }
    }
    if ($env:CTX_INSTALL_SEMANTIC -cne 'inert-semantic' -or $env:CTX_INSTALL_NO_PRO_TRIAL -cne 'inert-trial') { throw 'feature environment not restored' }
}
Write-Output 'environment:4'
`, { "helper.ps1": helper, "profile.ps1": "function Test-LoadedUserProfile {}\n" });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.match(result.stdout, /environment:4/);
});
