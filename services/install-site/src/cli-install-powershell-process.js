import { CLI_INSTALL_POWERSHELL_NATIVE_PROCESS } from "./cli-install-powershell-native-process.js";

export const CLI_INSTALL_POWERSHELL_PROCESS_HELPERS = `${CLI_INSTALL_POWERSHELL_NATIVE_PROCESS}

function Write-BoundedCapturedChildError([string]$ErrorPath) {
    $maximumCharacters = 8KB
    $maximumLines = 40
    try {
        $captureRoot = [IO.Path]::GetFullPath($tempRoot).TrimEnd("\\", "/") +
            [IO.Path]::DirectorySeparatorChar
        $capturePath = [IO.Path]::GetFullPath($ErrorPath)
        if (-not $capturePath.StartsWith(
            $captureRoot,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -or [IO.Path]::GetFileName($capturePath) -cnotmatch
            '^ctx-command-[0-9a-f]{32}\\.err$') {
            return
        }
        $captureItem = Get-Item -LiteralPath $capturePath -Force
        if ($captureItem.PSIsContainer -or
            ($captureItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            $captureItem.Length -eq 0) {
            return
        }
        $stream = [IO.File]::Open(
            $capturePath,
            [IO.FileMode]::Open,
            [IO.FileAccess]::Read,
            [IO.FileShare]::Read
        )
        try {
            $reader = [IO.StreamReader]::new(
                $stream,
                [Text.UTF8Encoding]::new($false, $false),
                $true,
                4096,
                $true
            )
            try {
                $buffer = [char[]]::new($maximumCharacters)
                $count = $reader.ReadBlock($buffer, 0, $buffer.Length)
                $capturedOutput = [string]::new($buffer, 0, $count)
                $truncated = $reader.Peek() -ne -1
            } finally {
                $reader.Dispose()
            }
        } finally {
            $stream.Dispose()
        }
        $capturedOutput = [regex]::Replace(
            $capturedOutput,
            '[\\x00-\\x08\\x0B\\x0C\\x0E-\\x1F\\x7F]',
            '?'
        )
        $capturedOutput = [regex]::Replace(
            $capturedOutput,
            '(?im)(authorization|password|secret|token|credential|api[_-]?key)\\s*[:=]\\s*[^\\r\\n]*',
            '$1=<redacted>'
        )
        $capturedOutput = [regex]::Replace(
            $capturedOutput,
            '(?i)\\bBearer\\s+[^\\s]+',
            'Bearer <redacted>'
        )
        $capturedOutput = [regex]::Replace(
            $capturedOutput,
            '(?i)([?&](?:token|key|secret|signature|credential|code)=)[^&\\s]+',
            '$1<redacted>'
        )
        $capturedLines = @($capturedOutput -split '\\r?\\n')
        if ($capturedLines.Count -gt $maximumLines) {
            $capturedLines = @($capturedLines | Select-Object -First $maximumLines)
            $truncated = $true
        }
        $capturedOutput = ($capturedLines -join [Environment]::NewLine).TrimEnd(
            [char[]]@([char]13, [char]10)
        )
        if ([string]::IsNullOrWhiteSpace($capturedOutput)) {
            return
        }
        [Console]::Error.WriteLine("ctx child output:")
        [Console]::Error.WriteLine($capturedOutput)
        if ($truncated) {
            [Console]::Error.WriteLine("... ctx child output truncated ...")
        }
    } catch {
        return
    }
}

function Read-BoundedSuccessReceipt(
    [string]$Path,
    [long]$MaximumBytes,
    [string[]]$RequiredNames,
    [string[]]$OptionalNames,
    [switch]$AllowEmptyWarnings,
    [switch]$AllowPrettyJson
) {
    try {
        $item = Get-Item -LiteralPath $Path -Force
        if ($item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            $item.Length -lt 1 -or $item.Length -gt $MaximumBytes) {
            return $null
        }
        $receiptText = [IO.File]::ReadAllText($Path)
        if (-not $receiptText.EndsWith("\`n", [System.StringComparison]::Ordinal)) {
            return $null
        }
        $receiptBody = $receiptText.Substring(0, $receiptText.Length - 1)
        if ($receiptBody.EndsWith("\`r", [System.StringComparison]::Ordinal)) {
            $receiptBody = $receiptBody.Substring(0, $receiptBody.Length - 1)
        }
        if (-not $AllowPrettyJson -and ($receiptBody.Contains("\`r") -or $receiptBody.Contains("\`n"))) {
            return $null
        }
        $receipt = $receiptBody | ConvertFrom-Json
        if ($null -eq $receipt -or $receipt -isnot [pscustomobject]) {
            return $null
        }
        $properties = @($receipt.PSObject.Properties)
        $allowedNames = @($RequiredNames + $OptionalNames)
        if ($properties.Count -lt $RequiredNames.Count -or
            $properties.Count -gt $allowedNames.Count) {
            return $null
        }
        foreach ($property in $properties) {
            if (@($allowedNames | Where-Object { $_ -ceq $property.Name }).Count -ne 1) {
                return $null
            }
        }
        foreach ($name in $RequiredNames) {
            if (@($properties | Where-Object { $_.Name -ceq $name }).Count -ne 1) {
                return $null
            }
        }
        $warnings = @()
        $warningProperties = @($properties | Where-Object { $_.Name -ceq "warnings" })
        if ($warningProperties.Count -eq 1) {
            $warningValue = $warningProperties[0].Value
            if ($warningValue -isnot [System.Array]) {
                return $null
            }
            $warnings = @($warningValue)
            if (($warnings.Count -lt 1 -and -not $AllowEmptyWarnings) -or
                $warnings.Count -gt 4 -or
                @($warnings | Where-Object {
                    $_ -isnot [string] -or $_.Length -lt 1 -or $_.Length -gt 160 -or
                    $_ -cnotmatch '^[A-Za-z0-9 .,:;()/_-]+$'
                }).Count -ne 0) {
                return $null
            }
        }
        return [pscustomobject]@{
            Receipt = $receipt
            Warnings = [object[]]$warnings
        }
    } catch {
        return $null
    }
}

function Write-SuccessReceiptWarnings([object[]]$Warnings) {
    foreach ($warning in $Warnings) {
        [Console]::Error.WriteLine("warning: $warning")
    }
}

function Invoke-CtxCaptured(
    [string[]]$Arguments,
    [switch]$InheritStandardError
) {
    return Invoke-ExecutableCaptured -Executable $installPath -Arguments $Arguments -InheritStandardError:$InheritStandardError
}

function Invoke-HostedInstallerSetupCtxCaptured(
    [string[]]$Arguments,
    [switch]$InheritStandardError
) {
    $previousHostedInstallerSetup = [Environment]::GetEnvironmentVariable(
        "CTX_HOSTED_INSTALLER_SETUP",
        "Process"
    )
    try {
        [Environment]::SetEnvironmentVariable(
            "CTX_HOSTED_INSTALLER_SETUP",
            "1",
            "Process"
        )
        return Invoke-CtxCaptured -Arguments $Arguments -InheritStandardError:$InheritStandardError
    } finally {
        [Environment]::SetEnvironmentVariable(
            "CTX_HOSTED_INSTALLER_SETUP",
            $previousHostedInstallerSetup,
            "Process"
        )
    }
}

function Invoke-CtxQuiet([string[]]$Arguments) {
    $commandResult = Invoke-CtxCaptured -Arguments $Arguments
    return $commandResult.ExitCode
}
`;
