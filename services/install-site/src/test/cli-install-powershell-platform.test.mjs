// PowerShell rendering, trust, and native path platform contracts.
// Full signed fresh/reinstall remains mandatory in tests/install_live_smoke.ps1.
import {
  INSTALL_SCRIPT_FAMILIES,
  INSTALL_STAGE_EVENT_NAME,
  INSTALL_STAGE_EVENT_VERSION,
  INSTALL_STAGE_PAYLOAD_KEYS,
  INSTALL_STAGES,
  INSTALL_STAGE_STATUS_PAIRS,
  INSTALL_STAGE_STATUSES,
  assert,
  assertLinesAtMost,
  chmodSync,
  existsSync,
  fileURLToPath,
  gzipSync,
  installerOutcome,
  installerOutput,
  linkSync,
  makeSignedMetadataFixture,
  mkdirSync,
  mkdtempSync,
  path,
  powerShellCommand,
  powerShellNativeGuardBlock,
  readCtxCommands,
  readFileSync,
  readOwnershipRecords,
  readdirSync,
  renderCliInstallPowerShellScript,
  renderCliInstallScript,
  renderUninstallScript,
  rmSync,
  runHostedUninstallForInstallerFixture,
  runRenderedCliInstaller,
  sha256,
  signMetadataBase64,
  spawnSync,
  statSync,
  stripAnsi,
  symlinkSync,
  test,
  tmpdir,
  utilLinuxScriptCommand,
  writeExecutable,
  writeFileSync,
} from "./cli-install-test-helpers.mjs";
import {
  assertRuntimeRepairUsesVerifiedMetadata,
  readStageReports,
} from "./cli-install-report-helpers.mjs";

