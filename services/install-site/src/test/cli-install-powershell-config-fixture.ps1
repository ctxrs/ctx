param(
    [Parameter(Mandatory = $true)][string]$RendererPath,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    $RendererPath, [ref]$tokens, [ref]$parseErrors
)
if ($parseErrors.Count -ne 0) { throw 'complete hosted renderer failed PowerShell parsing' }
# Execute the production parser without download, installation or setup side effects.
foreach ($name in @('Fail', 'Remove-ConfigComment', 'Test-ConfigQuotedString',
    'Get-ConfigStringValue', 'Assert-PersistedConfigValue', 'Get-PersistedConfigControls')) {
    $functions = @($ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
    }, $true))
    if ($functions.Count -ne 1) { throw "expected one production function $name" }
    . ([scriptblock]::Create($functions[0].Extent.Text))
}
$previousRoot = $env:CTX_DATA_ROOT
$env:CTX_DATA_ROOT = [IO.Path]::GetFullPath($OutputDirectory)
$null = [IO.Directory]::CreateDirectory($env:CTX_DATA_ROOT)
$configPath = Join-Path $env:CTX_DATA_ROOT 'config.toml'
$providerPath = Join-Path ([IO.Path]::GetDirectoryName($env:CTX_DATA_ROOT)) 'provider #1'
$utf8 = [Text.UTF8Encoding]::new($false)
$cases = @(
    @{ name = 'default'; config = ''; semantic = $false; disabled = $false },
    @{ name = 'reported builtin throttling'; config = "[daemon]`n[search]`nsemantic = true`n[indexing]`nmode = 'auto'`n[semantic]`nbuiltin_throttling = false`n"; semantic = $true; disabled = $false },
    @{ name = 'throttling does not opt in'; config = "[semantic]`nbuiltin_throttling = true`n"; semantic = $false; disabled = $false },
    @{ name = 'external executor'; config = "[search]`nsemantic = true`n[semantic]`nexecutor = 'http://127.0.0.1:8080'`nspace_id = 'my-space'`ndimensions = 768`n"; semantic = $true; disabled = $false },
    @{ name = 'named source root'; config = "[sources]`nautomatic = false`n[sources.roots.work]`nprovider = 'codex'`npath = '$providerPath'`ngroup = 'work'`n[indexing]`nmode = 'manual'`n"; semantic = $false; disabled = $true },
    @{ name = 'dotted keys and canonical precedence'; config = "daemon.enabled = false`nindexing.mode = 'automatic'`nsemantic.builtin_throttling = false`nsearch.semantic = true`n"; semantic = $true; disabled = $false },
    @{ name = 'invalid semantic'; config = "[search]`nsemantic = maybe`n"; error = 'must be a boolean' },
    @{ name = 'invalid daemon'; config = "[daemon]`nenabled = FALSE`n"; error = 'must be a boolean' },
    @{ name = 'invalid indexing'; config = "[indexing]`nmode = 'on-demand'`n"; error = 'must be either auto or manual' },
    @{ name = 'unquoted indexing'; config = "[indexing]`nmode = manual`n"; error = 'must be a quoted string' },
    @{ name = 'duplicate control'; config = "search.semantic = true`n[search]`nsemantic = false`n"; error = 'duplicate config key' }
)
try {
    foreach ($case in $cases) {
        [IO.File]::WriteAllText($configPath, $case.config, $utf8)
        $failure = $null
        try { $controls = Get-PersistedConfigControls } catch { $failure = $_.Exception.Message }
        if ($case.ContainsKey('error')) {
            if ($null -eq $failure -or -not $failure.Contains($case.error)) {
                throw "$($case.name): expected $($case.error), got $failure"
            }
        } elseif ($null -ne $failure) {
            throw "$($case.name): $failure"
        } elseif ($controls.SemanticEnabled -ne $case.semantic -or $controls.DaemonDisabled -ne $case.disabled) {
            throw "$($case.name): incorrect installer controls"
        }
        if ([IO.File]::ReadAllText($configPath) -cne $case.config) { throw "$($case.name): config changed" }
    }
    [ordered]@{ cases = $cases.Count; powershell = $PSVersionTable.PSVersion.ToString() } | ConvertTo-Json -Compress
} finally {
    $env:CTX_DATA_ROOT = $previousRoot
}
