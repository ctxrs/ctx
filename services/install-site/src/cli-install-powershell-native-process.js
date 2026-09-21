// Start-Process owns native file redirection, not PowerShell's error stream.
// Deliberately omit -Wait: it waits for descendants on Windows, including a
// daemon that a successful foreground command legitimately starts.
export const CLI_INSTALL_POWERSHELL_NATIVE_PROCESS = String.raw`function ConvertTo-NativeProcessArgument([string]$Argument) {
    if ($Argument.IndexOf([char]0) -ge 0) {
        throw "native command argument contains a null character"
    }
    $quoted = [Text.StringBuilder]::new()
    [void]$quoted.Append('"')
    $backslashes = 0
    foreach ($character in $Argument.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes++
            continue
        }
        if ($character -eq '"') {
            [void]$quoted.Append([char]'\', 2 * $backslashes + 1)
        } else {
            [void]$quoted.Append([char]'\', $backslashes)
        }
        [void]$quoted.Append($character)
        $backslashes = 0
    }
    [void]$quoted.Append([char]'\', 2 * $backslashes)
    [void]$quoted.Append('"')
    return $quoted.ToString()
}

function Invoke-ExecutableCaptured(
    [string]$Executable,
    [string[]]$Arguments,
    [switch]$InheritStandardError
) {
    $captureName = "ctx-command-" + [Guid]::NewGuid().ToString("n")
    $commandOutputPath = Join-Path $tempRoot ($captureName + ".out")
    $commandErrorPath = Join-Path $tempRoot ($captureName + ".err")
    $utf8 = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($commandOutputPath, "", $utf8)
    [IO.File]::WriteAllText($commandErrorPath, "", $utf8)
    $commandExitCode = 1
    $started = $false
    $processError = $null
    $process = $null
    try {
        $startArguments = @{
            FilePath = $Executable
            NoNewWindow = $true
            PassThru = $true
            RedirectStandardOutput = $commandOutputPath
            ErrorAction = "Stop"
        }
        if ($null -ne $Arguments -and $Arguments.Count -gt 0) {
            $startArguments.ArgumentList = (@($Arguments | ForEach-Object {
                ConvertTo-NativeProcessArgument -Argument $_
            }) -join " ")
        }
        if (-not $InheritStandardError) {
            $startArguments.RedirectStandardError = $commandErrorPath
        }
        $process = Start-Process @startArguments
        $started = $true
        # Retain the process handle before waiting so its exit status remains
        # available even when a short-lived child has already exited.
        $null = $process.Handle
        $process.WaitForExit()
        $process.Refresh()
        if ($null -eq $process.ExitCode) {
            throw "native command did not provide an exit status"
        }
        $commandExitCode = [int]$process.ExitCode
    } catch {
        $processError = $_.Exception.GetBaseException().Message
        [IO.File]::AppendAllText($commandErrorPath, $processError + [Environment]::NewLine, $utf8)
    } finally {
        if ($null -ne $process) { $process.Dispose() }
    }
    return [pscustomobject]@{
        ExitCode = $commandExitCode
        OutputPath = $commandOutputPath
        ErrorPath = $commandErrorPath
        Started = $started
        ProcessError = $processError
    }
}
`;
