Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This contract test must run on Windows"
}

function Test-Windows11WorkstationBaseline {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Caption,
        [Parameter(Mandatory = $true)]
        [string]$Version,
        [Parameter(Mandatory = $true)]
        [int]$ProductType
    )

    $parsedVersion = $null
    if (-not [System.Version]::TryParse($Version, [ref]$parsedVersion)) {
        return $false
    }
    return (
        $Caption -cmatch '^Microsoft\s+Windows\s+11(?:\s|$)' -and
        $ProductType -eq 1 -and
        $parsedVersion.Major -eq 10 -and
        $parsedVersion.Minor -eq 0 -and
        $parsedVersion.Build -ge 22000
    )
}

if (-not (Test-Windows11WorkstationBaseline `
    -Caption "Microsoft Windows 11 Pro" `
    -Version "10.0.22000" `
    -ProductType 1)) {
    throw "Exact synthetic Windows 11 workstation baseline was rejected"
}
foreach ($wrongBaseline in @(
    [PSCustomObject]@{
        Caption = "Microsoft Windows 10 Pro"
        Version = "10.0.19045"
        ProductType = 1
    },
    [PSCustomObject]@{
        Caption = "Microsoft Windows 11 Pro"
        Version = "10.0.21999"
        ProductType = 1
    },
    [PSCustomObject]@{
        Caption = "Microsoft Windows Server 2025 Datacenter"
        Version = "10.0.26100"
        ProductType = 3
    }
)) {
    if (Test-Windows11WorkstationBaseline `
        -Caption $wrongBaseline.Caption `
        -Version $wrongBaseline.Version `
        -ProductType $wrongBaseline.ProductType) {
        throw "Wrong synthetic Windows OS baseline was accepted: $($wrongBaseline.Caption)"
    }
}

