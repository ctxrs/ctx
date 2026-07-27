Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This contract test must run on Windows"
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
    '$runtimeMarker.metadata_trust -cne "signed-release-metadata"',
    '"signed-hosted-provisioning"'
)) {
    if (-not $smokeSource.Contains($requiredSignedProvisioningContract)) {
        throw "Windows ML proof is missing signed provisioning contract: $requiredSignedProvisioningContract"
    }
}
if ($smokeSource.Contains("Install-WindowsMlRuntime")) {
    throw "Windows ML proof must not directly extract or synthesize a signed runtime install"
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
$passedCanaryProof = $smokeSource.IndexOf(
    '$runtimeProofLines += "semantic_contract_canary=passed"',
    [System.StringComparison]::Ordinal
)
$proofWrite = $smokeSource.IndexOf(
    '[System.IO.File]::WriteAllText(',
    $passedCanaryProof,
    [System.StringComparison]::Ordinal
)
if (
    $selectedBackendCheck -lt 0 -or
    $archiveBindingCheck -lt $selectedBackendCheck -or
    $canaryCheck -lt $archiveBindingCheck -or
    $passedCanaryProof -lt $canaryCheck -or
    $proofWrite -lt $passedCanaryProof
) {
    throw "Windows ML proof must follow selected-backend, exact-archive, and passed-canary checks"
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-windows-smoke-contract-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
try {
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
            "import", "--no-daemon", "--format", "ctx-history-jsonl-v1", "--path", $fixturePath
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
        "--format",
        "ctx-history-jsonl-v1",
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