test("rendered Windows CLI installer defaults to cli.ctx.rs release metadata", () => {
  const body = renderCliInstallPowerShellScript();
  assert.match(body, /https:\/\/cli\.ctx\.rs\/functions\/v1/);
  assert.match(body, /CTX_RELEASE_ARTIFACT_windows_x64/);
  assert.match(body, /\[switch\]\$NoSetup/);
  assert.match(body, /\[switch\]\$NoDaemon/);
  assert.match(body, /\[Parameter\(DontShow\)\] \[switch\]\$ProTrial/);
  assert.match(body, /\[Parameter\(DontShow\)\] \[switch\]\$NoProTrial/);
  assert.match(body, /\[switch\]\$Semantic/);
  assert.match(body, /\[switch\]\$NoSkill/);
  assert.match(body, /\[string\[\]\]\$SkillAgent = @\(\)/);
  assert.match(body, /\[switch\]\$AllSkillAgents/);
  assert.match(body, /\[switch\]\$NoModifyPath/);
  assert.match(body, /CTX_INSTALL_NO_SETUP/);
  assert.match(body, /CTX_INSTALL_NO_DAEMON/);
  assert.match(body, /CTX_INSTALL_SEMANTIC/);
  assert.match(body, /function Get-PersistedConfigControls/);
  assert.match(body, /function Remove-ConfigComment/);
  assert.match(body, /function Assert-PersistedConfigValue/);
  assert.match(body, /\[System\.Text\.UTF8Encoding\]::new\(\$false, \$true\)/);
  assert.match(body, /\[System\.IO\.File\]::ReadAllBytes\(\$configPath\)/);
  assert.match(body, /"daemon\.enabled"/);
  assert.match(body, /"indexing\.mode"/);
  assert.match(body, /"search\.semantic"/);
  assert.match(body, /\$fullKey -ceq "search\.semantic"/);
  assert.match(body, /\$fullKey -ceq "daemon\.enabled"/);
  assert.match(body, /duplicate config key `\{0\}` at line \{1\}; first set at line \{2\}/);
  assert.match(body, /function Test-CanonicalDaemonDisabled/);
  assert.match(body, /function Test-CiEnvironment/);
  assert.match(body, /function Get-OptionalUnsignedCount/);
  assert.doesNotMatch(body, /Test-ProActionUrl|pro\.ctx\.rs/);
  assert.doesNotMatch(body, /Read-ProTrialChoice|Read-Host|ReadLine\(\)/);
  const semanticSelection = body.match(
    /\$persistedConfigControls = Get-PersistedConfigControls[\s\S]*?(?=\$tempRoot =)/,
  )?.[0] ?? "";
  assert.match(semanticSelection, /CTX_SEARCH_SEMANTIC/);
  assert.match(
    semanticSelection,
    /\$semanticInstallControl -in @\("1", "true", "yes", "on"\)/,
  );
  assert.match(
    semanticSelection,
    /\$semanticSearchControl -in @\("1", "true", "yes", "on"\)/,
  );
  assert.match(semanticSelection, /\$semanticSearchControl = "unset"/);
  assert.match(semanticSelection, /\$semanticSearchControl = "false"/);
  assert.match(semanticSelection, /\$semanticSearchControl -eq ""/);
  assert.match(
    semanticSelection,
    /\$semanticSearchControlValue\.Trim\(\)\.Trim\('"'\)\.ToLowerInvariant\(\)/,
  );
  assert.match(semanticSelection, /\$persistedConfigControls\.SemanticEnabled/);
  assert.match(semanticSelection, /\$daemonEnabled = -not \$setupNoDaemon/);
  assert.match(semanticSelection, /\(Test-CanonicalDaemonDisabled\)/);
  assert.match(semanticSelection, /\$persistedConfigControls\.DaemonDisabled/);
  assert.match(
    semanticSelection,
    /Semantic installation requires an enabled daemon/,
  );
  const powerShellSemanticPreflight = body.indexOf(
    "Semantic installation requires an enabled daemon",
  );
  assert.ok(powerShellSemanticPreflight >= 0);
  assert.ok(
    powerShellSemanticPreflight < body.indexOf("$tempRoot ="),
    "PowerShell Semantic/no-daemon preflight must run before temporary/download activity",
  );
  const powerShellConfigPreflight = body.indexOf(
    "$persistedConfigControls = Get-PersistedConfigControls",
  );
  assert.ok(powerShellConfigPreflight >= 0);
  assert.ok(
    powerShellConfigPreflight < body.indexOf("$tempRoot ="),
    "PowerShell persisted-config preflight must run before temporary/download activity",
  );
  assert.match(body, /CTX_INSTALL_NO_SKILL/);
  assert.match(body, /CTX_INSTALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_ALL_SKILL_AGENTS/);
  assert.match(body, /CTX_INSTALL_NO_MODIFY_PATH/);
  assert.match(body, /CTX_ANALYTICS_ENABLED/);
  assert.match(body, /CTX_UPGRADE_FUNCTIONS_BASE/);
  assert.match(body, /CTX_UPGRADE_CHANNEL/);
  for (const retired of ["CTX_CHANNEL", "CTX_FUNCTIONS_BASE"]) {
    assert.doesNotMatch(body, new RegExp(retired));
  }
  for (const deprecated of [
    "CTX_ANALYTICS_OFF",
    "CTX_DISABLE_ANALYTICS",
    "CTX_INSTALL_DIAGNOSTICS_OFF",
    "CTX_DAEMON_OFF",
    "CTX_DISABLE_DAEMON",
    "CTX_UPGRADE_OFF",
    "CTX_DISABLE_AUTO_UPGRADE",
  ]) {
    assert.match(body, new RegExp(deprecated));
  }
  assert.match(body, /function Test-CanonicalAnalyticsDisabled/);
  assert.match(body, /\$value\.Trim\(\)\.ToLowerInvariant\(\)/);
  assert.match(body, /return \$normalized -in @\("0", "false", "no", "off"\)/);
  assert.match(body, /if \(Test-CanonicalAnalyticsDisabled\) \{\s+return\s+\}/);
  const canonicalFalseValues = new Set(["0", "false", "no", "off"]);
  for (const [value, expected] of [
    [undefined, false],
    ["", false],
    ["   ", false],
    [" 0 ", true],
    [" FaLsE ", true],
    ["\tNO\n", true],
    [" oFf ", true],
    [" true ", false],
    [" yes ", false],
    [" on ", false],
    ["invalid", false],
  ]) {
    const actual = value === undefined
      ? false
      : canonicalFalseValues.has(value.trim().toLowerCase());
    assert.equal(actual, expected, `PowerShell canonical vector ${JSON.stringify(value)}`);
  }
  assert.match(body, /CTX_INSTALL_ATTEMPT_ID/);
  assert.match(body, /'\^ia_\[A-Za-z0-9_-\]\{8,128\}\$'/);
  assert.match(body, /install-attempt/);
  assert.match(body, /if \(\$DryRun\) \{\s+return\s+\}/);
  assert.match(body, /if \(\$installTelemetryBase -notmatch '\^https:\/\/'\) \{\s+return\s+\}/);
  assert.match(body, /CTX_ALLOW_CUSTOM_RELEASE_BASE_URL/);
  assert.match(body, /CTX_RELEASE_METADATA_SIGNATURE_URL/);
  assert.match(body, /Verify-MetadataSignature/);
  assert.match(body, /System\.Security\.Cryptography\.RSA/);
  assert.match(body, /VerifyData/);
  assert.match(body, /RSASignaturePadding\]::Pkcs1/);
  assert.doesNotMatch(body, /openssl/i);
  assert.doesNotMatch(body, /minisign/i);
  assert.match(body, /cli\\.ctx\\.rs\/storage\/v1\/object\/public\/releases\/artifacts/);
  assert.match(body, /"integrations", "install", "skills"/);
  assert.match(body, /\$skillArgs \+= "--format=json"/);
  assert.match(body, /\$setupArgs = @\("setup", "--quiet", "--format", "json"\)/);
  assert.match(body, /\$setupArgs \+= "--semantic"/);
  assert.match(body, /\$setupArgs \+= @\("--progress", \$SetupProgress\)/);
  assert.doesNotMatch(body, /Test-LiveProDisclosure|\$proLiveDisclosure/);
  assert.match(body, /\$setupArgs \+= "--no-daemon"/);
  assert.match(
    body,
    /Invoke-HostedInstallerSetupCtxCaptured -Arguments \$setupArgs -InheritStandardError:\(\$SetupProgress -cne "none"\)/,
  );
  assert.doesNotMatch(body, /Get-CtxSetupStatusVerification|"status",\s*"--format=json"/);
  assert.doesNotMatch(body, /"pro",\s*"setup"|"--trial-only"/);
  assert.match(body, /\$setupReceipt = Get-Content[\s\S]*?ConvertFrom-Json/);
  assert.match(body, /Invoke-CtxQuiet -Arguments @\(\s+"upgrade",\s+"--channel",\s+\$channel,\s+"--format=json"/);
  assert.match(body, /function Write-ReceiptItem/);
  assert.match(body, /GetEnvironmentVariable\(\s+"NO_COLOR",\s+"Process"/);
  assert.match(body, /\[Console\]::IsOutputRedirected/);
  assert.match(body, /Write-Host \(\[char\]0x2713\) -NoNewline -ForegroundColor Green/);
  assert.match(body, /Write-ReceiptItem "Installed and verified"/);
  const powerShellBinaryVerified = body.indexOf(
    'Send-InstallStage -Stage "binary_install" -Status "completed"',
  );
  const powerShellInstalledReceipt = body.indexOf(
    'Write-ReceiptItem "Installed and verified"',
    powerShellBinaryVerified,
  );
  const powerShellSetupStarted = body.indexOf(
    'Send-InstallStage -Stage "setup" -Status "started"',
    powerShellInstalledReceipt,
  );
  assert.ok(
    powerShellBinaryVerified >= 0 &&
      powerShellBinaryVerified < powerShellInstalledReceipt &&
      powerShellInstalledReceipt < powerShellSetupStarted,
    "PowerShell must retain the install line, then receipt verification before setup begins",
  );
  assert.doesNotMatch(
    body.slice(powerShellBinaryVerified, powerShellInstalledReceipt),
    /Write-Host ""/,
  );
  assert.match(body, /"Found \$\(Format-ReceiptCount \$indexedSessions\) sessions"/);
  assert.match(body, /"Found \$\(Format-ReceiptCount \$indexedItems\) records"/);
  assert.doesNotMatch(body, /function Format-ReceiptGiB/);
  assert.doesNotMatch(body, /function Format-ReceiptMinutes/);
  assert.doesNotMatch(body, /inventory_source_bytes|lexical_index_estimate_seconds/);
  assert.match(body, /Write-ReceiptItem "Index ready"/);
  assert.match(body, /Write-ReceiptItem "Indexing started"/);
  assert.match(
    body,
    /Write-ReceiptItem \("Indexing deferred " \+ \[char\]0x2014 \+ " daemon disabled"\)/,
  );
  assert.doesNotMatch(body, /estimateMinutes|estimateUnit/);
  assert.match(body, /Write-Host "Indexing will continue in the background\."/);
  assert.match(body, /'  Search:    ctx search "test failure"'/);
  assert.match(body, /Write-Host "  Progress:  ctx index watch"/);
  assert.match(body, /Write-Host "  Status:    ctx status"/);
  assert.doesNotMatch(body, /CTX_PRO_CHANNEL|commercialChannel|Retry on the/);
  assert.doesNotMatch(body, /exit \$proStatus/);
  assert.match(body, /Write-ReceiptWarning "Setup failed\. Retry: ctx setup"/);
  assert.doesNotMatch(body, /Core is ready/);
  assert.match(body, /Configure-InstallPath -InstallPath \$installPath/);
  assert.match(body, /Get-Command -Name "ctx" -ErrorAction Stop/);
  assert.match(body, /\$command\.CommandType -cne "Application"/);
  assert.match(body, /Get-FileHash -Algorithm SHA256/);
  assert.match(body, /System\.IO\.Compression\.GZipStream/);
  assert.doesNotMatch(body, /Downloaded gzip-compressed artifact/);
  assert.match(body, /function Read-Artifact/);
  assert.match(body, /file artifact URLs are available only for explicit development inputs/);
  assert.match(body, /CTX_ALLOW_CUSTOM_RELEASE_BASE_URL/);
  assert.match(body, /artifact file must not be a reparse point/);
  assert.match(body, /\[System\.IO\.Path\]::IsPathRooted\(\$BinDir\)/);
  assert.match(body, /\$binPathRoot -ne "\\"/);
  assert.match(body, /CreateFileW/);
  assert.match(body, /FILE_FLAG_OPEN_REPARSE_POINT/);
  assert.match(body, /GetFileInformationByHandle/);
  assert.match(body, /GetFinalPathNameByHandleW/);
  assert.match(body, /information\.NumberOfLinks > 1/);
  assert.match(body, /FILE_SHARE_READ \| FILE_SHARE_WRITE/);
  assert.doesNotMatch(body, /FILE_SHARE_DELETE/);
  assert.match(body, /CtxInstallerPathGuard\]::AcquireDirectory\(\$BinDir, \$false\)/);
  assert.match(body, /CtxInstallerPathGuard\]::AcquireLeaf\(\$installPath\)/);
  assert.match(body, /CtxInstallerPathGuard\]::AcquireLeaf\(\$markerPath\)/);
  assert.match(body, /function Invoke-HostedInstallTransaction/);
  assert.match(
    body,
    /"upgrade", "--hosted-transaction", "install",\s+"--install-path", \$installPath,/,
  );
  assert.match(body, /"--marker-source", \$markerSourcePath/);
  assert.match(body, /"--binary-sha256", \$actualChecksum/);
  assert.doesNotMatch(body, /CtxInstallerLeafWriter|WriteFromFile|SetLength\(0\)/);
  assert.match(body, /function Protect-ManagedPath/);
  const preflightValidation = body.indexOf(
    "[CtxInstallerPathGuard]::AcquireDirectory($BinDir, $false)",
  );
  const tempCreation = body.indexOf("$tempRoot =");
  assert.ok(
    preflightValidation >= 0 && preflightValidation < tempCreation,
    "PowerShell must reject reparse-point ancestors before temporary/download activity",
  );
  const directoryValidation = body.indexOf(
    "$installGuard = [CtxInstallerPathGuard]::AcquireDirectory($BinDir, $true)",
  );
  const directoryProtection = body.indexOf("Protect-ManagedPath -Path $BinDir -Directory");
  const directoryRevalidation = body.indexOf(
    "$installGuard.AssertUnchanged()",
    directoryProtection,
  );
  const binaryLeafValidation = body.indexOf(
    "$binaryDestinationGuard = [CtxInstallerPathGuard]::AcquireLeaf($installPath)",
  );
  const markerLeafValidation = body.indexOf(
    "$markerDestinationGuard = [CtxInstallerPathGuard]::AcquireLeaf($markerPath)",
  );
  const binaryLeafProtectionGate = body.indexOf(
    "if ($binaryDestinationGuard.LeafExists)",
    binaryLeafValidation,
  );
  const binaryLeafProtection = body.indexOf(
    "Protect-ManagedPath -Path $installPath",
    binaryLeafProtectionGate,
  );
  const markerLeafProtectionGate = body.indexOf(
    "if ($markerDestinationGuard.LeafExists)",
    markerLeafValidation,
  );
  const markerLeafProtection = body.indexOf(
    "Protect-ManagedPath -Path $markerPath",
    markerLeafProtectionGate,
  );
  const destinationGuardsReleased = body.indexOf(
    "$installGuard.Dispose()",
    markerLeafProtection,
  );
  const transactionInvocation = body.indexOf(
    "Invoke-HostedInstallTransaction",
    destinationGuardsReleased,
  );
  assert.ok(
    directoryValidation >= 0 &&
      directoryValidation < directoryProtection &&
      directoryProtection < directoryRevalidation &&
      directoryRevalidation < binaryLeafValidation &&
      binaryLeafValidation < markerLeafValidation &&
      markerLeafValidation < binaryLeafProtectionGate &&
      binaryLeafProtectionGate < binaryLeafProtection &&
      binaryLeafProtection < markerLeafProtectionGate &&
      markerLeafProtectionGate < markerLeafProtection &&
      markerLeafProtection < destinationGuardsReleased &&
      destinationGuardsReleased < transactionInvocation,
    "PowerShell must validate and release destination guards before Core atomic publication",
  );
  assert.match(body, /To use the newly installed ctx in this shell, run:/);
  assert.match(body, /'  \$env:Path = "' \+ \$dir/);
  assert.match(body, /Protect-ManagedPath -Path \$installPath/);
  assert.match(body, /Protect-ManagedPath -Path \$markerPath/);
  assert.match(body, /checksum for windows-x64 is a placeholder/);
  assert.match(body, /checksum mismatch for \$\{artifact\}:/);
  assert.match(body, /metadata channel \$releaseChannel does not match requested channel \$channel/);
  assert.match(body, /ctx-hosted-installer/);
  assert.doesNotMatch(
    body.match(/function Send-InstallStage[\s\S]*?^}/m)?.[0] ?? "",
    /^\s+(error_kind|channel|version|path|command|user)\s*=/im,
  );
});