$hostWindows = Get-CimInstance Win32_OperatingSystem
if (
    $null -eq $hostWindows -or
    -not (Test-Windows11WorkstationBaseline `
        -Caption ([string]$hostWindows.Caption).Trim() `
        -Version ([string]$hostWindows.Version) `
        -ProductType ([int]$hostWindows.ProductType))
) {
    throw (
        "Windows semantic smoke requires a Windows 11 workstation at build 22000 or newer; " +
        "got $($hostWindows.Caption) $($hostWindows.Version) ProductType=$($hostWindows.ProductType)"
    )
}

$smokeScript = Join-Path $PSScriptRoot "smoke-daemon-semantic-release.ps1"
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $smokeScript,
    [ref]$tokens,
    [ref]$parseErrors
)
if ($parseErrors.Count -ne 0) {
    throw "Windows semantic smoke script did not parse: $($parseErrors[0].Message)"
}

$runtimeModeParameters = @(
    $ast.ParamBlock.Parameters | Where-Object {
        $_.Name.VariablePath.UserPath -ceq "RuntimeMode"
    }
)
if (
    $runtimeModeParameters.Count -ne 1 -or
    $null -eq $runtimeModeParameters[0].DefaultValue -or
    $runtimeModeParameters[0].DefaultValue.Extent.Text -cne '"onnxruntime"'
) {
    throw "Windows semantic smoke must default RuntimeMode exactly to legacy onnxruntime"
}

$signedProvisionedParameters = @(
    $ast.ParamBlock.Parameters | Where-Object {
        $_.Name.VariablePath.UserPath -ceq "SignedProvisioned"
    }
)
if ($signedProvisionedParameters.Count -ne 1) {
    throw "Windows semantic smoke must expose exactly one SignedProvisioned switch"
}

$buildInfoParameters = @(
    $ast.ParamBlock.Parameters | Where-Object {
        $_.Name.VariablePath.UserPath -ceq "BuildInfo"
    }
)
if ($buildInfoParameters.Count -ne 1) {
    throw "Windows semantic smoke must expose exactly one optional BuildInfo input"
}

$requiredFunctions = @(
    "Get-WindowsRuntimeContract",
    "Assert-WindowsRuntimeArchive",
    "Get-BoundWindowsBuildInfoSha256",
    "New-UniqueFixtureRoot",
    "Complete-SmokeTeardown",
    "Invoke-Ctx",
    "Invoke-CtxChecked"
)
foreach ($name in $requiredFunctions) {
    $matches = @(
        $ast.FindAll(
            {
                param($node)
                $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
                    $node.Name -ceq $name
            },
            $true
        )
    )
    if ($matches.Count -ne 1) {
        throw "Expected exactly one $name function in the Windows semantic smoke script"
    }
    Invoke-Expression $matches[0].Extent.Text
}

$legacyRuntime = Get-WindowsRuntimeContract -Mode "onnxruntime"
if (
    $legacyRuntime.Asset -cne "ctx-onnxruntime-windows-x64.zip" -or
    $legacyRuntime.Version -cne "1.27.0" -or
    $legacyRuntime.EmbeddingBackend -cne "cpu" -or
    $legacyRuntime.BackendPreference -cne "cpu" -or
    @($legacyRuntime.Files).Count -ne 10
) {
    throw "Legacy Windows ONNX Runtime smoke contract changed"
}

$windowsMlRuntime = Get-WindowsRuntimeContract -Mode "windows-ml"
$expectedWindowsMlFiles = @(
    "LICENSE",
    "ThirdPartyNotices.txt",
    "lib/Microsoft.Windows.AI.MachineLearning.dll",
    "lib/onnxruntime.dll",
    "lib/DirectML.dll"
)
if (
    $windowsMlRuntime.Asset -cne "ctx-windowsml-windows-x64.zip" -or
    $windowsMlRuntime.Version -cne "2.1.74" -or
    $windowsMlRuntime.EmbeddingBackend -cne "windows-ml" -or
    $windowsMlRuntime.BackendPreference -cne "windowsml" -or
    $windowsMlRuntime.StatusBackend -cne "windows_ml" -or
    $windowsMlRuntime.ExecutionProvider -cne "WindowsML:DmlExecutionProvider:GPU" -or
    (@($windowsMlRuntime.Files) -join "`n") -cne ($expectedWindowsMlFiles -join "`n")
) {
    throw "Windows ML smoke contract does not select the exact runtime layout"
}

$unknownModeFailure = $null
try {
    Get-WindowsRuntimeContract -Mode "synthesized" | Out-Null
} catch {
    $unknownModeFailure = $_.Exception.Message
}
if (
    [string]::IsNullOrEmpty($unknownModeFailure) -or
    -not $unknownModeFailure.Contains("must be exactly onnxruntime or windows-ml")
) {
    throw "Windows semantic smoke accepted an unknown runtime mode"
}

$smokeSource = [System.IO.File]::ReadAllText($smokeScript)
foreach ($requiredSignedProvisioningContract in @(
    'Windows ML proof requires -SignedProvisioned after hosted signed model/runtime provisioning',
    '$binaryMarker.manager -cne "ctx-hosted-installer"',
    '$runtimeMarker.metadata_trust -cne "signed-release-metadata"'
)) {
    if (-not $smokeSource.Contains($requiredSignedProvisioningContract)) {
        throw "Windows ML proof is missing signed provisioning contract: $requiredSignedProvisioningContract"
    }
}
if ($smokeSource.Contains("Install-WindowsMlRuntime")) {
    throw "Windows ML proof must not directly extract or synthesize a signed runtime install"
}
if ($smokeSource.Contains('$fixtureDir = Join-Path $DataRoot "smoke-fixture"')) {
    throw "Windows semantic smoke must not place fixture input under ctx DataRoot"
}
foreach ($requiredFixtureRootContract in @(
    '$fixtureParent = Split-Path -Parent $DataRoot',
    '$fixtureParent = $dataRootParent',
    '$fixtureRoot = New-UniqueFixtureRoot -Parent $fixtureParent -DataRoot $DataRoot',
    '$ownsFixtureRoot = $true',
    '$fixturePath = Join-Path $fixtureRoot "history.jsonl"',
    'Remove-Item -LiteralPath $fixtureRoot -Recurse -Force -ErrorAction Stop'
)) {
    if (-not $smokeSource.Contains($requiredFixtureRootContract)) {
        throw "Windows semantic smoke is missing external fixture-root ownership: $requiredFixtureRootContract"
    }
}
foreach ($requiredTeardownContract in @(
    '$primaryError = $_',
    '-PrimaryError $primaryError',
    '-ErrorAction Continue'
)) {
    if (-not $smokeSource.Contains($requiredTeardownContract)) {
        throw "Windows semantic smoke is missing primary-error-preserving teardown: $requiredTeardownContract"
    }
}
$selectedBackendCheck = $smokeSource.IndexOf(
    '$embeddingRuntime.backend -cne $runtimeContract.StatusBackend',
    [System.StringComparison]::Ordinal
)
$archiveBindingCheck = $smokeSource.IndexOf(
    '"sha256=$actualRuntimeSha"',
    [System.StringComparison]::Ordinal
)
$canaryCheck = $smokeSource.IndexOf(
    '$embeddingRuntime.canary -cne "passed"',
    [System.StringComparison]::Ordinal
)
$exactLoadedRuntimeCheck = $smokeSource.IndexOf(
    '$actualOnnxRuntime.Equals($runtimeDylib',
    [System.StringComparison]::Ordinal
)
$exactWindowsMlModuleCheck = $smokeSource.IndexOf(
    '$actualModule.Equals($expectedModule',
    [System.StringComparison]::Ordinal
)
$directSuccess = $smokeSource.IndexOf(
    'Write-Host "ctx semantic smoke ok:',
    [System.StringComparison]::Ordinal
)
$directSuccessExit = $smokeSource.IndexOf(
    'exit 0',
    [System.StringComparison]::Ordinal
)
if (
    $selectedBackendCheck -lt 0 -or
    $canaryCheck -lt $selectedBackendCheck -or
    $archiveBindingCheck -lt $canaryCheck -or
    $exactLoadedRuntimeCheck -lt $archiveBindingCheck -or
    $exactWindowsMlModuleCheck -lt $exactLoadedRuntimeCheck -or
    $directSuccess -lt $exactWindowsMlModuleCheck -or
    $directSuccessExit -lt $directSuccess
) {
    throw "Windows ML smoke must directly pass selected-backend, exact-archive canary, and loaded-module checks before success"
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-windows-smoke-contract-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $primaryMessage = "synthetic primary lifecycle failure"
    $primaryRecord = [System.Management.Automation.ErrorRecord]::new(
        [System.TimeoutException]::new($primaryMessage),
        "CtxSemanticSmoke.Primary",
        [System.Management.Automation.ErrorCategory]::OperationTimeout,
        $null
    )
    $cleanupException = [System.InvalidOperationException]::new(
        "synthetic daemon teardown failure"
    )
    $script:dualFailure = $null
    $dualFailureOutput = @(
        & {
            try {
                Complete-SmokeTeardown `
                    -PrimaryError $primaryRecord `
                    -TeardownError $cleanupException `
                    -FixtureCleanupError $null `
                    -EnvironmentCleanupError $null `
                    -RunRoot "C:\synthetic-run" `
                    -FixtureRoot "C:\synthetic-fixture"
            } catch {
                $script:dualFailure = $_
            }
        } 2>&1
    )
    if (
        $null -eq $dualFailure -or
        $dualFailure.Exception.GetType() -ne [System.TimeoutException] -or
        $dualFailure.Exception.Message -cne $primaryMessage -or
        -not $dualFailure.FullyQualifiedErrorId.Contains("CtxSemanticSmoke.Primary")
    ) {
        throw "Cleanup failure replaced the primary semantic smoke failure"
    }
    if (-not ($dualFailureOutput | Out-String).Contains("cleanup also failed to stop the daemon")) {
        throw "Secondary cleanup failure was not reported non-terminatingly"
    }

    $script:cleanupOnlyFailure = $null
    & {
        try {
            Complete-SmokeTeardown `
                -PrimaryError $null `
                -TeardownError $cleanupException `
                -FixtureCleanupError $null `
                -EnvironmentCleanupError $null `
                -RunRoot "C:\synthetic-run" `
                -FixtureRoot "C:\synthetic-fixture"
        } catch {
            $script:cleanupOnlyFailure = $_
        }
    } 2>&1 | Out-Null
    if (
        $null -eq $cleanupOnlyFailure -or
        $cleanupOnlyFailure.Exception.GetType() -ne [System.InvalidOperationException] -or
        $cleanupOnlyFailure.Exception.Message -cne $cleanupException.Message
    ) {
        throw "Cleanup failure was not authoritative after an otherwise-successful smoke body"
    }

    $fixtureCases = @(
        [PSCustomObject]@{
            Name = "ordinary"
            Parent = Join-Path $root "short-root"
            DataRoot = Join-Path $root "short-root\run\data"
        },
        [PSCustomObject]@{
            Name = "signed-provisioned"
            Parent = Join-Path $root "signed"
            DataRoot = Join-Path $root "signed\provisioned-data"
        }
    )
    foreach ($fixtureCase in $fixtureCases) {
        New-Item -ItemType Directory -Path $fixtureCase.DataRoot -Force | Out-Null
        $ownedFixtureRoot = New-UniqueFixtureRoot `
            -Parent $fixtureCase.Parent `
            -DataRoot $fixtureCase.DataRoot
        try {
            $fixtureFullPath = [System.IO.Path]::GetFullPath($ownedFixtureRoot)
            $dataFullPath = [System.IO.Path]::GetFullPath($fixtureCase.DataRoot)
            $dataPrefix = $dataFullPath
            $directorySeparator = [string][System.IO.Path]::DirectorySeparatorChar
            if (-not $dataPrefix.EndsWith($directorySeparator, [System.StringComparison]::Ordinal)) {
                $dataPrefix += $directorySeparator
            }
            if (
                -not (Test-Path -LiteralPath $fixtureFullPath -PathType Container) -or
                -not (Split-Path -Parent $fixtureFullPath).Equals(
                    [System.IO.Path]::GetFullPath($fixtureCase.Parent),
                    [System.StringComparison]::OrdinalIgnoreCase
                ) -or
                $fixtureFullPath.Equals($dataFullPath, [System.StringComparison]::OrdinalIgnoreCase) -or
                $fixtureFullPath.StartsWith($dataPrefix, [System.StringComparison]::OrdinalIgnoreCase)
            ) {
                throw "$($fixtureCase.Name) fixture root overlaps ctx DataRoot or escaped its task parent"
            }
        } finally {
            Remove-Item -LiteralPath $ownedFixtureRoot -Recurse -Force -ErrorAction Stop
        }
        if (Test-Path -LiteralPath $ownedFixtureRoot) {
            throw "$($fixtureCase.Name) fixture root survived deterministic cleanup"
        }
    }

    $overlapRoot = Join-Path $root "overlap\data"
    New-Item -ItemType Directory -Path $overlapRoot -Force | Out-Null
    $overlapFailure = $null
    try {
        New-UniqueFixtureRoot -Parent $overlapRoot -DataRoot $overlapRoot | Out-Null
    } catch {
        $overlapFailure = $_.Exception.Message
    }
    if (
        [string]::IsNullOrEmpty($overlapFailure) -or
        -not $overlapFailure.Contains("Fixture root must be outside ctx DataRoot")
    ) {
        throw "Fixture-root helper accepted a parent inside ctx DataRoot"
    }

    $artifact = Join-Path $root "ctx.exe"
    [System.IO.File]::WriteAllText(
        $artifact,
        "synthetic Windows artifact`n",
        [System.Text.UTF8Encoding]::new($false)
    )
    $artifactSha = (Get-FileHash -Algorithm SHA256 -LiteralPath $artifact).Hash.ToLowerInvariant()
    $matrix = Join-Path $root "release-targets-v1.json"
    Copy-Item -LiteralPath (
        Join-Path (Split-Path -Parent $PSScriptRoot) "contracts\release-targets-v1.json"
    ) -Destination $matrix
    $buildInfoPath = "$artifact.build-info.json"
    $buildInfo = [ordered]@{
        artifact_sha256 = $artifactSha
        cargo_lock_sha256 = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
        gates = [ordered]@{
            local_runtime = "not_run"
            local_runtime_authority = "not_run"
            static = "passed"
            static_abi = "passed"
        }
        linux_build = $null
        platform = "windows-x64"
        schema_version = 1
        source = [ordered]@{
            clean = $true
            commit = "1111111111111111111111111111111111111111"
        }
        target = "x86_64-pc-windows-gnu"
    }
    [System.IO.File]::WriteAllText(
        $buildInfoPath,
        (($buildInfo | ConvertTo-Json -Depth 8 -Compress) + "`n"),
        [System.Text.UTF8Encoding]::new($false)
    )
    $expectedBuildInfoSha = (
        Get-FileHash -Algorithm SHA256 -LiteralPath $buildInfoPath
    ).Hash.ToLowerInvariant()
    $actualBuildInfoSha = Get-BoundWindowsBuildInfoSha256 `
        -ArtifactPath $artifact `
        -ExpectedArtifactSha256 $artifactSha `
        -BuildInfoPath $buildInfoPath `
        -MatrixPath $matrix
    if ($actualBuildInfoSha -cne $expectedBuildInfoSha) {
        throw "Windows build-info validator returned the wrong digest"
    }
    $buildInfo["source"]["clean"] = $false
    [System.IO.File]::WriteAllText(
        $buildInfoPath,
        (($buildInfo | ConvertTo-Json -Depth 8 -Compress) + "`n"),
        [System.Text.UTF8Encoding]::new($false)
    )
    $dirtyFailure = $null
    try {
        Get-BoundWindowsBuildInfoSha256 `
            -ArtifactPath $artifact `
            -ExpectedArtifactSha256 $artifactSha `
            -BuildInfoPath $buildInfoPath `
            -MatrixPath $matrix | Out-Null
    } catch {
        $dirtyFailure = $_.Exception.Message
    }
    if (
        [string]::IsNullOrEmpty($dirtyFailure) -or
        -not $dirtyFailure.Contains("does not bind the clean exact matrix artifact")
    ) {
        throw "Windows build-info validator accepted dirty provenance"
    }

    $script:DataRoot = Join-Path $root "data root"
    $fixturePath = Join-Path $root "fixture path.jsonl"
    $argumentLog = Join-Path $root "arguments.txt"
    $invocationLog = Join-Path $root "invocations.txt"
    $script:Ctx = Join-Path $root "fake-ctx.cmd"
    $env:CTX_SMOKE_ARGUMENT_LOG = $argumentLog
    $env:CTX_SMOKE_INVOCATION_LOG = $invocationLog
    [System.IO.File]::WriteAllText(
        $script:Ctx,
        "@echo off`r`necho invocation>>`"%CTX_SMOKE_INVOCATION_LOG%`"`r`ntype nul>`"%CTX_SMOKE_ARGUMENT_LOG%`"`r`n:args`r`nif `"%~1`"==`"`" goto done`r`n>>`"%CTX_SMOKE_ARGUMENT_LOG%`" echo(%~1`r`nshift`r`ngoto args`r`n:done`r`necho fake stdout`r`necho fake stderr 1>&2`r`nexit /b 23`r`n",
        [System.Text.UTF8Encoding]::new($false)
    )

    $failure = $null
    try {
        Invoke-CtxChecked -FailureLabel "fixture import" -CommandArgs @(
            "import", "--no-daemon", "--input-format", "ctx-history-jsonl-v2", "--path", $fixturePath
        ) | Out-Null
    } catch {
        $failure = $_.Exception.Message
    }
    if ([string]::IsNullOrEmpty($failure)) {
        throw "Invoke-CtxChecked accepted a failing ctx import"
    }
    foreach ($expected in @("fixture import", "status 23", "fake stdout", "fake stderr")) {
        if (-not $failure.Contains($expected)) {
            throw "Failure diagnostics omitted '$expected': $failure"
        }
    }

    $expectedArguments = @(
        "--data-root",
        $script:DataRoot,
        "import",
        "--no-daemon",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        $fixturePath
    )
    $arguments = @([System.IO.File]::ReadAllLines($argumentLog))
    if ($arguments.Count -ne $expectedArguments.Count) {
        throw "Forwarded ctx argument count was $($arguments.Count), expected $($expectedArguments.Count)"
    }
    for ($index = 0; $index -lt $expectedArguments.Count; $index++) {
        if ($arguments[$index] -cne $expectedArguments[$index]) {
            throw "Forwarded ctx argument $index was '$($arguments[$index])', expected '$($expectedArguments[$index])'"
        }
    }
    $invocations = @([System.IO.File]::ReadAllLines($invocationLog))
    if ($invocations.Count -ne 1 -or $invocations[0] -cne "invocation") {
        throw "Expected exactly one ctx invocation, got $($invocations.Count)"
    }
} finally {
    Remove-Item Env:CTX_SMOKE_ARGUMENT_LOG -ErrorAction SilentlyContinue
    Remove-Item Env:CTX_SMOKE_INVOCATION_LOG -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "Windows semantic smoke contract passed"
