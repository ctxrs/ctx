export const CLI_INSTALL_POWERSHELL_PATH_SETUP = `function Test-BareCtxResolvesToInstall([string]$InstallPath) {
    try {
        $command = @(Get-Command -Name "ctx" -ErrorAction Stop)[0]
    } catch {
        return $false
    }
    if ($command.CommandType -cne "Application") {
        return $false
    }
    return (Normalize-PathEntry $command.Path).Equals(
        (Normalize-PathEntry $InstallPath),
        [System.StringComparison]::OrdinalIgnoreCase
    )
}

function Prepend-PathDirectory([string]$PathValue, [string]$Directory) {
    $dir = Normalize-PathEntry $Directory
    $entries = @($dir)
    foreach ($entry in ($PathValue -split [regex]::Escape([System.IO.Path]::PathSeparator))) {
        $normalized = Normalize-PathEntry $entry
        if (-not [string]::IsNullOrWhiteSpace($normalized) -and
            -not $normalized.Equals($dir, [System.StringComparison]::OrdinalIgnoreCase)) {
            $entries += $entry
        }
    }
    return $entries -join [System.IO.Path]::PathSeparator
}

function Configure-InstallPath([string]$InstallPath, [bool]$ModifyPath) {
    $dir = (Split-Path -Parent $InstallPath).TrimEnd("\\", "/")
    $script:pathResult = ""
    if (Test-BareCtxResolvesToInstall -InstallPath $InstallPath) {
        return
    }
    $script:pathResult = 'To use the newly installed ctx in this shell, run:' + [Environment]::NewLine +
        '  $env:Path = "' + $dir + [System.IO.Path]::PathSeparator + '$env:Path"'

    if (-not $ModifyPath) {
        return
    }

    if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_PATH)) {
        Add-Content -LiteralPath $env:GITHUB_PATH -Value $dir
        $env:Path = Prepend-PathDirectory -PathValue $env:Path -Directory $dir
        return
    }

    if ($env:CI -eq "1" -or $env:CI -eq "true") {
        $env:Path = Prepend-PathDirectory -PathValue $env:Path -Directory $dir
        return
    }

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $newUserPath = Prepend-PathDirectory -PathValue $userPath -Directory $dir
    try {
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    } catch {
    }

    $env:Path = Prepend-PathDirectory -PathValue $env:Path -Directory $dir
}
`;