test("rendered Windows installer replaces managed ACLs with exact user and SYSTEM authority", () => {
  const body = renderCliInstallPowerShellScript();
  const aclClass = body.match(
    /public static class CtxInstallerAcl[\s\S]*?^}/m,
  )?.[0];
  assert.ok(aclClass);
  assert.match(aclClass, /ConvertStringSecurityDescriptorToSecurityDescriptorW/);
  assert.match(aclClass, /"D:P"/);
  assert.match(
    aclClass,
    /"\(A;" \+ inheritance \+ ";FA;;;" \+ canonicalCurrentSid \+ "\)"/,
  );
  assert.match(
    aclClass,
    /"\(A;" \+ inheritance \+ ";FA;;;S-1-5-18\)"/,
  );
  assert.equal(
    aclClass.match(/"\(A;/g)?.length,
    2,
    "replacement DACL must contain only current-user and SYSTEM ACE templates",
  );
  const retainedHandleOpen = aclClass.indexOf("CtxInstallerNativePath.Open(");
  const retainedHandleInspection = aclClass.indexOf(
    "CtxInstallerNativePath.Inspect(",
    retainedHandleOpen,
  );
  const descriptorReplacement = aclClass.indexOf(
    "SetSecurityInfo(",
    retainedHandleInspection,
  );
  assert.ok(
    retainedHandleOpen >= 0 &&
      retainedHandleOpen < retainedHandleInspection &&
      retainedHandleInspection < descriptorReplacement,
    "ACL replacement must use the validated no-delete-share destination handle",
  );
  assert.match(aclClass, /OWNER_SECURITY_INFORMATION/);
  assert.match(aclClass, /DACL_SECURITY_INFORMATION/);
  assert.match(aclClass, /PROTECTED_DACL_SECURITY_INFORMATION/);
  assert.doesNotMatch(body, /icacls\.exe/);
});

