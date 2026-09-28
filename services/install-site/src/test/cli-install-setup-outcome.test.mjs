// Authored setup receipts exercise the actual installer result and summary paths.
import {
  assert, mkdtempSync, path, powerShellCommand, renderCliInstallPowerShellScript,
  rmSync, runRenderedCliInstaller, spawnSync, test, tmpdir, writeFileSync,
} from "./cli-install-test-helpers.mjs";

const failure = "insufficient free space for history publication: required 4096 bytes, available 1024 bytes";
const cases = [
  { name: "failed requested refresh", mode: "unavailable", reason: "refresh_failed", native: 7, exit: 7, manual: true, failed: true },
  { name: "released producer returned zero for failed refresh", mode: "unavailable", reason: "refresh_failed", native: 0, exit: 1, manual: true, failed: true },
  { name: "manual indexing without refresh", mode: "unavailable", reason: "daemon_disabled", native: 0, exit: 0, manual: true },
  { name: "explicit no-daemon", mode: "unavailable", reason: "explicit_opt_out", native: 0, exit: 0, noDaemon: true },
  { name: "genuinely pending refresh", mode: "pending", reason: "refresh_queued_without_published_generation", native: 0, exit: 0 },
  { name: "ready history", mode: "ready", reason: null, native: 0, exit: 0 },
  { name: "partial refresh with usable lexical history", mode: "unavailable", reason: null, native: 0, exit: 0 },
];

function receipt(c) {
  return JSON.stringify({
    schema_version: 3, initialized: true, mode: c.mode,
    indexed_sessions: 3, indexed_items: 30,
    lexical: { status: "ready" },
    refresh: { status: c.failed ? "unavailable" : "partial" },
    refresh_request: { status: "unavailable", reason: c.reason, last_error: c.failed ? failure : null },
  }, null, 2);
}

function assertOutcome(result, c) {
  const output = result.stdout + result.stderr;
  assert.equal(result.status, c.exit, `${c.name}: ${output}`);
  if (c.failed) {
    assert.ok(output.includes(failure), output);
    assert.match(output, /Previously indexed history remains searchable/);
    assert.match(output, /Setup failed/);
    assert.doesNotMatch(output, /Index ready|Indexing deferred|Indexing started|will continue in the background/);
  } else {
    assert.doesNotMatch(output, /Setup failed/);
    if (c.mode === "pending") assert.match(output, /Indexing started/);
    if (c.mode === "ready") assert.match(output, /Index ready/);
    if (c.noDaemon || c.manual) assert.match(output, /Indexing deferred/);
  }
}

for (const c of cases) {
  test(`shell setup outcome: ${c.name}`, () => {
    const fixture = runRenderedCliInstaller({
      args: ["--no-skill", "--no-man", "--no-modify-path", ...(c.noDaemon ? ["--no-daemon"] : [])],
      ...(c.manual ? { rawConfig: '[indexing]\nmode = "manual"\n' } : {}),
      env: { CTX_FAKE_SETUP_RECEIPT: receipt(c), CTX_FAKE_SETUP_STATUS: String(c.native) },
    });
    try { assertOutcome(fixture.result, c); } finally { fixture.cleanup(); }
  });
}

test("quiet shell setup without a receipt relays bounded failure and available Core evidence", () => {
  const stderr = 'attribution failed: io_storage_full\nCore history remains searchable.\nBearer authored-secret\n' + 'extra detail\n'.repeat(2000);
  const fixture = runRenderedCliInstaller({
    args: ["--no-skill", "--no-man", "--no-modify-path"],
    env: { CTX_SETUP_PROGRESS: "none", CTX_FAKE_SETUP_RECEIPT: "", CTX_FAKE_SETUP_STATUS: "7", CTX_FAKE_SETUP_STDERR: stderr },
  });
  try {
    const output = fixture.result.stdout + fixture.result.stderr;
    assert.equal(fixture.result.status, 7, output);
    assert.match(output, /attribution failed: io_storage_full/);
    assert.match(output, /Core history remains searchable/);
    assert.match(output, /Setup failed/);
    assert.match(output, /child output truncated/);
    assert.doesNotMatch(output, /authored-secret|Indexing deferred|Index ready/);
    assert.ok(output.length < 10000);
  } finally { fixture.cleanup(); }
});

test("PowerShell setup failure, pending, manual and ready outcomes", {
  skip: powerShellCommand ? false : "PowerShell is not installed",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-setup-outcome-"));
  try {
    const body = renderCliInstallPowerShellScript();
    const helpers = body.slice(body.indexOf("function Test-JsonIntegerValue"), body.indexOf("function Remove-ConfigComment"));
    const start = body.indexOf("    $setupStatus = 0");
    const end = body.indexOf("    if (-not [string]::IsNullOrWhiteSpace($script:pathResult))", start);
    assert.ok(start >= 0 && end > start);
    const script = path.join(root, "setup.ps1");
    const receiptPath = path.join(root, "receipt.json");
    writeFileSync(script, `param([string]$ReceiptPath, [int]$NativeStatus)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
${helpers}
$runSetup = $true
$setupNoDaemon = $env:CTX_TEST_NO_DAEMON -eq '1'
$daemonConfigurationDisabled = $env:CTX_TEST_MANUAL -eq '1'
$daemonEnabled = -not ($setupNoDaemon -or $daemonConfigurationDisabled)
$SetupProgress = 'none'
$semanticEnabled = $false
$installPath = 'authored-fixture'
$modifyPath = $false
$skillInstallFailed = $false
function Invoke-HostedInstallerSetupCtxCaptured { param($Arguments, [switch]$InheritStandardError)
  [pscustomobject]@{ ExitCode = $NativeStatus; OutputPath = $ReceiptPath; ErrorPath = $ReceiptPath }
}
function Send-InstallStage { param($Stage, $Status) }
function Configure-InstallPath { param($InstallPath, $ModifyPath) }
function Write-ReceiptItem { param($Text) [Console]::WriteLine($Text) }
function Write-ReceiptWarning { param($Text) [Console]::Error.WriteLine($Text) }
function Write-BoundedCapturedChildError { param($ErrorPath) }
function Format-ReceiptCount { param($Count) return [string]$Count }
${body.slice(start, end)}
exit $setupStatus
`);
    for (const c of cases) {
      writeFileSync(receiptPath, receipt(c));
      const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File", script,
        "-ReceiptPath", receiptPath, "-NativeStatus", String(c.native)], {
        encoding: "utf8", timeout: 30000,
        env: { ...process.env, CTX_TEST_MANUAL: c.manual ? "1" : "0", CTX_TEST_NO_DAEMON: c.noDaemon ? "1" : "0" },
      });
      assertOutcome(result, c);
    }
  } finally { rmSync(root, { recursive: true, force: true }); }
});
