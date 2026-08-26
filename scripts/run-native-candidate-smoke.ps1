param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,
    [string]$Companion = "",
    [string]$PairEnvelope = "",
    [Parameter(Mandatory = $true)]
    [string]$Fixture,
    [string]$ExpectedVersion,
    [string]$ExpectedVersionFile,
    [Parameter(Mandatory = $true)]
    [string]$ResultPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ProcessStartInfo cannot establish a Job Object before the child executes.
# Start suspended so every descendant is born inside this invocation's job.
if ($null -eq ("CtxNativeOwnedProcess" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Collections.Specialized;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

public sealed class CtxNativeOwnedProcess : IDisposable
{
    private const uint CREATE_SUSPENDED = 0x00000004;
    private const uint CREATE_NO_WINDOW = 0x08000000;
    private const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
    private const uint STARTF_USESTDHANDLES = 0x00000100;
    private const uint HANDLE_FLAG_INHERIT = 0x00000001;
    private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;
    private const int JobObjectExtendedLimitInformation = 9;
    private const uint WAIT_OBJECT_0 = 0x00000000;
    private const uint WAIT_TIMEOUT = 0x00000102;
    private const uint WAIT_FAILED = 0xffffffff;
    private const uint STD_INPUT_HANDLE = unchecked((uint)-10);

    [StructLayout(LayoutKind.Sequential)]
    private struct SECURITY_ATTRIBUTES
    {
        public uint nLength;
        public IntPtr lpSecurityDescriptor;
        [MarshalAs(UnmanagedType.Bool)]
        public bool bInheritHandle;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct STARTUPINFO
    {
        public uint cb;
        public string lpReserved;
        public string lpDesktop;
        public string lpTitle;
        public uint dwX;
        public uint dwY;
        public uint dwXSize;
        public uint dwYSize;
        public uint dwXCountChars;
        public uint dwYCountChars;
        public uint dwFillAttribute;
        public uint dwFlags;
        public ushort wShowWindow;
        public ushort cbReserved2;
        public IntPtr lpReserved2;
        public IntPtr hStdInput;
        public IntPtr hStdOutput;
        public IntPtr hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_INFORMATION
    {
        public IntPtr hProcess;
        public IntPtr hThread;
        public uint dwProcessId;
        public uint dwThreadId;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_BASIC_LIMIT_INFORMATION
    {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize;
        public UIntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct IO_COUNTERS
    {
        public ulong ReadOperationCount;
        public ulong WriteOperationCount;
        public ulong OtherOperationCount;
        public ulong ReadTransferCount;
        public ulong WriteTransferCount;
        public ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    {
        public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
        public IO_COUNTERS IoInfo;
        public UIntPtr ProcessMemoryLimit;
        public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed;
        public UIntPtr PeakJobMemoryUsed;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr CreateJobObject(IntPtr jobAttributes, string name);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetInformationJobObject(
        IntPtr job,
        int informationClass,
        ref JOBOBJECT_EXTENDED_LIMIT_INFORMATION information,
        uint informationLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool TerminateJobObject(IntPtr job, uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CreatePipe(
        out IntPtr readPipe,
        out IntPtr writePipe,
        ref SECURITY_ATTRIBUTES pipeAttributes,
        uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CreateProcessW(
        string applicationName,
        StringBuilder commandLine,
        IntPtr processAttributes,
        IntPtr threadAttributes,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandles,
        uint creationFlags,
        IntPtr environment,
        string currentDirectory,
        ref STARTUPINFO startupInfo,
        out PROCESS_INFORMATION processInformation);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint ResumeThread(IntPtr thread);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool TerminateProcess(IntPtr process, uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr GetStdHandle(uint standardHandle);

    private IntPtr job;
    private IntPtr process;

    private CtxNativeOwnedProcess(
        IntPtr jobHandle,
        IntPtr processHandle,
        uint processId,
        StreamReader standardOutput,
        StreamReader standardError)
    {
        job = jobHandle;
        process = processHandle;
        Id = processId;
        StandardOutput = standardOutput;
        StandardError = standardError;
    }

    public uint Id { get; private set; }
    public StreamReader StandardOutput { get; private set; }
    public StreamReader StandardError { get; private set; }

    public bool HasExited
    {
        get { return WaitForExit(0); }
    }

    public int ExitCode
    {
        get
        {
            uint exitCode;
            if (!GetExitCodeProcess(process, out exitCode))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            return unchecked((int)exitCode);
        }
    }

    public static CtxNativeOwnedProcess Start(
        string applicationName,
        string commandLine,
        string currentDirectory,
        StringDictionary environment)
    {
        IntPtr jobHandle = IntPtr.Zero;
        IntPtr stdoutRead = IntPtr.Zero;
        IntPtr stdoutWrite = IntPtr.Zero;
        IntPtr stderrRead = IntPtr.Zero;
        IntPtr stderrWrite = IntPtr.Zero;
        IntPtr environmentBlock = IntPtr.Zero;
        PROCESS_INFORMATION processInfo = new PROCESS_INFORMATION();
        StreamReader stdout = null;
        StreamReader stderr = null;
        bool assigned = false;

        try
        {
            jobHandle = CreateJobObject(IntPtr.Zero, null);
            if (jobHandle == IntPtr.Zero)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits =
                new JOBOBJECT_EXTENDED_LIMIT_INFORMATION();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if (!SetInformationJobObject(
                jobHandle,
                JobObjectExtendedLimitInformation,
                ref limits,
                (uint)Marshal.SizeOf(typeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION))))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }

            SECURITY_ATTRIBUTES pipeAttributes = new SECURITY_ATTRIBUTES();
            pipeAttributes.nLength = (uint)Marshal.SizeOf(typeof(SECURITY_ATTRIBUTES));
            pipeAttributes.bInheritHandle = true;
            if (!CreatePipe(out stdoutRead, out stdoutWrite, ref pipeAttributes, 0) ||
                !CreatePipe(out stderrRead, out stderrWrite, ref pipeAttributes, 0))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            if (!SetHandleInformation(stdoutRead, HANDLE_FLAG_INHERIT, 0) ||
                !SetHandleInformation(stderrRead, HANDLE_FLAG_INHERIT, 0))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }

            STARTUPINFO startupInfo = new STARTUPINFO();
            startupInfo.cb = (uint)Marshal.SizeOf(typeof(STARTUPINFO));
            startupInfo.dwFlags = STARTF_USESTDHANDLES;
            startupInfo.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
            startupInfo.hStdOutput = stdoutWrite;
            startupInfo.hStdError = stderrWrite;
            environmentBlock = BuildEnvironmentBlock(environment);

            if (!CreateProcessW(
                applicationName,
                new StringBuilder(commandLine),
                IntPtr.Zero,
                IntPtr.Zero,
                true,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                environmentBlock,
                currentDirectory,
                ref startupInfo,
                out processInfo))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            CloseOwnedHandle(ref stdoutWrite);
            CloseOwnedHandle(ref stderrWrite);

            if (!AssignProcessToJobObject(jobHandle, processInfo.hProcess))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            assigned = true;

            stdout = OpenReader(ref stdoutRead);
            stderr = OpenReader(ref stderrRead);
            if (ResumeThread(processInfo.hThread) == UInt32.MaxValue)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            CloseOwnedHandle(ref processInfo.hThread);

            CtxNativeOwnedProcess owned = new CtxNativeOwnedProcess(
                jobHandle,
                processInfo.hProcess,
                processInfo.dwProcessId,
                stdout,
                stderr);
            jobHandle = IntPtr.Zero;
            processInfo.hProcess = IntPtr.Zero;
            stdout = null;
            stderr = null;
            return owned;
        }
        catch
        {
            if (processInfo.hProcess != IntPtr.Zero)
            {
                if (assigned)
                {
                    TerminateJobObject(jobHandle, 1);
                }
                else
                {
                    TerminateProcess(processInfo.hProcess, 1);
                }
            }
            throw;
        }
        finally
        {
            if (stdout != null)
            {
                stdout.Dispose();
            }
            if (stderr != null)
            {
                stderr.Dispose();
            }
            CloseOwnedHandle(ref processInfo.hThread);
            CloseOwnedHandle(ref processInfo.hProcess);
            CloseOwnedHandle(ref stdoutRead);
            CloseOwnedHandle(ref stdoutWrite);
            CloseOwnedHandle(ref stderrRead);
            CloseOwnedHandle(ref stderrWrite);
            CloseOwnedHandle(ref jobHandle);
            if (environmentBlock != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(environmentBlock);
            }
        }
    }

    public bool WaitForExit(int milliseconds)
    {
        uint result = WaitForSingleObject(process, unchecked((uint)milliseconds));
        if (result == WAIT_OBJECT_0)
        {
            return true;
        }
        if (result == WAIT_TIMEOUT)
        {
            return false;
        }
        if (result == WAIT_FAILED)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        throw new Win32Exception("unexpected process wait result: " + result);
    }

    public void Terminate()
    {
        if (!TerminateJobObject(job, 1))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error());
        }
    }

    public void Dispose()
    {
        CloseOwnedHandle(ref job);
        if (StandardOutput != null)
        {
            StandardOutput.Dispose();
            StandardOutput = null;
        }
        if (StandardError != null)
        {
            StandardError.Dispose();
            StandardError = null;
        }
        CloseOwnedHandle(ref process);
    }

    private static StreamReader OpenReader(ref IntPtr readHandle)
    {
        SafeFileHandle safeHandle = new SafeFileHandle(readHandle, true);
        readHandle = IntPtr.Zero;
        FileStream stream = new FileStream(safeHandle, FileAccess.Read, 4096, false);
        return new StreamReader(stream, Console.OutputEncoding, true, 4096);
    }

    private static IntPtr BuildEnvironmentBlock(StringDictionary environment)
    {
        List<string> keys = new List<string>();
        foreach (string key in environment.Keys)
        {
            keys.Add(key);
        }
        keys.Sort(StringComparer.OrdinalIgnoreCase);

        StringBuilder block = new StringBuilder();
        foreach (string key in keys)
        {
            block.Append(key);
            block.Append('=');
            block.Append(environment[key]);
            block.Append('\0');
        }
        block.Append('\0');
        return Marshal.StringToHGlobalUni(block.ToString());
    }

    private static void CloseOwnedHandle(ref IntPtr handle)
    {
        if (handle != IntPtr.Zero && handle != new IntPtr(-1))
        {
            CloseHandle(handle);
        }
        handle = IntPtr.Zero;
    }
}
"@
}

function Fail([string]$Message) {
    throw "native candidate smoke: $Message"
}

function ConvertTo-NativeArgument([string]$Value) {
    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
        return $Value
    }

    $quoted = New-Object System.Text.StringBuilder
    [void]$quoted.Append('"')
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes++
            continue
        }
        if ($character -eq '"') {
            [void]$quoted.Append(('\' * (($backslashes * 2) + 1)))
            [void]$quoted.Append('"')
        } else {
            [void]$quoted.Append(('\' * $backslashes))
            [void]$quoted.Append($character)
        }
        $backslashes = 0
    }
    [void]$quoted.Append(('\' * ($backslashes * 2)))
    [void]$quoted.Append('"')
    return $quoted.ToString()
}

$Binary = [System.IO.Path]::GetFullPath($Binary)
$pairMode = -not [string]::IsNullOrWhiteSpace($Companion) -or
    -not [string]::IsNullOrWhiteSpace($PairEnvelope)
if ($pairMode -and (
    [string]::IsNullOrWhiteSpace($Companion) -or
    [string]::IsNullOrWhiteSpace($PairEnvelope)
)) {
    Fail "Companion and PairEnvelope must be provided together"
}
if ($pairMode) {
    $Companion = [System.IO.Path]::GetFullPath($Companion)
    $PairEnvelope = [System.IO.Path]::GetFullPath($PairEnvelope)
}
$Fixture = [System.IO.Path]::GetFullPath($Fixture)
$ResultPath = [System.IO.Path]::GetFullPath($ResultPath)

if ([string]::IsNullOrWhiteSpace($ExpectedVersion) -eq
    [string]::IsNullOrWhiteSpace($ExpectedVersionFile)) {
    Fail "provide exactly one of ExpectedVersion or ExpectedVersionFile"
}
if (-not [string]::IsNullOrWhiteSpace($ExpectedVersionFile)) {
    $ExpectedVersionFile = [System.IO.Path]::GetFullPath($ExpectedVersionFile)
    if (-not (Test-Path -LiteralPath $ExpectedVersionFile -PathType Leaf)) {
        Fail "expected-version file is missing: $ExpectedVersionFile"
    }
    $ExpectedVersion = (Get-Content -LiteralPath $ExpectedVersionFile -Raw).Trim()
}

if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    Fail "binary is missing: $Binary"
}
if (-not (Test-Path -LiteralPath $Fixture -PathType Leaf)) {
    Fail "fixture is missing: $Fixture"
}
if ($pairMode -and -not (Test-Path -LiteralPath $Companion -PathType Leaf)) {
    Fail "companion is missing: $Companion"
}
if ($pairMode -and -not (Test-Path -LiteralPath $PairEnvelope -PathType Leaf)) {
    Fail "signed pair envelope is missing: $PairEnvelope"
}
if ($ExpectedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$') {
    Fail "expected version is invalid: $ExpectedVersion"
}
$versionParts = (($ExpectedVersion -split '[+-]', 2)[0]).Split(".")
$freshEpochRequired = [int]$versionParts[0] -gt 0 -or [int]$versionParts[1] -ge 26

$resultParent = Split-Path -Parent $ResultPath
if ([string]::IsNullOrWhiteSpace($resultParent)) {
    $resultParent = (Get-Location).Path
}
New-Item -ItemType Directory -Path $resultParent -Force | Out-Null
Remove-Item -LiteralPath $ResultPath -Force -ErrorAction SilentlyContinue
$resultTemp = "$ResultPath.tmp.$PID"

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-native-candidate-smoke-" + [Guid]::NewGuid().ToString("n"))
$profile = Join-Path $root "profile"
$dataRoot = Join-Path $root "data"
$configRoot = Join-Path $root "config"
$cacheRoot = Join-Path $root "cache"
$stateRoot = Join-Path $root "state"
$tmpRoot = Join-Path $root "tmp"
$workRoot = Join-Path $root "work"
foreach ($path in @($profile, $dataRoot, $configRoot, $cacheRoot, $stateRoot, $tmpRoot, $workRoot)) {
    New-Item -ItemType Directory -Path $path -Force | Out-Null
}
if ($pairMode) {
    $helper = Join-Path $PSScriptRoot "install-managed-pair.py"
    $python = Get-Command python3 -ErrorAction SilentlyContinue
    if ($null -eq $python) {
        $python = Get-Command python -ErrorAction SilentlyContinue
    }
    if ($null -eq $python -or -not (Test-Path -LiteralPath $helper -PathType Leaf)) {
        Fail "Python 3 and install-managed-pair.py are required for signed-pair qualification"
    }
    $installRoot = Join-Path $root "installation"
    & $python.Source -I $helper install `
        --envelope $PairEnvelope --core $Binary --companion $Companion `
        --install-root $installRoot --target windows-x64 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Fail "signed managed-pair installation failed"
    }
    $Binary = Join-Path $installRoot "bin\ctx.exe"
}

$savedLocation = (Get-Location).Path
$savedEnvironment = @{}
$timeoutText = if ([string]::IsNullOrWhiteSpace($env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS)) {
    "60"
} else {
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
}
$timeoutSeconds = 0
if (-not [int]::TryParse($timeoutText, [ref]$timeoutSeconds) -or
    $timeoutSeconds -lt 1 -or $timeoutSeconds -gt 900) {
    Fail "timeout must be a whole number of seconds between 1 and 900"
}
$isolation = [ordered]@{
    HOME = $profile
    USERPROFILE = $profile
    APPDATA = $configRoot
    LOCALAPPDATA = $dataRoot
    XDG_CONFIG_HOME = $configRoot
    XDG_CACHE_HOME = $cacheRoot
    XDG_DATA_HOME = (Join-Path $root "xdg-data")
    XDG_STATE_HOME = $stateRoot
    TEMP = $tmpRoot
    TMP = $tmpRoot
    CTX_DATA_ROOT = $dataRoot
    CTX_ANALYTICS_ENABLED = "false"
    CTX_UPGRADE_AUTO = "off"
    CTX_DAEMON_AUTOSTART_OFF = "1"
    CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS = "1"
    CTX_DAEMON_ENABLED = "false"
    CTX_SEARCH_SEMANTIC = "0"
    CTX_SEMANTIC_CACHE_DIR = (Join-Path $root "semantic-cache")
    HF_HOME = (Join-Path $root "huggingface")
    HF_HUB_OFFLINE = "1"
    TRANSFORMERS_OFFLINE = "1"
    CODEX_HOME = (Join-Path $profile ".codex")
    CLAUDE_CONFIG_DIR = (Join-Path $profile ".claude")
    COPILOT_HOME = (Join-Path $profile ".copilot")
    OPENCLAW_STATE_DIR = (Join-Path $profile ".openclaw")
    HERMES_HOME = (Join-Path $profile ".hermes")
    ASTRBOT_ROOT = (Join-Path $profile ".astrbot")
    SHELLEY_DB = (Join-Path $profile "shelley.db")
    KILO_DB = (Join-Path $profile "kilo.db")
    MIMOCODE_HOME = (Join-Path $profile ".mimocode")
    MIMOCODE_CONFIG_DIR = (Join-Path $profile ".mimocode-config")
    MIMOCODE_DB = (Join-Path $profile "mimocode.db")
    MIMOCODE_DISABLE_CHANNEL_DB = "1"
    FORGE_CONFIG = (Join-Path $profile "forge.json")
    VIBE_HOME = (Join-Path $profile ".vibe")
}

function Get-RemainingMilliseconds(
    [System.Diagnostics.Stopwatch]$Clock,
    [int]$LimitMilliseconds
) {
    $remaining = [long]$LimitMilliseconds - $Clock.ElapsedMilliseconds
    if ($remaining -le 0) {
        return 0
    }
    return [int]$remaining
}

function Wait-ProcessUntil(
    [CtxNativeOwnedProcess]$Process,
    [System.Diagnostics.Stopwatch]$Clock,
    [int]$LimitMilliseconds
) {
    if ($Process.HasExited) {
        return $true
    }
    return $Process.WaitForExit((Get-RemainingMilliseconds $Clock $LimitMilliseconds))
}

function Wait-TaskUntil(
    [System.Threading.Tasks.Task]$Task,
    [System.Diagnostics.Stopwatch]$Clock,
    [int]$LimitMilliseconds
) {
    if ($Task.IsCompleted) {
        return $true
    }
    try {
        return $Task.Wait((Get-RemainingMilliseconds $Clock $LimitMilliseconds))
    } catch [System.AggregateException] {
        # A faulted task is complete. GetResult below will preserve its precise
        # stream error instead of misclassifying it as a timeout.
        return $true
    }
}

function Invoke-CtxRaw([string[]]$Arguments) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    [void]$start.EnvironmentVariables.Remove("CI")
    $isCommandScript = [System.IO.Path]::GetExtension($Binary) -ieq ".cmd"
    if ($isCommandScript) {
        $start.FileName = $env:ComSpec
    } else {
        $start.FileName = $Binary
    }

    if ($isCommandScript) {
        $command = (@($Binary) + $Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
        $start.Arguments = "/d /s /c `"$command`""
    } else {
        $start.Arguments = ($Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
    }
    $commandLine = @((ConvertTo-NativeArgument $start.FileName), $start.Arguments) |
        Where-Object { -not [string]::IsNullOrEmpty($_) }
    $commandLine = $commandLine -join " "
    $timeoutMilliseconds = $timeoutSeconds * 1000
    $commandClock = [System.Diagnostics.Stopwatch]::StartNew()
    $process = $null
    try {
        $process = [CtxNativeOwnedProcess]::Start(
            $start.FileName,
            $commandLine,
            (Get-Location).Path,
            $start.EnvironmentVariables)
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()

        $timeoutPhase = $null
        $rootExitCode = $null
        $cleanupAfterExit = $false
        if (-not (Wait-ProcessUntil $process $commandClock $timeoutMilliseconds)) {
            $timeoutPhase = "process exit"
        } else {
            # Preserve the root result before terminating any descendants that
            # retained inherited pipe handles. A short grace lets ordinary
            # buffered output finish without turning a successful root exit
            # into a full command-deadline wait.
            $rootExitCode = $process.ExitCode
            $postExitDrainClock = [System.Diagnostics.Stopwatch]::StartNew()
            $postExitDrainMilliseconds = 1000
            [void](Wait-TaskUntil $stdout $postExitDrainClock $postExitDrainMilliseconds)
            [void](Wait-TaskUntil $stderr $postExitDrainClock $postExitDrainMilliseconds)
            $pendingStreams = @()
            if (-not $stdout.IsCompleted) {
                $pendingStreams += "stdout"
            }
            if (-not $stderr.IsCompleted) {
                $pendingStreams += "stderr"
            }
            if ($pendingStreams.Count -ne 0) {
                $cleanupAfterExit = $true
            }
        }

        if ($null -eq $timeoutPhase -and -not $cleanupAfterExit) {
            $text = @($stdout.GetAwaiter().GetResult(), $stderr.GetAwaiter().GetResult()) |
                Where-Object { -not [string]::IsNullOrEmpty($_) }
            return [pscustomobject]@{
                ExitCode = $rootExitCode
                Text = ($text -join [Environment]::NewLine).TrimEnd()
            }
        }

        $terminationErrors = @()
        try {
            # The root may already have exited while a descendant retains its
            # inherited pipe handles. Terminate the owned job unconditionally.
            $process.Terminate()
        } catch {
            $terminationErrors += $_.Exception.Message
        }

        $finalDrainClock = [System.Diagnostics.Stopwatch]::StartNew()
        $finalDrainMilliseconds = 5000
        $pendingFinal = @()
        if (-not (Wait-ProcessUntil $process $finalDrainClock $finalDrainMilliseconds)) {
            $pendingFinal += "process exit"
        }
        if (-not (Wait-TaskUntil $stdout $finalDrainClock $finalDrainMilliseconds)) {
            $pendingFinal += "stdout"
        }
        if (-not (Wait-TaskUntil $stderr $finalDrainClock $finalDrainMilliseconds)) {
            $pendingFinal += "stderr"
        }

        $terminationDiagnostic = if ($terminationErrors.Count -eq 0) {
            "owned tree termination completed"
        } else {
            "owned tree termination failed: " + ($terminationErrors -join "; ")
        }
        $finalDrainDiagnostic = if ($pendingFinal.Count -eq 0) {
            "final drain completed"
        } else {
            "final drain still pending: " + ($pendingFinal -join ",")
        }

        if ($null -ne $timeoutPhase) {
            Fail ("ctx command exceeded {0} seconds during {1}; {2}; {3}: {4}" -f
                $timeoutSeconds,
                $timeoutPhase,
                $terminationDiagnostic,
                $finalDrainDiagnostic,
                ($Arguments -join " "))
        }
        if ($terminationErrors.Count -ne 0 -or $pendingFinal.Count -ne 0) {
            Fail ("ctx command root exited but owned tree cleanup failed; {0}; {1}: {2}" -f
                $terminationDiagnostic,
                $finalDrainDiagnostic,
                ($Arguments -join " "))
        }

        $text = @($stdout.GetAwaiter().GetResult(), $stderr.GetAwaiter().GetResult()) |
            Where-Object { -not [string]::IsNullOrEmpty($_) }
        return [pscustomobject]@{
            ExitCode = $rootExitCode
            Text = ($text -join [Environment]::NewLine).TrimEnd()
        }
    } finally {
        if ($null -ne $process) {
            $process.Dispose()
        }
    }
}

function Invoke-Ctx([string[]]$Arguments) {
    $result = Invoke-CtxRaw $Arguments
    if ($result.ExitCode -ne 0) {
        Fail ("ctx {0} failed: {1}" -f ($Arguments -join " "), $result.Text)
    }
    return $result.Text
}

try {
    foreach ($name in $isolation.Keys) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
        [Environment]::SetEnvironmentVariable($name, [string]$isolation[$name], "Process")
    }
    Set-Location -LiteralPath $workRoot

    $version = Invoke-Ctx @("--version")
    if ($version.Trim() -ne "ctx $ExpectedVersion") {
        Fail "version mismatch: expected ctx $ExpectedVersion, got $version"
    }
    if ($pairMode) {
        [void](Invoke-Ctx @("pro", "--help"))
    }

    [void](Invoke-Ctx @("setup", "--catalog-only", "--no-daemon", "--progress", "none"))
    $importArguments = @(
        "import", "--input-format", "ctx-history-jsonl-v2", "--path", $Fixture,
        "--no-daemon", "--format=json", "--progress", "none"
    )
    $importResult = Invoke-CtxRaw $importArguments
    $coreManifestRequired = $freshEpochRequired
    if ($importResult.ExitCode -eq 0) {
        $import = $importResult.Text
    } else {
        if ($importResult.Text -notmatch 'no foreground writer was started') {
            Fail ("ctx {0} failed: {1}" -f ($importArguments -join " "), $importResult.Text)
        }
        $coreManifestRequired = $true
        $env:CTX_DAEMON_ENABLED = "true"
        $env:CTX_DAEMON_AUTOSTART_OFF = "0"
        try {
            $import = Invoke-Ctx @(
                "import", "--input-format", "ctx-history-jsonl-v2", "--path", $Fixture,
                "--format=json", "--progress", "none"
            )
        } finally {
            $env:CTX_DAEMON_ENABLED = "false"
            $env:CTX_DAEMON_AUTOSTART_OFF = "1"
        }
    }
    if ($freshEpochRequired) {
        if ($import -notmatch '"current_source_count"\s*:\s*[1-9][0-9]*' -or
            $import -notmatch '"current_indexed_documents"\s*:\s*[1-9][0-9]*' -or
            $import -notmatch '"published_generation"\s*:\s*"[0-9a-f]{64}"') {
            Fail "fixture import did not publish Core-generation authority"
        }
    } elseif ($import -notmatch '"imported_events"\s*:\s*[1-9][0-9]*' -and
            ($import -notmatch '"imported_sources"\s*:\s*[1-9][0-9]*' -or
             $import -notmatch '"published_generation"\s*:\s*"[0-9a-f]{64}"')) {
        Fail "fixture import did not report imported data"
    }

    $search = Invoke-Ctx @("search", "parser test", "--backend", "lexical", "--refresh", "off", "--format=json")
    if ($search -notmatch '"requested_mode"\s*:\s*"lexical"' -or
        $search -notmatch '"effective_mode"\s*:\s*"lexical"' -or
        $search -notmatch [regex]::Escape("Add a parser test.")) {
        Fail "lexical search did not return the expected fixture result"
    }
    # Import and search execute in separate candidate processes. The expected
    # hit plus the absence of the old Store proves fresh Core-generation
    # authority carried the fixture across that boundary.
    if (Test-Path -LiteralPath (Join-Path $dataRoot "work.sqlite")) {
        Fail "candidate created or opened the pre-v0.26 Store"
    }
    if ($coreManifestRequired) {
        $lexicalRoot = Join-Path $dataRoot "search\lexical"
        if (-not (Test-Path -LiteralPath (Join-Path $lexicalRoot "active-generation.json") -PathType Leaf)) {
            Fail "candidate did not publish the fresh lexical generation"
        }
        $manifestRoot = Join-Path $lexicalRoot "ctx-generations"
        $coreManifests = @(Get-ChildItem -LiteralPath $manifestRoot -Filter "*.json" -File -ErrorAction SilentlyContinue)
        if ($coreManifests.Count -eq 0) {
            Fail "candidate did not publish Core-generation authority"
        }
    }

    $env:CTX_SEARCH_SEMANTIC = $null
    $env:CTX_DAEMON_ENABLED = $null
    try {
        $status = Invoke-Ctx @("status", "--format=json")
    } finally {
        $env:CTX_SEARCH_SEMANTIC = "0"
        $env:CTX_DAEMON_ENABLED = "false"
    }
    if ($status -notmatch '"read_only"\s*:\s*true') {
        Fail "read-only status command returned an unexpected payload"
    }
    if ($status -notmatch '"config_source"\s*:\s*"default"' -or
        $status -notmatch '"reason"\s*:\s*"semantic_disabled"') {
        Fail "native candidate does not report semantic search as disabled by default"
    }
    if ($status -match '"source"\s*:\s*"unsupported"') {
        Fail "native candidate unexpectedly reports semantic search as unsupported"
    }

    # Semantic search is supported but opt-in. Without a provisioned model, an
    # explicit offline request must fail before fallback, state, or network.
    $env:CTX_SEARCH_SEMANTIC = "1"
    $env:CTX_DAEMON_ENABLED = "true"
    $savedErrorActionPreference = $ErrorActionPreference
    try {
        # This command must fail. Windows PowerShell promotes native stderr to
        # NativeCommandError when the global preference is Stop, so capture it
        # under Continue and validate the exit status and message ourselves.
        $ErrorActionPreference = "Continue"
        $capabilityResult = Invoke-CtxRaw @("search", "parser test", "--backend", "semantic", "--refresh", "off", "--format=json")
        $capabilityOutput = $capabilityResult.Text
        $capabilityExit = $capabilityResult.ExitCode
    } finally {
        $ErrorActionPreference = $savedErrorActionPreference
    }
    $env:CTX_SEARCH_SEMANTIC = "0"
    $env:CTX_DAEMON_ENABLED = "false"
    $capabilityText = $capabilityOutput -join [Environment]::NewLine
    if ($capabilityExit -eq 0) {
        Fail "semantic-only search unexpectedly succeeded"
    }
    if ($capabilityText -notmatch 'semantic_store_missing|semantic-only search will not initialize or download') {
        Fail "semantic-only search did not report the fail-closed capability contract"
    }
    if ($capabilityText -match '"effective_mode"\s*:\s*"lexical"') {
        Fail "semantic-only search silently fell back to lexical"
    }
    foreach ($unexpected in @(
        (Join-Path $root "semantic-cache"),
        (Join-Path $root "huggingface"),
        (Join-Path $dataRoot "search\semantic")
    )) {
        if (Test-Path -LiteralPath $unexpected) {
            Fail "semantic-only search created semantic state"
        }
    }

    $resultSteps = [ordered]@{
        version = "passed"
        setup = "passed"
        import = "passed"
        search = "passed"
        read_only = "passed"
        semantic_offline_fail_closed = "passed"
    }
    if ($pairMode) {
        $resultSteps = [ordered]@{
            signed_pair_install = "passed"
            companion_selection = "passed"
            version = "passed"
            setup = "passed"
            import = "passed"
            search = "passed"
            read_only = "passed"
            semantic_offline_fail_closed = "passed"
        }
    }

    $result = [ordered]@{
        schema_version = 1
        kind = "ctx-native-candidate-smoke"
        status = "passed"
        steps = $resultSteps
    }
    $resultJson = $result | ConvertTo-Json -Compress -Depth 3
    [System.IO.File]::WriteAllText($resultTemp, $resultJson, (New-Object System.Text.UTF8Encoding($false)))
    Move-Item -LiteralPath $resultTemp -Destination $ResultPath -Force
    Write-Host "native candidate smoke passed: Windows $([Environment]::Is64BitProcess)"
} finally {
    Set-Location -LiteralPath $savedLocation
    foreach ($name in $isolation.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], "Process")
    }
    Remove-Item -LiteralPath $resultTemp -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
