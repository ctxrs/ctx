param([string]$FunctionsPath, [string]$WorkRoot)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Fail([string]$Message) { throw $Message }
. $FunctionsPath
$results = @()
foreach ($size in @(17, 65536, 65537, 2097152)) {
    $inputStream = [IO.MemoryStream]::new([byte[]]::new($size))
    $outputStream = [IO.MemoryStream]::new()
    $failed = $false
    try {
        try { Copy-LimitedStream $inputStream $outputStream 65536 }
        catch { $failed = $true; if ($_.Exception.Message -notmatch 'exceeds 65536 bytes') { throw } }
        if ($failed -ne ($size -gt 65536)) { throw "wrong stream result: $size" }
        if ($outputStream.Length -gt 65536) { throw 'write exceeded cap' }
        if (-not $failed -and $outputStream.Length -ne $size) { throw 'lost valid bytes' }
        if ($inputStream.Position -ne [Math]::Min($size, 65537)) { throw 'read exceeded remaining+1' }
        $results += @{ input=$size; read=$inputStream.Position; written=$outputStream.Length; refused=$failed }
    } finally { $inputStream.Dispose(); $outputStream.Dispose() }
}
foreach ($name in @('exact', 'bomb', 'corrupt')) {
    $output = Join-Path $WorkRoot "$name.raw"
    $failed = $false
    try { Expand-GzipFile (Join-Path $WorkRoot "$name.gz") $output -MaxBytes 65536 }
    catch { $failed = $true }
    if ($failed -ne ($name -ne 'exact')) { throw "wrong gzip result: $name" }
    if ((Get-Item -LiteralPath $output).Length -gt 65536) { throw 'gzip output exceeded cap' }
    # Exclusive reopens prove both file handles have been released on failure.
    $inputCheck = [IO.File]::Open((Join-Path $WorkRoot "$name.gz"), 'Open', 'ReadWrite', 'None')
    $inputCheck.Dispose()
    $outputCheck = [IO.File]::Open($output, 'Open', 'ReadWrite', 'None')
    $outputCheck.Dispose()
    $results += @{ gzip=$name; written=(Get-Item -LiteralPath $output).Length; refused=$failed }
}
$signature = Join-Path $WorkRoot 'large.sig'
[IO.File]::WriteAllBytes($signature, [byte[]]::new(65537))
$destination = Join-Path $WorkRoot 'signature-copy'
$failed = $false
try { Read-DetachedSignature $signature $destination } catch { $failed = $true }
if (-not $failed -or (Test-Path -LiteralPath $destination)) { throw 'oversized local signature was copied' }
$check = [IO.File]::Open($signature, 'Open', 'ReadWrite', 'None'); $check.Dispose()
$results += @{ local_signature_refused=$true; created=$false }
[ordered]@{ powershell=$PSVersionTable.PSVersion.ToString(); cases=$results } | ConvertTo-Json -Depth 4 -Compress
