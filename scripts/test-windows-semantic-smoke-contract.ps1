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

$requiredFunctions = @("Invoke-Ctx", "Invoke-CtxChecked")
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

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-windows-smoke-contract-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
try {
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