test("rendered Windows fresh install delegates atomic publication to Core", () => {
  const body = renderCliInstallPowerShellScript();
  const nativePathClass = body.match(
    /internal static class CtxInstallerNativePath[\s\S]*?^}/m,
  )?.[0];
  assert.ok(nativePathClass);
  assert.match(
    nativePathClass,
    /CreateFileW\(\s+path,\s+desiredAccess,\s+shareMode,/,
  );
  assert.match(
    body,
    /function Invoke-HostedInstallTransaction \{[\s\S]*--hosted-transaction", "install"/,
  );
  assert.match(
    body,
    /elseif \(\$managedPair\) \{\s+if \(\$releasedPairInstall\) \{\s+Invoke-HostedInstallTransaction\s+Invoke-ReleasedManagedPairInstall\s+} else \{\s+\$null = Invoke-ManagedPairApply -MarkerSource \$markerSourcePath -Required \$true\s+}\s+} else \{\s+Invoke-HostedInstallTransaction/,
  );
  assert.doesNotMatch(body, /CtxInstallerLeafWriter/);
  assert.doesNotMatch(body, /WriteFromFile|WriteBytes|SetLength\(0\)/);
});

test("rendered Windows installer lets Core resume before classifying destination pairs", () => {
  const body = renderCliInstallPowerShellScript();
  const classifier = body.match(
    /function Get-ExistingInstallPairState \{[\s\S]*?^\}/m,
  )?.[0] ?? "";
  assert.match(classifier, /return "fresh"/);
  assert.match(classifier, /return "managed"/);
  assert.match(
    classifier,
    /an unmanaged ctx executable already exists at \$installPath; move it to a backup path outside \$BinDir, rerun this installer/,
  );
  assert.match(
    classifier,
    /managed ctx install is corrupted: its hosted-install marker exists at \$markerPath but its executable is missing/,
  );

  const recovery = body.indexOf("Resume-InterruptedManagedPair", body.indexOf("[IO.File]::WriteAllBytes($markerSourcePath"));
  const classification = body.indexOf("$existingManagedInstall = Read-ExistingManagedInstall");
  assert.ok(
    recovery >= 0 && classification > recovery,
    "Windows must give authenticated Core a chance to resume before rejecting an incomplete pair",
  );
});

test(
  "rendered Windows install-pair classifier preserves fresh and managed states",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-pair-state-"));
    try {
      const body = renderCliInstallPowerShellScript();
      const classifier = body.match(
        /function Get-ExistingInstallPairState \{[\s\S]*?^\}/m,
      )?.[0];
      assert.ok(classifier);
      const installPath = path.join(sandbox, "ctx.exe");
      const markerPath = `${installPath}.install.json`;
      const command = `function Fail([string]$Message) { throw $Message }
${classifier}
$installPath = $env:CTX_TEST_INSTALL_PATH
$markerPath = $env:CTX_TEST_MARKER_PATH
$BinDir = [IO.Path]::GetDirectoryName($installPath)
Get-ExistingInstallPairState
`;
      const runClassifier = () => spawnSync(
        powerShellCommand,
        ["-NoProfile", "-NonInteractive", "-Command", command],
        {
          encoding: "utf8",
          env: {
            ...process.env,
            CTX_TEST_INSTALL_PATH: installPath,
            CTX_TEST_MARKER_PATH: markerPath,
          },
        },
      );

      const fresh = runClassifier();
      assert.equal(fresh.status, 0, fresh.stderr);
      assert.equal(fresh.stdout.trim(), "fresh");

      writeFileSync(installPath, "binary");
      const unmanaged = runClassifier();
      assert.notEqual(unmanaged.status, 0);
      assert.match(`${unmanaged.stdout}${unmanaged.stderr}`, /move it to a backup path/);

      writeFileSync(markerPath, "marker");
      const managed = runClassifier();
      assert.equal(managed.status, 0, managed.stderr);
      assert.equal(managed.stdout.trim(), "managed");

      rmSync(installPath);
      const corrupted = runClassifier();
      assert.notEqual(corrupted.status, 0);
      assert.match(`${corrupted.stdout}${corrupted.stderr}`, /managed ctx install is corrupted/);
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test("rendered Windows installer uses one exact managed-path identity helper", () => {
  const body = renderCliInstallPowerShellScript();
  const helperStart = body.indexOf(
    "function ConvertTo-ManagedInstallComparablePath",
  );
  const helperEnd = body.indexOf("function Get-ExistingInstallPairState", helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart);
  const helper = body.slice(helperStart, helperEnd);
  assert.ok(
    helper.includes(
      '$comparablePath.StartsWith("\\\\?\\", [System.StringComparison]::Ordinal)',
    ),
  );
  assert.ok(helper.includes("$comparablePath.Substring(4) -cnotmatch '^[A-Za-z]:\\\\'"));
  assert.doesNotMatch(helper, /GetFullPath/);
  assert.match(helper, /\$candidatePath -ceq \$expectedPath/);
  assert.equal(
    [...body.matchAll(/Test-ManagedInstallPathIdentity -Candidate/g)].length,
    5,
    "every marker and receipt install_path boundary must use the shared helper",
  );
  assert.doesNotMatch(
    body,
    /\[IO\.Path\]::GetFullPath\([^\n)]*\.install_path/,
  );
});

test(
  "rendered Windows managed-path identity accepts only equivalent drive spellings",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const body = renderCliInstallPowerShellScript();
    const helperStart = body.indexOf(
      "function ConvertTo-ManagedInstallComparablePath",
    );
    const helperEnd = body.indexOf(
      "function Get-ExistingInstallPairState",
      helperStart,
    );
    assert.ok(helperStart >= 0 && helperEnd > helperStart);
    const helper = body.slice(helperStart, helperEnd);
    const command = `$ErrorActionPreference = "Stop"
${helper}
[ordered]@{
    Ordinary = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_ORDINARY -Expected $env:CTX_TEST_ORDINARY
    ExtendedCandidate = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_EXTENDED -Expected $env:CTX_TEST_ORDINARY
    ExtendedExpected = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_ORDINARY -Expected $env:CTX_TEST_EXTENDED
    DifferentLeaf = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_DIFFERENT_LEAF -Expected $env:CTX_TEST_ORDINARY
    DifferentDrive = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_DIFFERENT_DRIVE -Expected $env:CTX_TEST_ORDINARY
    DifferentCase = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_DIFFERENT_CASE -Expected $env:CTX_TEST_ORDINARY
    Relative = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_RELATIVE -Expected $env:CTX_TEST_ORDINARY
    Traversal = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_TRAVERSAL -Expected $env:CTX_TEST_ORDINARY
    ForwardSlash = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_FORWARD_SLASH -Expected $env:CTX_TEST_ORDINARY
    DuplicateSeparator = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_DUPLICATE_SEPARATOR -Expected $env:CTX_TEST_ORDINARY
    Unc = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_UNC -Expected $env:CTX_TEST_ORDINARY
    VerbatimUnc = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_VERBATIM_UNC -Expected $env:CTX_TEST_ORDINARY
    DeviceNamespace = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_DEVICE_PATH -Expected $env:CTX_TEST_ORDINARY
    ExactDeviceNamespace = Test-ManagedInstallPathIdentity -Candidate $env:CTX_TEST_DEVICE_PATH -Expected $env:CTX_TEST_DEVICE_PATH
    NonString = Test-ManagedInstallPathIdentity -Candidate 7 -Expected $env:CTX_TEST_ORDINARY
} | ConvertTo-Json -Compress
`;
    const ordinary = String.raw`C:\Users\owner\.local\bin\ctx.exe`;
    const result = spawnSync(
      powerShellCommand,
      ["-NoProfile", "-NonInteractive", "-Command", command],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          CTX_TEST_ORDINARY: ordinary,
          CTX_TEST_EXTENDED: String.raw`\\?\C:\Users\owner\.local\bin\ctx.exe`,
          CTX_TEST_DIFFERENT_LEAF: String.raw`C:\Users\owner\.local\bin\other.exe`,
          CTX_TEST_DIFFERENT_DRIVE: String.raw`D:\Users\owner\.local\bin\ctx.exe`,
          CTX_TEST_DIFFERENT_CASE: String.raw`C:\Users\Owner\.local\bin\ctx.exe`,
          CTX_TEST_RELATIVE: String.raw`.local\bin\ctx.exe`,
          CTX_TEST_TRAVERSAL: String.raw`C:\Users\owner\other\..\.local\bin\ctx.exe`,
          CTX_TEST_FORWARD_SLASH: String.raw`C:/Users/owner/.local/bin/ctx.exe`,
          CTX_TEST_DUPLICATE_SEPARATOR: String.raw`C:\Users\owner\\.local\bin\ctx.exe`,
          CTX_TEST_UNC: String.raw`\\server\share\ctx.exe`,
          CTX_TEST_VERBATIM_UNC: String.raw`\\?\UNC\server\share\ctx.exe`,
          CTX_TEST_DEVICE_PATH: String.raw`\\?\GLOBALROOT\Device\HarddiskVolume1\ctx.exe`,
        },
      },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), {
      Ordinary: true,
      ExtendedCandidate: true,
      ExtendedExpected: true,
      DifferentLeaf: false,
      DifferentDrive: false,
      DifferentCase: false,
      Relative: false,
      Traversal: false,
      ForwardSlash: false,
      DuplicateSeparator: false,
      Unc: false,
      VerbatimUnc: false,
      DeviceNamespace: false,
      ExactDeviceNamespace: false,
      NonString: false,
    });
  },
);

