using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace Ctx.AgentHistory;

/// <summary>
/// Owns a CLI process and every descendant it launches. Linux establishes a
/// dedicated process group before the CLI executes. Windows creates the root
/// suspended and assigns it to a kill-on-close Job Object before resuming it.
/// </summary>
internal sealed class ProcessTree : IAsyncDisposable
{
    private const int LinuxSigKill = 9;
    private const int LinuxNoSuchProcess = 3;
    private const int LinuxExecutableAccess = 1;
    private const int LinuxPermissionDenied = 13;
    private const uint WindowsCreateSuspended = 0x00000004;
    private const uint WindowsCreateUnicodeEnvironment = 0x00000400;
    private const uint WindowsExtendedStartupInfoPresent = 0x00080000;
    private const uint WindowsDuplicateSameAccess = 0x00000002;
    private const uint WindowsGenericRead = 0x80000000;
    private const uint WindowsFileShareRead = 0x00000001;
    private const uint WindowsFileShareWrite = 0x00000002;
    private const uint WindowsOpenExisting = 3;
    private const uint WindowsFileAttributeNormal = 0x00000080;
    private const uint WindowsHandleFlagInherit = 0x00000001;
    private const uint WindowsJobObjectLimitKillOnJobClose = 0x00002000;
    private const int WindowsJobObjectBasicAccountingInformation = 1;
    private const int WindowsJobObjectExtendedLimitInformation = 9;
    private const int WindowsStartfUseStdHandles = 0x00000100;
    private const int WindowsStdInputHandle = -10;
    private const int WindowsErrorInsufficientBuffer = 122;
    private static readonly IntPtr WindowsProcThreadAttributeHandleList = new(0x00020002);
    private static readonly TimeSpan CleanupTimeout = TimeSpan.FromSeconds(2);
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(10);

    private readonly Process _process;
    private readonly int? _linuxProcessGroupId;
    private SafeFileHandle? _windowsJob;
    private int _cleanupWaitCompleted;
    private int _disposed;

    private ProcessTree(
        Process process,
        Stream standardOutput,
        Stream standardError,
        int? linuxProcessGroupId = null,
        SafeFileHandle? windowsJob = null)
    {
        _process = process;
        StandardOutput = standardOutput;
        StandardError = standardError;
        _linuxProcessGroupId = linuxProcessGroupId;
        _windowsJob = windowsJob;
    }

    internal Stream StandardOutput { get; }
    internal Stream StandardError { get; }
    internal int ExitCode => _process.ExitCode;

    internal static ProcessTree Start(ProcessStartInfo startInfo)
    {
        if (OperatingSystem.IsWindows())
        {
            return StartWindows(startInfo);
        }
        if (OperatingSystem.IsLinux())
        {
            return StartLinux(startInfo);
        }
        throw new PlatformNotSupportedException(
            "the ctx .NET local adapter requires Linux process groups or Windows Job Objects");
    }

    internal Task WaitForExitAsync(CancellationToken cancellationToken)
    {
        return _process.WaitForExitAsync(cancellationToken);
    }

    internal void MarkCleanupWaitCompleted()
    {
        Interlocked.Exchange(ref _cleanupWaitCompleted, 1);
    }

    internal void TryTerminateTree()
    {
        var job = _windowsJob;
        if (job is not null && !job.IsInvalid && !job.IsClosed)
        {
            _ = WindowsTerminateJobObject(job, 1);
        }

        if (_linuxProcessGroupId is { } processGroupId)
        {
            _ = LinuxKill(-processGroupId, LinuxSigKill);
        }

        try
        {
            if (!_process.HasExited)
            {
                _process.Kill(entireProcessTree: true);
            }
        }
        catch (Exception error) when (error is InvalidOperationException or Win32Exception or NotSupportedException)
        {
            // A concurrent exit or an already-terminated OS owner is success.
        }

        // Close the race where setsid had not established the group before the
        // root fallback kill above. Once the CLI can execute, this group exists.
        if (_linuxProcessGroupId is { } finalProcessGroupId)
        {
            _ = LinuxKill(-finalProcessGroupId, LinuxSigKill);
        }
    }

