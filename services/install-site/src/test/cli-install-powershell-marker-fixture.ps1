param(
    [Parameter(Mandatory = $true)][string]$RendererPath,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [switch]$CoreOnly
)

$ErrorActionPreference = "Stop"
$body = [IO.File]::ReadAllText($RendererPath)
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput(
    $body, [ref]$tokens, [ref]$parseErrors
)
if ($parseErrors.Count -ne 0) {
    throw "complete hosted renderer failed PowerShell parsing"
}
$assignments = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.AssignmentStatementAst] -and
        $node.Left -is [Management.Automation.Language.VariableExpressionAst] -and
        $node.Left.VariablePath.UserPath -ceq "marker"
}, $true))
$writes = @($ast.FindAll({
    param($node)
    $node -is [Management.Automation.Language.InvokeMemberExpressionAst] -and
        $node.Extent.Text -ceq '[IO.File]::WriteAllBytes($markerSourcePath, $markerBytes)'
}, $true))
if ($assignments.Count -ne 1 -or $writes.Count -ne 1 -or
    $writes[0].Extent.StartOffset -le $assignments[0].Extent.EndOffset) {
    throw "expected one hosted marker construction and byte serialization"
}
$start = $assignments[0].Extent.StartOffset
$serialization = $body.Substring($start, $writes[0].Extent.EndOffset - $start)

# Execute the actual statements selected from the complete renderer. Fixture
# values are inputs; marker fields and JSON serialization stay production-owned.
$managedPair = -not $CoreOnly
$tempRoot = [IO.Path]::GetFullPath($OutputDirectory)
$null = [IO.Directory]::CreateDirectory($tempRoot)
$installPath = Join-Path $tempRoot "install bin/ctx.exe"
$installAttemptId = "ia_marker_contract_fixture"
$releaseChannel = "stable"
$version = "1.3.1"
$actualChecksum = "a" * 64
$Metadata = "https://example.invalid/metadata.env"
$artifactUrl = "https://example.invalid/ctx.exe"
$sourceCommit = "b" * 40
$publishedAt = "2026-09-04T00:00:00Z"
$null = & ([scriptblock]::Create($serialization))
$markerPath = Join-Path $tempRoot "ctx.install.json"
[ordered]@{
    renderer_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $RendererPath).Hash.ToLowerInvariant()
    marker_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $markerPath).Hash.ToLowerInvariant()
    marker_path = $markerPath
} | ConvertTo-Json -Compress