test("rendered Windows installer invokes the candidate Core once for a fresh signed pair", () => {
  const body = renderCliInstallPowerShellScript();
  assert.match(body, /CTX_RELEASE_MANAGED_PAIR_ENVELOPE_windows_x64/u);
  assert.match(body, /CTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_windows_x64/u);
  assert.match(body, /CTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_windows_x64/u);
  assert.match(body, /managed-pair Core checksum differs from release metadata/u);
  const pairPublication = body.indexOf("Invoke-ManagedPairApply", body.indexOf("if ($managedReinstall)"));
  const setup = body.indexOf('$setupArgs = @("setup", "--quiet", "--format", "json")');
  assert.ok(
    pairPublication >= 0 && setup > pairPublication,
    "Windows must complete candidate-Core pair apply before setup",
  );
  assert.match(
    body,
    /Invoke-ExecutableCaptured -Executable \$downloadPath -Arguments @\(\s+"--ctx-core-managed-pair-apply-v1",\s+\$pairInstallRoot,\s+"-",\s+\$pairEnvelopePath,\s+\$downloadPath,\s+\$pairCompanionPath,\s+\$MarkerSource/u,
  );
  assert.match(body, /Invoke-ManagedPairApply -MarkerSource \$markerSourcePath -Required \$true/u);
  assert.equal(body.match(/--ctx-core-managed-pair-apply-v1/gu)?.length, 1);
  assert.equal(body.match(/--ctx-core-hosted-pair-install-v1/gu)?.length, 1);
  assert.match(body, /candidate Core returned invalid managed-pair apply proof/u);
  assert.match(body, /ConvertFrom-Json/u);
  assert.match(body, /"warnings"/u);
  const receiptHelperStart = body.indexOf("function Read-BoundedSuccessReceipt");
  const receiptHelperEnd = body.indexOf("function Invoke-CtxCaptured", receiptHelperStart);
  const receiptHelper = body.slice(receiptHelperStart, receiptHelperEnd);
  const pairApply = body.match(
    /function Invoke-ManagedPairApply\([^\n]+\) \{[\s\S]*?^\}/m,
  )?.[0] ?? "";
  const receiptBound = receiptHelper.indexOf("$item.Length -gt $MaximumBytes");
  const receiptRead = receiptHelper.indexOf("[IO.File]::ReadAllText($Path)");
  assert.match(pairApply, /-MaximumBytes 512/u);
  assert.ok(
    receiptBound >= 0 && receiptRead > receiptBound,
    "managed-pair receipt capture must be bounded before it is read",
  );
  assert.match(receiptHelper, /\$_ -ceq \$property\.Name/u);
  assert.match(receiptHelper, /\$warningValue -isnot \[System\.Array\]/u);
  assert.match(receiptHelper, /\$warnings\.Count -gt 4/u);
  assert.match(receiptHelper, /\$_\.Length -gt 160/u);
  assert.doesNotMatch(pairApply, /\*> \$commandOutputPath/u);

  const transaction = body.slice(
    body.indexOf("function Invoke-HostedInstallTransaction"),
    body.indexOf("function Invoke-ManagedCoreUpgrade"),
  );
  assert.match(transaction, /-MaximumBytes 64KB/u);
  assert.match(transaction, /-OptionalNames @\("warnings"\)/u);
  assert.doesNotMatch(transaction, /ReadAllText\(\$transaction\.OutputPath\)/u);
});

test(
  "Windows managed-pair receipts enforce exact warningful success on clean stdout",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const body = renderCliInstallPowerShellScript();
    const pairApply = body.match(
      /function Invoke-ManagedPairApply\([^\n]+\) \{[\s\S]*?^\}/m,
    )?.[0];
    assert.ok(pairApply);
    const receiptHelpers = body.slice(
      body.indexOf("function Read-BoundedSuccessReceipt"),
      body.indexOf("function Invoke-CtxCaptured"),
    );
    const currentValidator = body.slice(body.indexOf("function Assert-ManagedUpgradeResult"), body.indexOf("function Assert-LegacyManagedUpgradeResult"));
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-pair-receipt-"));
    try {
      const command = `${receiptHelpers}
${currentValidator}
${pairApply}
function Protect-ManagedPath { param($Path, [switch]$Directory) }
function Invoke-ExecutableCaptured {
    param([string]$Executable, [string[]]$Arguments)
    return [pscustomobject]@{
        ExitCode = 0
        OutputPath = $script:pairReceiptPath
        ErrorPath = $script:pairReceiptPath
    }
}
function Write-BoundedCapturedChildError { param([string]$ErrorPath) }
function Fail([string]$Message) { throw $Message }
$downloadPath = "candidate"
$pairInstallRoot = "install-root"
$pairEnvelopePath = "envelope"
$pairCompanionPath = "companion"
$canonical = '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
$warningful = '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["pair cleanup pending"]}'
$nativeReceipt = $canonical + [Environment]::NewLine
$alternateLineEnding = if ([Environment]::NewLine -ceq "\`r\`n") { "\`n" } else { "\`r\`n" }
$utf8 = [System.Text.UTF8Encoding]::new($false)
$utf16 = [System.Text.UnicodeEncoding]::new($false, $true)
$cases = @(
    [pscustomobject]@{ Name = "utf8-native"; Bytes = $utf8.GetBytes($nativeReceipt); Expected = $true },
    [pscustomobject]@{ Name = "utf16-native"; Bytes = [byte[]]($utf16.GetPreamble() + $utf16.GetBytes($nativeReceipt)); Expected = $true },
    [pscustomobject]@{ Name = "warningful"; Bytes = $utf8.GetBytes($warningful + [Environment]::NewLine); Expected = $true },
    [pscustomobject]@{ Name = "scalar-warning"; Bytes = $utf8.GetBytes('{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":"scalar"}' + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "null-warning"; Bytes = $utf8.GetBytes('{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":null}' + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "case-collision"; Bytes = $utf8.GetBytes('{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["one"],"Warnings":["two"]}' + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "extra-field"; Bytes = $utf8.GetBytes('{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","extra":1}' + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "too-many-warnings"; Bytes = $utf8.GetBytes('{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["one","two","three","four","five"]}' + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "unsafe-warning"; Bytes = $utf8.GetBytes('{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed","warnings":["unsafe!"]}' + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "missing-ending"; Bytes = $utf8.GetBytes($canonical); Expected = $false },
    [pscustomobject]@{ Name = "alternate-ending"; Bytes = $utf8.GetBytes($canonical + $alternateLineEnding); Expected = $true },
    [pscustomobject]@{ Name = "extra-line"; Bytes = $utf8.GetBytes($nativeReceipt + "extra" + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "extra-content"; Bytes = $utf8.GetBytes($canonical + "x" + [Environment]::NewLine); Expected = $false },
    [pscustomobject]@{ Name = "oversized"; Bytes = $utf8.GetBytes($nativeReceipt + ("x" * 600)); Expected = $false }
)
foreach ($case in $cases) {
    $script:pairReceiptPath = Join-Path $env:CTX_TEST_PAIR_RECEIPT_ROOT ($case.Name + ".out")
    [IO.File]::WriteAllBytes($script:pairReceiptPath, $case.Bytes)
    $accepted = $false
    try {
        $accepted = [bool](Invoke-ManagedPairApply -MarkerSource "marker" -Required $true)
    } catch {
        $accepted = $false
    }
    [Console]::Out.WriteLine("{0}|{1}|{2}", $case.Name, $accepted, $case.Expected)
}
$script:pairReceiptPath = Join-Path $env:CTX_TEST_PAIR_RECEIPT_ROOT "empty-lifecycle.out"
[IO.File]::WriteAllText($script:pairReceiptPath, '{"schema_version":1,"command":"upgrade","ok":true,"status":"applied","warnings":[]}' + [Environment]::NewLine)
$lifecycle = Read-BoundedSuccessReceipt -Path $script:pairReceiptPath -MaximumBytes 512 -RequiredNames @("schema_version","command","ok","status","warnings") -OptionalNames @() -AllowEmptyWarnings
[Console]::Out.WriteLine("empty-lifecycle|{0}|True", ($null -ne $lifecycle))
function Test-ManagedInstallPathIdentity { return $true }
$version = "1.2.3"; $channel = "stable"; $installPath = "install-root"
$uppercase = '{"schema_version":1,"command":"upgrade","ok":true,"status":"APPLIED","message":"done","current_version":"1.2.3","latest_version":"1.2.3","update_available":false,"update_was_available":true,"channel":"stable","platform":"windows-x64","metadata_url":"metadata","artifact_url":"artifact","install_path":"install-root","managed":true,"applied":true,"dry_run":false,"warnings":[],"upgrade_attempt_id":"ua_test"}' | ConvertFrom-Json
$uppercaseAccepted = $true; try { $null = Assert-ManagedUpgradeResult -Result $uppercase } catch { $uppercaseAccepted = $false }
[Console]::Out.WriteLine("uppercase-status|{0}|False", $uppercaseAccepted)`;
      const result = spawnSync(
        powerShellCommand,
        ["-NoProfile", "-NonInteractive", "-Command", command],
        {
          encoding: "utf8",
          env: {
            ...process.env,
            CTX_TEST_PAIR_RECEIPT_ROOT: sandbox,
          },
        },
      );
      assert.equal(result.status, 0, result.stderr);
      const outcomes = result.stdout.replaceAll("\r", "").trim().split("\n");
      assert.deepEqual(outcomes, [
        "utf8-native|True|True",
        "utf16-native|True|True",
        "warningful|True|True",
        "scalar-warning|False|False",
        "null-warning|False|False",
        "case-collision|False|False",
        "extra-field|False|False",
        "too-many-warnings|False|False",
        "unsafe-warning|False|False",
        "missing-ending|False|False",
        "alternate-ending|True|True",
        "extra-line|False|False",
        "extra-content|False|False",
        "oversized|False|False",
        "empty-lifecycle|True|True",
        "uppercase-status|False|False",
      ]);
      assert.equal(
        result.stderr.replaceAll("\r", ""),
        "warning: pair cleanup pending\n",
      );
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test("rendered Windows managed rerun delegates lifecycle before executable mutation", () => {
  const body = renderCliInstallPowerShellScript();
  assert.match(body, /function Read-ExistingManagedInstall/);
  assert.match(body, /function Assert-ManagedUpgradeResult/);
  assert.match(body, /function Assert-LegacyManagedUpgradeResult/);
  const currentReceiptValidator = body.slice(
    body.indexOf("function Assert-ManagedUpgradeResult"),
    body.indexOf("function Assert-LegacyManagedUpgradeResult"),
  );
  const v025ReceiptValidator = body.slice(
    body.indexOf("function Assert-LegacyManagedUpgradeResult"),
    body.indexOf("function Test-InstalledTargetIdentity"),
  );
  assert.doesNotMatch(currentReceiptValidator, /"path"/);
  assert.match(v025ReceiptValidator, /\$Result\.path -isnot \[pscustomobject\]/);
  assert.match(body, /function Invoke-ManagedCoreUpgrade/);
  assert.match(body, /function Test-InstalledTargetIdentity/);
  assert.match(
    body,
    /\$upgradeCommand = Invoke-CtxCaptured -Arguments @\(\s*"upgrade",\s*"--channel",\s*\$channel,\s*"--format=json"\s*\)/,
  );
  assert.match(
    body,
    /\$existingManagedInstall\.version -ceq "0\.25\.0"/,
  );
  assert.match(
    body,
    /32aa550cc5c56d4d2989d0f929bbc1e634d8b730219feb8e4a4ba770b02a9867/,
  );
  assert.match(
    body,
    /if \(\$legacyManagedReinstall\) \{\s+\$upgradeCommand = Invoke-CtxCaptured -Arguments @\(\s*"upgrade",\s*"--channel",\s*\$channel,\s*"--json"\s*\)/,
  );
  assert.match(currentReceiptValidator, /\$Result\.status -cnotin @\("applied", "up_to_date", "scheduled"\)/u);
  assert.match(currentReceiptValidator, /\$Result\.status -ceq "applied"/u);
  assert.match(currentReceiptValidator, /\$Result\.status -cne "scheduled"/u);
  assert.match(currentReceiptValidator, /\$Result\.status -cin @\("applied", "scheduled"\)/u);
  assert.doesNotMatch(currentReceiptValidator, /\$Result\.status -(?:eq|ne|in|notin)\b/u);
  for (const field of ["command", "latest_version", "channel", "platform"]) assert.match(currentReceiptValidator, new RegExp(`\\$Result\\.${field} -cne`, "u"));
  assert.match(body, /\$upgradeProof = Read-BoundedSuccessReceipt[\s\S]*-AllowEmptyWarnings/u);
  assert.match(body, /\$upgradeRequired = if \(\$legacyManagedReinstall\)/u);
  assert.doesNotMatch(
    body.match(/function Invoke-ManagedCoreUpgrade \{[\s\S]*?^\}/m)?.[0] ?? "",
    /ReadAllText\(\$upgradeCommand\.OutputPath\)|ConvertFrom-Json/u,
  );
  assert.match(
    body,
    /disable any opted-in legacy daemon, wait for it to exit or stop\/reboot the host, then rerun this installer/,
  );
  const managedSelection = body.lastIndexOf(
    "$managedReinstall = $null -ne $existingManagedInstall",
  );
  const authoritativePairRead = body.lastIndexOf(
    "$existingManagedInstall = Read-ExistingManagedInstall",
  );
  const destinationGuardValidation = body.lastIndexOf(
    "$installGuard.AssertUnchanged()",
    authoritativePairRead,
  );
  const destinationGuardsReleased = body.lastIndexOf(
    "$installGuard.Dispose()",
  );
  const managedHandoff = body.lastIndexOf("Invoke-ManagedCoreUpgrade");
  assert.ok(
    managedSelection >= 0 &&
      destinationGuardValidation < authoritativePairRead &&
      managedSelection < destinationGuardsReleased &&
      destinationGuardsReleased < managedHandoff,
    "Windows must release no-delete-share guards before Core atomic replacement",
  );
  assert.doesNotMatch(body, /Write-ManagedMarkerAfterUpgrade/);
  assert.doesNotMatch(
    body.slice(managedHandoff),
    /CtxInstallerLeafWriter|WriteBytes\(\$markerBytes\)/,
  );
  assert.match(
    body,
    /\} else \{\s+Invoke-ManagedCoreUpgrade\s+if \(\$releasedPairInstall\) \{\s+Invoke-ReleasedManagedPairInstall\s+\}\s+\}\s+\} elseif \(\$managedPair\) \{/u,
  );
  assert.doesNotMatch(
    body.slice(managedHandoff, body.indexOf('} elseif ($managedPair)', managedHandoff)),
    /Invoke-ManagedPairApply/,
    "reinstalls outside the exact-0.25 recovery must keep the installed Core as the replacement owner",
  );
  assert.doesNotMatch(
    body.match(/function Invoke-ManagedCoreUpgrade \{[\s\S]*?^\}/m)?.[0] ?? "",
    /CtxInstallerLeafWriter|WriteFromFile|SetLength|Stop-Process|Get-Process/,
  );
  assert.match(
    body,
    /\$currentRequired = "schema_version,command,ok,status,message,current_version,latest_version,update_available,update_was_available,channel,platform,metadata_url,artifact_url,install_path,managed,applied,dry_run,warnings,upgrade_attempt_id"\.Split/,
  );
  assert.match(
    body,
    /\$legacyRequired = "schema_version,command,ok,status,message,current_version,latest_version,update_available,channel,platform,metadata_url,artifact_url,install_path,managed,applied,dry_run,warnings"\.Split/,
  );
});

test("installers hand setup the ambient contract without enumerating or dumping secrets", () => {
  const posix = renderCliInstallScript();
  const powershell = renderCliInstallPowerShellScript();
  assert.match(posix, /CTX_HOSTED_INSTALLER_SETUP=1 "\$install_path" "\$@"/);
  assert.match(posix, /"\$@"\s+>"\$tmp_dir\/setup-receipt\.json"/);
  assert.match(powershell, /\$process = Start-Process @startArguments/);
  assert.doesNotMatch(powershell, /\*> \$commandOutputPath/);
  assert.match(
    powershell,
    /Invoke-ExecutableCaptured -Executable \$installPath -Arguments \$Arguments/,
  );
  for (const body of [posix, powershell]) {
    assert.doesNotMatch(
      body,
      /(?:env -i|GetEnvironmentVariables|Get-ChildItem\s+Env:|printenv|set\s*>\s*.*env)/i,
    );
    assert.doesNotMatch(
      body,
      /AWS_SECRET_ACCESS_KEY|AZURE_CLIENT_SECRET|GITHUB_TOKEN|OPENAI_API_KEY/,
    );
  }
});

test(
  "rendered Windows child failure output is capped, sanitized, and path-scoped",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const body = renderCliInstallPowerShellScript();
    const helperStart = body.indexOf("function Write-BoundedCapturedChildError");
    const helperEnd = body.indexOf("function Invoke-CtxCaptured", helperStart);
    assert.ok(helperStart >= 0 && helperEnd > helperStart);
    const helper = body.slice(helperStart, helperEnd);
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-child-output-"));
    const captureRoot = path.join(sandbox, "capture");
    const outsideRoot = path.join(sandbox, "outside");
    const capturePath = path.join(
      captureRoot,
      `ctx-command-${"a".repeat(32)}.err`,
    );
    const outsidePath = path.join(
      outsideRoot,
      `ctx-command-${"b".repeat(32)}.err`,
    );
    mkdirSync(captureRoot);
    mkdirSync(outsideRoot);
    writeFileSync(
      capturePath,
      `Error: exact child failure\ntoken=assignment-secret\nBearer standalone-secret\nhttps://example.test/fail?token=query-secret&mode=one\n\u001b[31m${"A".repeat(9000)}\nUNREACHABLE-END`,
    );
    writeFileSync(outsidePath, "outside-secret-must-not-print");
    try {
      const command = `$ErrorActionPreference = "Stop"
$tempRoot = $env:CTX_TEST_CAPTURE_ROOT
${helper}
Write-BoundedCapturedChildError -ErrorPath $env:CTX_TEST_CAPTURE_PATH
Write-BoundedCapturedChildError -ErrorPath $env:CTX_TEST_OUTSIDE_PATH
`;
      const result = spawnSync(
        powerShellCommand,
        ["-NoProfile", "-NonInteractive", "-Command", command],
        {
          encoding: "utf8",
          env: {
            ...process.env,
            CTX_TEST_CAPTURE_ROOT: captureRoot,
            CTX_TEST_CAPTURE_PATH: capturePath,
            CTX_TEST_OUTSIDE_PATH: outsidePath,
          },
        },
      );
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout, "");
      assert.match(result.stderr, /ctx child output:/);
      assert.match(result.stderr, /Error: exact child failure/);
      assert.match(result.stderr, /token=<redacted>/i);
      assert.match(result.stderr, /ctx child output truncated/);
      assert.doesNotMatch(
        result.stderr,
        /assignment-secret|standalone-secret|query-secret|outside-secret-must-not-print|UNREACHABLE-END|\u001b/,
      );
      assert.ok(result.stderr.length < 9_000, "captured diagnostic must stay bounded");
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test("installers scope the hosted setup marker to one native setup receipt invocation", () => {
  const posix = renderCliInstallScript();
  const powershell = renderCliInstallPowerShellScript();

  assert.equal(
    [...posix.matchAll(/CTX_HOSTED_INSTALLER_SETUP=1/g)].length,
    1,
  );
  assert.match(
    posix,
    /CTX_HOSTED_INSTALLER_SETUP=1 "\$install_path" "\$@"/,
  );
  assert.match(posix, /"\$@"\s+>"\$tmp_dir\/setup-receipt\.json"/);
  assert.doesNotMatch(posix, /status --format=json|pro setup --trial-only/);
  assert.doesNotMatch(posix, /export\s+CTX_HOSTED_INSTALLER_SETUP/);
  assert.doesNotMatch(
    posix,
    /CTX_HOSTED_INSTALLER_SETUP=1 "\$install_path" (?:upgrade|docs|integrations|pro)\b/,
  );

  const markerWrapper = powershell.match(
    /function Invoke-HostedInstallerSetupCtxCaptured\([\s\S]*?^\}/m,
  )?.[0];
  assert.ok(markerWrapper);
  assert.match(
    markerWrapper,
    /GetEnvironmentVariable\(\s*"CTX_HOSTED_INSTALLER_SETUP",\s*"Process"\s*\)/,
  );
  assert.match(
    markerWrapper,
    /SetEnvironmentVariable\(\s*"CTX_HOSTED_INSTALLER_SETUP",\s*"1",\s*"Process"\s*\)/,
  );
  assert.match(
    markerWrapper,
    /finally \{\s*\[Environment\]::SetEnvironmentVariable\(\s*"CTX_HOSTED_INSTALLER_SETUP",\s*\$previousHostedInstallerSetup,\s*"Process"\s*\)/,
  );
  assert.equal(
    [...powershell.matchAll(/Invoke-HostedInstallerSetupCtxCaptured/g)].length,
    2,
    "the wrapper definition plus one setup invocation must be the only occurrences",
  );
  assert.match(
    powershell,
    /\$setupCommand = Invoke-HostedInstallerSetupCtxCaptured -Arguments \$setupArgs -InheritStandardError:\(\$SetupProgress -cne "none"\)/,
  );
  assert.doesNotMatch(powershell, /\$statusCommand|"status",\s*"--format=json"/);
  assert.doesNotMatch(powershell, /\$env:CTX_HOSTED_INSTALLER_SETUP\s*=/);
  assert.match(powershell, /\$skillStatus = Invoke-CtxQuiet -Arguments \$skillArgs/);
  assert.doesNotMatch(powershell, /Invoke-CtxQuiet -Arguments @\(\s*"pro",\s*"setup"/);
});

test("POSIX and PowerShell installers share the exact stage/status vocabulary", () => {
  const posixPairs = [...renderCliInstallScript().matchAll(
    /report_install_stage "([a-z_]+)" "([a-z]+)"/g,
  )].map((match) => `${match[1]}:${match[2]}`);
  const powerShellPairs = [...renderCliInstallPowerShellScript().matchAll(
    /Send-InstallStage -Stage "([a-z_]+)" -Status "([a-z]+)"/g,
  )].map((match) => `${match[1]}:${match[2]}`);
  const expected = Object.entries(INSTALL_STAGE_STATUS_PAIRS)
    .filter(([stage]) => stage !== "uninstall")
    .flatMap(([stage, statuses]) => statuses.map((status) => `${stage}:${status}`))
    .sort();

  assert.deepEqual([...new Set(posixPairs)].sort(), expected);
  assert.deepEqual([...new Set(powerShellPairs)].sort(), expected);
});

test("POSIX and PowerShell installer diagnostics use short budgets and failure latches", () => {
  const posix = renderCliInstallScript();
  const powershell = renderCliInstallPowerShellScript();

  assert.match(posix, /install_stage_delivery_enabled=1/);
  assert.match(posix, /--connect-timeout 1 --max-time 1/);
  assert.match(posix, /install_stage_delivery_enabled=0/);
  assert.match(powershell, /\$script:installStageDeliveryEnabled = \$true/);
  assert.match(powershell, /-TimeoutSec 1/);
  assert.match(powershell, /\$script:installStageDeliveryEnabled = \$false/);
});

test(
  "rendered Windows CLI installer rejects a relative install directory before mutation",
  { skip: powerShellCommand ? false : "PowerShell is not installed" },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-relative-bin-"));
    try {
      const dataRoot = path.join(sandbox, "data");
      const installerTmpRoot = path.join(sandbox, "installer-tmp");
      const scriptPath = path.join(sandbox, "install.ps1");
      mkdirSync(dataRoot);
      mkdirSync(installerTmpRoot);
      writeFileSync(
        scriptPath,
        renderCliInstallPowerShellScript(),
      );
      const result = spawnSync(
        powerShellCommand,
        [
          "-NoProfile",
          "-File",
          scriptPath,
          "-BinDir",
          "relative-bin",
          "-NoSetup",
          "-DryRun",
        ],
        {
          encoding: "utf8",
          env: {
            ...process.env,
            HOME: sandbox,
            CTX_DATA_ROOT: dataRoot,
            TEMP: installerTmpRoot,
            TMP: installerTmpRoot,
          },
        },
      );
      assert.notEqual(result.status, 0);
      assert.match(
        `${result.stdout}${result.stderr}`,
        /ctx install directory must be an absolute path/,
      );
      assert.deepEqual(readdirSync(installerTmpRoot), []);
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test(
  "rendered Windows CLI installer rejects a junction in an install-path ancestor",
  {
    skip: process.platform === "win32" && powerShellCommand
      ? false
      : "requires Windows PowerShell",
  },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-reparse-ancestor-"));
    try {
      const targetDirectory = path.join(sandbox, "target");
      const targetChild = path.join(targetDirectory, "bin");
      const linkedAncestor = path.join(sandbox, "linked");
      const linkedChild = path.join(linkedAncestor, "bin");
      const scriptPath = path.join(sandbox, "check-install-ancestor.ps1");
      mkdirSync(targetChild, { recursive: true });
      symlinkSync(targetDirectory, linkedAncestor, "junction");

      const body = renderCliInstallPowerShellScript();
      writeFileSync(
        scriptPath,
        `${powerShellNativeGuardBlock(body)}
$guard = [CtxInstallerPathGuard]::AcquireDirectory(
    $env:CTX_TEST_INSTALL_DIRECTORY,
    $true
)
try {
    $guard.AssertUnchanged()
} finally {
    $guard.Dispose()
}
`,
      );

      const ordinary = spawnSync(powerShellCommand, ["-NoProfile", "-File", scriptPath], {
        encoding: "utf8",
        env: {
          ...process.env,
          CTX_TEST_INSTALL_DIRECTORY: targetChild,
        },
      });
      assert.equal(ordinary.status, 0, `${ordinary.stdout}${ordinary.stderr}`);

      const linked = spawnSync(powerShellCommand, ["-NoProfile", "-File", scriptPath], {
        encoding: "utf8",
        env: {
          ...process.env,
          CTX_TEST_INSTALL_DIRECTORY: linkedChild,
        },
      });
      assert.notEqual(linked.status, 0);
      assert.match(
        `${linked.stdout}${linked.stderr}`,
        /ctx install path must not contain reparse points/,
      );
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test(
  "rendered Windows CLI installer rejects binary and marker destination reparse points",
  {
    skip: process.platform === "win32" && powerShellCommand
      ? false
      : "requires Windows PowerShell",
  },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-reparse-leaves-"));
    try {
      const installBin = path.join(sandbox, "bin");
      const binaryTarget = path.join(sandbox, "binary-target");
      const markerTarget = path.join(sandbox, "marker-target");
      const binaryPath = path.join(installBin, "ctx.exe");
      const markerPath = path.join(installBin, "ctx.exe.install.json");
      const scriptPath = path.join(sandbox, "check-install-leaf.ps1");
      mkdirSync(installBin);
      mkdirSync(binaryTarget);
      mkdirSync(markerTarget);
      symlinkSync(binaryTarget, binaryPath, "junction");
      symlinkSync(markerTarget, markerPath, "junction");

      const body = renderCliInstallPowerShellScript();
      writeFileSync(
        scriptPath,
        `${powerShellNativeGuardBlock(body)}
$guard = [CtxInstallerPathGuard]::AcquireLeaf($env:CTX_TEST_INSTALL_LEAF)
try {
    $guard.AssertUnchanged()
} finally {
    $guard.Dispose()
}
`,
      );

      for (const leafPath of [binaryPath, markerPath]) {
        const result = spawnSync(powerShellCommand, ["-NoProfile", "-File", scriptPath], {
          encoding: "utf8",
          env: {
            ...process.env,
            CTX_TEST_INSTALL_LEAF: leafPath,
          },
        });
        assert.notEqual(result.status, 0, leafPath);
        assert.match(
          `${result.stdout}${result.stderr}`,
          /ctx install destination must not contain reparse points/,
          leafPath,
        );
      }
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);

test("native Windows ACL repair keeps exact owner/SYSTEM authority and leaf bytes", {
  skip: process.platform === "win32" && powerShellCommand ? false : "requires native Windows",
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), "ctx-native-installer-acl-"));
  try {
    const guard = path.join(root, "guard.ps1");
    writeFileSync(guard, powerShellNativeGuardBlock(renderCliInstallPowerShellScript()));
    const result = spawnSync(powerShellCommand, ["-NoProfile", "-NonInteractive", "-File",
      fileURLToPath(new URL("./cli-install-powershell-acl-fixture.ps1", import.meta.url)),
      "-GuardPath", guard, "-WorkRoot", root], { encoding: "utf8", timeout: 30_000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), { checked_paths: 3, leaves_unchanged: true });
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test(
  "rendered Windows CLI installer rejects hard-linked binary and marker destinations",
  {
    skip: process.platform === "win32" && powerShellCommand
      ? false
      : "requires Windows PowerShell",
  },
  () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ctx-cli-install-ps-hardlink-leaves-"));
    try {
      const installBin = path.join(sandbox, "bin");
      const binaryTarget = path.join(sandbox, "binary-target.exe");
      const markerTarget = path.join(sandbox, "marker-target.json");
      const binaryPath = path.join(installBin, "ctx.exe");
      const markerPath = path.join(installBin, "ctx.exe.install.json");
      const scriptPath = path.join(sandbox, "check-install-hardlink-leaf.ps1");
      mkdirSync(installBin);
      writeFileSync(binaryTarget, "binary target");
      writeFileSync(markerTarget, "marker target");
      linkSync(binaryTarget, binaryPath);
      linkSync(markerTarget, markerPath);

      for (const [label, targetPath, leafPath] of [
        ["binary", binaryTarget, binaryPath],
        ["marker", markerTarget, markerPath],
      ]) {
        assert.ok(statSync(targetPath).nlink > 1, `${label} target must be hard linked`);
        assert.ok(statSync(leafPath).nlink > 1, `${label} destination must be hard linked`);
      }

      const body = renderCliInstallPowerShellScript();
      writeFileSync(
        scriptPath,
        `${powerShellNativeGuardBlock(body)}
$guard = [CtxInstallerPathGuard]::AcquireLeaf($env:CTX_TEST_INSTALL_LEAF)
try {
    $guard.AssertUnchanged()
} finally {
    $guard.Dispose()
}
`,
      );

      for (const [label, leafPath] of [
        ["binary", binaryPath],
        ["marker", markerPath],
      ]) {
        const result = spawnSync(powerShellCommand, ["-NoProfile", "-File", scriptPath], {
          encoding: "utf8",
          env: {
            ...process.env,
            CTX_TEST_INSTALL_LEAF: leafPath,
          },
        });
        assert.notEqual(result.status, 0, `${label}: ${result.stdout}${result.stderr}`);
        assert.match(
          `${result.stdout}${result.stderr}`,
          /ctx install destination must not be a hard link/,
          label,
        );
      }
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  },
);
