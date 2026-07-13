param(
    [string]$RuntimeArchive = "target/public-cli-artifacts/ctx-onnxruntime-windows-x64.zip"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This contract test must run on Windows"
}

$archivePath = (Resolve-Path -LiteralPath $RuntimeArchive).Path
$installerSource = Join-Path $PSScriptRoot "..\crates\ctx-cli\src\upgrade\install.rs"
$source = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $installerSource).Path)
$pattern = 'const EXTRACT_SCRIPT: &str = r#"\r?\n(?<script>.*?)\r?\n"#;'
$matches = [regex]::Matches(
    $source,
    $pattern,
    [System.Text.RegularExpressions.RegexOptions]::Singleline
)
if ($matches.Count -ne 1) {
    throw "Expected exactly one embedded Windows runtime extraction script"
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-upgrade-extractor-" + [Guid]::NewGuid().ToString("n"))
$destination = Join-Path $root "runtime"
$extractor = Join-Path $root "extract.ps1"
New-Item -ItemType Directory -Path $destination -Force | Out-Null
try {
    [System.IO.File]::WriteAllText(
        $extractor,
        $matches[0].Groups["script"].Value,
        [System.Text.UTF8Encoding]::new($false)
    )
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $outputLines = @(
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $extractor `
                -ArchivePath $archivePath `
                -Destination $destination `
                -ExpectedVersion "1.27.0" `
                -MaxExpandedBytes 1073741824 2>&1
        )
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if ($exitCode -ne 0) {
        throw "Embedded Windows runtime extractor failed with status $exitCode`n$($outputLines -join [Environment]::NewLine)"
    }

    $expectedFiles = @(
        "GIT_COMMIT_ID",
        "LICENSE",
        "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
        "ThirdPartyNotices.txt",
        "VERSION_NUMBER",
        "lib/msvcp140.dll",
        "lib/msvcp140_1.dll",
        "lib/onnxruntime.dll",
        "lib/vcruntime140.dll",
        "lib/vcruntime140_1.dll"
    ) | Sort-Object
    $actualFiles = @(
        Get-ChildItem -LiteralPath $destination -Recurse -File | ForEach-Object {
            $_.FullName.Substring($destination.Length + 1).Replace("\", "/")
        } | Sort-Object
    )
    if ($actualFiles.Count -ne $expectedFiles.Count) {
        throw "Embedded extractor produced $($actualFiles.Count) files, expected $($expectedFiles.Count)"
    }
    for ($index = 0; $index -lt $expectedFiles.Count; $index++) {
        if ($actualFiles[$index] -cne $expectedFiles[$index]) {
            throw "Embedded extractor file $index was '$($actualFiles[$index])', expected '$($expectedFiles[$index])'"
        }
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            foreach ($relativePath in $expectedFiles) {
                $entry = $archive.GetEntry($relativePath)
                if ($null -eq $entry) {
                    throw "Runtime archive entry is missing: $relativePath"
                }
                $stream = $entry.Open()
                try {
                    $archiveHash = [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace("-", "")
                } finally {
                    $stream.Dispose()
                }
                $extractedPath = Join-Path $destination $relativePath.Replace("/", "\")
                $extractedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $extractedPath).Hash
                if ($archiveHash -cne $extractedHash) {
                    throw "Extracted runtime file does not match its archive entry: $relativePath"
                }
            }
        } finally {
            $sha256.Dispose()
        }
    } finally {
        $archive.Dispose()
    }
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "Windows runtime upgrade extractor contract passed"