    internal async Task WaitForTreeExitAsync(CancellationToken cancellationToken)
    {
        while (TreeHasLiveProcesses())
        {
            await Task.Delay(PollInterval, cancellationToken).ConfigureAwait(false);
            TryTerminateTree();
        }
    }

    public async ValueTask DisposeAsync()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        TryTerminateTree();
        try
        {
            if (Volatile.Read(ref _cleanupWaitCompleted) == 0)
            {
                using var cleanup = new CancellationTokenSource(CleanupTimeout);
                await WaitForTreeExitAsync(cleanup.Token).ConfigureAwait(false);
            }
        }
        catch (OperationCanceledException)
        {
            // The Job Object/process group remains the authority; closing the
            // job is also a final Windows kill-on-close backstop.
        }
        finally
        {
            TryDispose(StandardOutput);
            TryDispose(StandardError);
            TryDispose(_process);
            _windowsJob?.Dispose();
            _windowsJob = null;
        }
    }

    private static ProcessTree StartLinux(ProcessStartInfo startInfo)
    {
        ValidateLinuxExecutable(startInfo);
        var ownedStartInfo = new ProcessStartInfo
        {
            FileName = ResolveSetSid(startInfo),
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            WorkingDirectory = startInfo.WorkingDirectory
        };
        ownedStartInfo.Environment.Clear();
        foreach (var pair in startInfo.Environment)
        {
            ownedStartInfo.Environment[pair.Key] = pair.Value;
        }

        // A Process.Start child inherits its parent's process group and cannot
        // itself be that existing group's leader. Therefore setsid does not
        // fork: the returned PID becomes both the CLI PID and its new PGID.
        ownedStartInfo.ArgumentList.Add("--");
        ownedStartInfo.ArgumentList.Add(startInfo.FileName);
        foreach (var argument in startInfo.ArgumentList)
        {
            ownedStartInfo.ArgumentList.Add(argument);
        }

        var process = new Process { StartInfo = ownedStartInfo };
        try
        {
            if (!process.Start())
            {
                throw new Win32Exception("failed to start the Linux ctx process group");
            }
            return new ProcessTree(
                process,
                process.StandardOutput.BaseStream,
                process.StandardError.BaseStream,
                linuxProcessGroupId: process.Id);
        }
        catch
        {
            process.Dispose();
            throw;
        }
    }

    private static string ResolveSetSid(ProcessStartInfo startInfo)
    {
        foreach (var candidate in new[] { "/usr/bin/setsid", "/bin/setsid" })
        {
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        if (startInfo.Environment.TryGetValue("PATH", out var path))
        {
            var workingDirectory = string.IsNullOrWhiteSpace(startInfo.WorkingDirectory)
                ? Directory.GetCurrentDirectory()
                : Path.GetFullPath(startInfo.WorkingDirectory);
            foreach (var entry in path.Split(Path.PathSeparator))
            {
                var directory = string.IsNullOrEmpty(entry) ? workingDirectory : entry;
                if (!Path.IsPathRooted(directory))
                {
                    directory = Path.GetFullPath(directory, workingDirectory);
                }
                var candidate = Path.Combine(directory, "setsid");
                if (File.Exists(candidate))
                {
                    return candidate;
                }
            }
        }

        throw new Win32Exception(2, "setsid is required to own the ctx CLI process tree on Linux");
    }

    private static void ValidateLinuxExecutable(ProcessStartInfo startInfo)
    {
        var workingDirectory = string.IsNullOrWhiteSpace(startInfo.WorkingDirectory)
            ? Directory.GetCurrentDirectory()
            : Path.GetFullPath(startInfo.WorkingDirectory);
        if (startInfo.FileName.Contains(Path.DirectorySeparatorChar))
        {
            var candidate = Path.IsPathRooted(startInfo.FileName)
                ? startInfo.FileName
                : Path.GetFullPath(startInfo.FileName, workingDirectory);
            var isDirectory = Directory.Exists(candidate);
            if (LinuxAccess(candidate, LinuxExecutableAccess) == 0 && !isDirectory)
            {
                return;
            }
            throw new Win32Exception(isDirectory ? LinuxPermissionDenied : Marshal.GetLastWin32Error());
        }

        var path = startInfo.Environment.TryGetValue("PATH", out var configuredPath)
            && configuredPath is not null
                ? configuredPath
                : "/bin:/usr/bin";
        var error = 2;
        foreach (var entry in path.Split(Path.PathSeparator))
        {
            var directory = string.IsNullOrEmpty(entry) ? workingDirectory : entry;
            if (!Path.IsPathRooted(directory))
            {
                directory = Path.GetFullPath(directory, workingDirectory);
            }
            var candidate = Path.Combine(directory, startInfo.FileName);
            if (LinuxAccess(candidate, LinuxExecutableAccess) == 0 && !Directory.Exists(candidate))
            {
                return;
            }
            if (Marshal.GetLastWin32Error() == LinuxPermissionDenied || Directory.Exists(candidate))
            {
                error = LinuxPermissionDenied;
            }
        }
        throw new Win32Exception(error);
    }

    private bool TreeHasLiveProcesses()
    {
        var job = _windowsJob;
        if (job is not null && !job.IsInvalid && !job.IsClosed)
        {
            return WindowsJobHasActiveProcesses(job);
        }
        if (_linuxProcessGroupId is { } processGroupId)
        {
            return LinuxProcessGroupHasLiveProcesses(processGroupId);
        }
        try
        {
            return !_process.HasExited;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
    }

    private static bool LinuxProcessGroupHasLiveProcesses(int processGroupId)
    {
        try
        {
            foreach (var directory in Directory.EnumerateDirectories("/proc"))
            {
                if (!int.TryParse(Path.GetFileName(directory), NumberStyles.None, CultureInfo.InvariantCulture, out _))
                {
                    continue;
                }
                try
                {
                    var stat = File.ReadAllText(Path.Combine(directory, "stat"));
                    var commandEnd = stat.LastIndexOf(')');
                    if (commandEnd < 0 || commandEnd + 2 >= stat.Length)
                    {
                        continue;
                    }
                    var fields = stat[(commandEnd + 2)..]
                        .Split(' ', StringSplitOptions.RemoveEmptyEntries);
                    if (fields.Length > 2
                        && int.TryParse(fields[2], NumberStyles.None, CultureInfo.InvariantCulture, out var group)
                        && group == processGroupId
                        && fields[0] is not ("Z" or "X"))
                    {
                        return true;
                    }
                }
                catch (Exception error) when (error is IOException or UnauthorizedAccessException)
                {
                    // Processes can exit while /proc is sampled.
                }
            }
            return false;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            var result = LinuxKill(-processGroupId, 0);
            return result == 0 || Marshal.GetLastWin32Error() != LinuxNoSuchProcess;
        }
    }

    private static ProcessTree StartWindows(ProcessStartInfo startInfo)
    {
        SafeFileHandle? stdoutRead = null;
        SafeFileHandle? stdoutWrite = null;
        SafeFileHandle? stderrRead = null;
        SafeFileHandle? stderrWrite = null;
        SafeFileHandle? standardInput = null;
        SafeFileHandle? job = null;
        SafeFileHandle? nativeProcess = null;
        SafeFileHandle? nativeThread = null;
        FileStream? standardOutputStream = null;
        FileStream? standardErrorStream = null;
        Process? process = null;
        IntPtr environmentBlock = IntPtr.Zero;

        try
        {
            (stdoutRead, stdoutWrite) = CreateWindowsPipe();
            (stderrRead, stderrWrite) = CreateWindowsPipe();
            standardInput = DuplicateWindowsStandardInput();
            job = CreateWindowsJob();

            using var attributes = new WindowsHandleList(
                standardInput.DangerousGetHandle(),
                stdoutWrite.DangerousGetHandle(),
                stderrWrite.DangerousGetHandle());
            var startupInfo = new WindowsStartupInfoEx
            {
                StartupInfo = new WindowsStartupInfo
                {
                    Size = Marshal.SizeOf<WindowsStartupInfoEx>(),
                    Flags = WindowsStartfUseStdHandles,
                    StandardInput = standardInput.DangerousGetHandle(),
                    StandardOutput = stdoutWrite.DangerousGetHandle(),
                    StandardError = stderrWrite.DangerousGetHandle()
                },
                AttributeList = attributes.List
            };
            environmentBlock = CreateWindowsEnvironmentBlock(startInfo.Environment);
            var commandLine = BuildWindowsCommandLine(startInfo);
            var creationFlags = WindowsCreateSuspended
                | WindowsCreateUnicodeEnvironment
                | WindowsExtendedStartupInfoPresent;
            var currentDirectory = string.IsNullOrWhiteSpace(startInfo.WorkingDirectory)
                ? null
                : startInfo.WorkingDirectory;

            if (!WindowsCreateProcess(
                    null,
                    commandLine,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    true,
                    creationFlags,
                    environmentBlock,
                    currentDirectory,
                    ref startupInfo,
                    out var processInformation))
            {
                throw LastWindowsError();
            }

            nativeProcess = new SafeFileHandle(processInformation.Process, ownsHandle: true);
            nativeThread = new SafeFileHandle(processInformation.Thread, ownsHandle: true);
            if (!WindowsAssignProcessToJobObject(job, nativeProcess))
            {
                throw LastWindowsError();
            }

            process = Process.GetProcessById(checked((int)processInformation.ProcessId));
            // CreatePipe produces synchronous handles. FileStream still exposes
            // ReadAsync through the thread pool when isAsync is false.
            standardOutputStream = new FileStream(stdoutRead, FileAccess.Read, 4096, isAsync: false);
            stdoutRead = null;
            standardErrorStream = new FileStream(stderrRead, FileAccess.Read, 4096, isAsync: false);
            stderrRead = null;

            if (WindowsResumeThread(nativeThread) == uint.MaxValue)
            {
                throw LastWindowsError();
            }

            var result = new ProcessTree(
                process,
                standardOutputStream,
                standardErrorStream,
                windowsJob: job);
            process = null;
            standardOutputStream = null;
            standardErrorStream = null;
            job = null;
            return result;
        }
        catch
        {
            if (nativeProcess is not null && !nativeProcess.IsInvalid && !nativeProcess.IsClosed)
            {
                _ = WindowsTerminateProcess(nativeProcess, 1);
            }
            if (job is not null && !job.IsInvalid && !job.IsClosed)
            {
                _ = WindowsTerminateJobObject(job, 1);
            }
            throw;
        }
        finally
        {
            if (environmentBlock != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(environmentBlock);
            }
            TryDispose(nativeThread);
            TryDispose(nativeProcess);
            TryDispose(standardInput);
            TryDispose(stdoutWrite);
            TryDispose(stderrWrite);
            TryDispose(stdoutRead);
            TryDispose(stderrRead);
            TryDispose(standardOutputStream);
            TryDispose(standardErrorStream);
            TryDispose(process);
            TryDispose(job);
        }
    }

    private static (SafeFileHandle Read, SafeFileHandle Write) CreateWindowsPipe()
    {
        var security = new WindowsSecurityAttributes
        {
            Length = Marshal.SizeOf<WindowsSecurityAttributes>(),
            InheritHandle = true
        };
        if (!WindowsCreatePipe(out var read, out var write, ref security, 0))
        {
            throw LastWindowsError();
        }
        if (!WindowsSetHandleInformation(read, WindowsHandleFlagInherit, 0))
        {
            var error = LastWindowsError();
            read.Dispose();
            write.Dispose();
            throw error;
        }
        return (read, write);
    }

    private static SafeFileHandle DuplicateWindowsStandardInput()
    {
        var standardInput = WindowsGetStdHandle(WindowsStdInputHandle);
        var currentProcess = WindowsGetCurrentProcess();
        if (standardInput != IntPtr.Zero
            && standardInput != new IntPtr(-1)
            && WindowsDuplicateHandle(
                currentProcess,
                standardInput,
                currentProcess,
                out var duplicate,
                0,
                true,
                WindowsDuplicateSameAccess))
        {
            return duplicate;
        }

        var security = new WindowsSecurityAttributes
        {
            Length = Marshal.SizeOf<WindowsSecurityAttributes>(),
            InheritHandle = true
        };
        var nullInput = WindowsCreateFile(
            "NUL",
            WindowsGenericRead,
            WindowsFileShareRead | WindowsFileShareWrite,
            ref security,
            WindowsOpenExisting,
            WindowsFileAttributeNormal,
            IntPtr.Zero);
        if (nullInput.IsInvalid)
        {
            var error = LastWindowsError();
            nullInput.Dispose();
            throw error;
        }
        return nullInput;
    }

    private static SafeFileHandle CreateWindowsJob()
    {
        var job = WindowsCreateJobObject(IntPtr.Zero, null);
        if (job.IsInvalid)
        {
            var error = LastWindowsError();
            job.Dispose();
            throw error;
        }
        var limits = new WindowsJobObjectExtendedLimitInformation
        {
            BasicLimitInformation = new WindowsJobObjectBasicLimitInformation
            {
                LimitFlags = WindowsJobObjectLimitKillOnJobClose
            }
        };
        if (!WindowsSetInformationJobObject(
                job,
                WindowsJobObjectExtendedLimitInformation,
                ref limits,
                Marshal.SizeOf<WindowsJobObjectExtendedLimitInformation>()))
        {
            var error = LastWindowsError();
            job.Dispose();
            throw error;
        }
        return job;
    }

    private static bool WindowsJobHasActiveProcesses(SafeFileHandle job)
    {
        return !WindowsQueryInformationJobObject(
                job,
                WindowsJobObjectBasicAccountingInformation,
                out var accounting,
                Marshal.SizeOf<WindowsJobObjectBasicAccountingInformation>(),
                IntPtr.Zero)
            || accounting.ActiveProcesses != 0;
    }

    private static IntPtr CreateWindowsEnvironmentBlock(IDictionary<string, string?> environment)
    {
        var block = new StringBuilder();
        foreach (var pair in environment.OrderBy(pair => pair.Key, StringComparer.OrdinalIgnoreCase))
        {
            if (pair.Value is null)
            {
                continue;
            }
            block.Append(pair.Key);
            block.Append('=');
            block.Append(pair.Value);
            block.Append('\0');
        }
        block.Append('\0');
        if (block.Length == 1)
        {
            block.Append('\0');
        }
        return Marshal.StringToHGlobalUni(block.ToString());
    }

    private static StringBuilder BuildWindowsCommandLine(ProcessStartInfo startInfo)
    {
        var commandLine = new StringBuilder(QuoteWindowsArgument(startInfo.FileName));
        foreach (var argument in startInfo.ArgumentList)
        {
            commandLine.Append(' ');
            commandLine.Append(QuoteWindowsArgument(argument));
        }
        return commandLine;
    }

    private static string QuoteWindowsArgument(string argument)
    {
        if (argument.Length > 0 && !argument.Any(character => char.IsWhiteSpace(character) || character == '"'))
        {
            return argument;
        }

        var quoted = new StringBuilder(argument.Length + 2);
        quoted.Append('"');
        var backslashes = 0;
        foreach (var character in argument)
        {
            if (character == '\\')
            {
                backslashes++;
                continue;
            }
            if (character == '"')
            {
                quoted.Append('\\', checked(backslashes * 2 + 1));
                quoted.Append('"');
                backslashes = 0;
                continue;
            }
            quoted.Append('\\', backslashes);
            backslashes = 0;
            quoted.Append(character);
        }
        quoted.Append('\\', checked(backslashes * 2));
        quoted.Append('"');
        return quoted.ToString();
    }

    private static Win32Exception LastWindowsError()
    {
        return new Win32Exception(Marshal.GetLastWin32Error());
    }

    private static void TryDispose(IDisposable? disposable)
    {
        try
        {
            disposable?.Dispose();
        }
        catch
        {
            // Cleanup is best-effort after the OS tree owner has terminated.
        }
    }

    private sealed class WindowsHandleList : IDisposable
    {
        private IntPtr _handles;
        private IntPtr _list;
        private bool _listInitialized;

        internal WindowsHandleList(params IntPtr[] handles)
        {
            try
            {
                UIntPtr size = UIntPtr.Zero;
                var sized = WindowsInitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref size);
                var error = Marshal.GetLastWin32Error();
                if (sized || size == UIntPtr.Zero || error != WindowsErrorInsufficientBuffer)
                {
                    throw new Win32Exception(error);
                }

                _list = Marshal.AllocHGlobal(checked((int)size.ToUInt64()));
                if (!WindowsInitializeProcThreadAttributeList(_list, 1, 0, ref size))
                {
                    throw LastWindowsError();
                }
                _listInitialized = true;

                _handles = Marshal.AllocHGlobal(checked(IntPtr.Size * handles.Length));
                for (var index = 0; index < handles.Length; index++)
                {
                    Marshal.WriteIntPtr(_handles, index * IntPtr.Size, handles[index]);
                }
                if (!WindowsUpdateProcThreadAttribute(
                        _list,
                        0,
                        WindowsProcThreadAttributeHandleList,
                        _handles,
                        new UIntPtr(checked((uint)(IntPtr.Size * handles.Length))),
                        IntPtr.Zero,
                        IntPtr.Zero))
                {
                    throw LastWindowsError();
                }
            }
            catch
            {
                Dispose();
                throw;
            }
        }

        internal IntPtr List => _list;

        public void Dispose()
        {
            if (_list != IntPtr.Zero)
            {
                if (_listInitialized)
                {
                    WindowsDeleteProcThreadAttributeList(_list);
                    _listInitialized = false;
                }
                Marshal.FreeHGlobal(_list);
                _list = IntPtr.Zero;
            }
            if (_handles != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(_handles);
                _handles = IntPtr.Zero;
            }
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowsSecurityAttributes
    {
        internal int Length;
        internal IntPtr SecurityDescriptor;
        [MarshalAs(UnmanagedType.Bool)] internal bool InheritHandle;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WindowsStartupInfo
    {
        internal int Size;
        internal string? Reserved;
        internal string? Desktop;
        internal string? Title;
        internal int X;
        internal int Y;
        internal int XSize;
        internal int YSize;
        internal int XCountChars;
        internal int YCountChars;
        internal int FillAttribute;
        internal int Flags;
        internal short ShowWindow;
        internal short Reserved2Size;
        internal IntPtr Reserved2;
        internal IntPtr StandardInput;
        internal IntPtr StandardOutput;
        internal IntPtr StandardError;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WindowsStartupInfoEx
    {
        internal WindowsStartupInfo StartupInfo;
        internal IntPtr AttributeList;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowsProcessInformation
    {
        internal IntPtr Process;
        internal IntPtr Thread;
        internal uint ProcessId;
        internal uint ThreadId;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowsJobObjectBasicAccountingInformation
    {
        internal long TotalUserTime;
        internal long TotalKernelTime;
        internal long ThisPeriodTotalUserTime;
        internal long ThisPeriodTotalKernelTime;
        internal uint TotalPageFaultCount;
        internal uint TotalProcesses;
        internal uint ActiveProcesses;
        internal uint TotalTerminatedProcesses;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowsJobObjectBasicLimitInformation
    {
        internal long PerProcessUserTimeLimit;
        internal long PerJobUserTimeLimit;
        internal uint LimitFlags;
        internal UIntPtr MinimumWorkingSetSize;
        internal UIntPtr MaximumWorkingSetSize;
        internal uint ActiveProcessLimit;
        internal UIntPtr Affinity;
        internal uint PriorityClass;
        internal uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowsIoCounters
    {
        internal ulong ReadOperationCount;
        internal ulong WriteOperationCount;
        internal ulong OtherOperationCount;
        internal ulong ReadTransferCount;
        internal ulong WriteTransferCount;
        internal ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WindowsJobObjectExtendedLimitInformation
    {
        internal WindowsJobObjectBasicLimitInformation BasicLimitInformation;
        internal WindowsIoCounters IoInfo;
        internal UIntPtr ProcessMemoryLimit;
        internal UIntPtr JobMemoryLimit;
        internal UIntPtr PeakProcessMemoryUsed;
        internal UIntPtr PeakJobMemoryUsed;
    }

    [DllImport("libc", EntryPoint = "kill", SetLastError = true)]
    private static extern int LinuxKill(int processId, int signal);

    [DllImport("libc", EntryPoint = "access", CharSet = CharSet.Ansi, SetLastError = true)]
    private static extern int LinuxAccess(string path, int mode);

    [DllImport("kernel32.dll", EntryPoint = "CreatePipe", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsCreatePipe(
        out SafeFileHandle readPipe,
        out SafeFileHandle writePipe,
        ref WindowsSecurityAttributes pipeAttributes,
        int size);

    [DllImport("kernel32.dll", EntryPoint = "SetHandleInformation", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsSetHandleInformation(
        SafeFileHandle handle,
        uint mask,
        uint flags);

    [DllImport("kernel32.dll", EntryPoint = "GetStdHandle", SetLastError = true)]
    private static extern IntPtr WindowsGetStdHandle(int standardHandle);

    [DllImport("kernel32.dll", EntryPoint = "GetCurrentProcess")]
    private static extern IntPtr WindowsGetCurrentProcess();

    [DllImport("kernel32.dll", EntryPoint = "DuplicateHandle", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsDuplicateHandle(
        IntPtr sourceProcess,
        IntPtr sourceHandle,
        IntPtr targetProcess,
        out SafeFileHandle targetHandle,
        uint desiredAccess,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandle,
        uint options);

    [DllImport("kernel32.dll", EntryPoint = "CreateFileW", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    private static extern SafeFileHandle WindowsCreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        ref WindowsSecurityAttributes securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    [DllImport("kernel32.dll", EntryPoint = "CreateJobObjectW", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    private static extern SafeFileHandle WindowsCreateJobObject(IntPtr jobAttributes, string? name);

    [DllImport("kernel32.dll", EntryPoint = "SetInformationJobObject", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsSetInformationJobObject(
        SafeFileHandle job,
        int informationClass,
        ref WindowsJobObjectExtendedLimitInformation information,
        int informationLength);

    [DllImport("kernel32.dll", EntryPoint = "AssignProcessToJobObject", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsAssignProcessToJobObject(
        SafeFileHandle job,
        SafeFileHandle process);

    [DllImport("kernel32.dll", EntryPoint = "TerminateJobObject", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsTerminateJobObject(SafeFileHandle job, uint exitCode);

    [DllImport("kernel32.dll", EntryPoint = "TerminateProcess", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsTerminateProcess(SafeFileHandle process, uint exitCode);

    [DllImport("kernel32.dll", EntryPoint = "QueryInformationJobObject", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsQueryInformationJobObject(
        SafeFileHandle job,
        int informationClass,
        out WindowsJobObjectBasicAccountingInformation information,
        int informationLength,
        IntPtr returnLength);

    [DllImport("kernel32.dll", EntryPoint = "InitializeProcThreadAttributeList", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsInitializeProcThreadAttributeList(
        IntPtr attributeList,
        int attributeCount,
        int flags,
        ref UIntPtr size);

    [DllImport("kernel32.dll", EntryPoint = "UpdateProcThreadAttribute", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsUpdateProcThreadAttribute(
        IntPtr attributeList,
        uint flags,
        IntPtr attribute,
        IntPtr value,
        UIntPtr size,
        IntPtr previousValue,
        IntPtr returnSize);

    [DllImport("kernel32.dll", EntryPoint = "DeleteProcThreadAttributeList")]
    private static extern void WindowsDeleteProcThreadAttributeList(IntPtr attributeList);

    [DllImport("kernel32.dll", EntryPoint = "CreateProcessW", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WindowsCreateProcess(
        string? applicationName,
        StringBuilder commandLine,
        IntPtr processAttributes,
        IntPtr threadAttributes,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandles,
        uint creationFlags,
        IntPtr environment,
        string? currentDirectory,
        ref WindowsStartupInfoEx startupInfo,
        out WindowsProcessInformation processInformation);

    [DllImport("kernel32.dll", EntryPoint = "ResumeThread", SetLastError = true)]
    private static extern uint WindowsResumeThread(SafeFileHandle thread);
}
