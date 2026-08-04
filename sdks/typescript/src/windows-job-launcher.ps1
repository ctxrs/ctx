param(
  [Parameter(Mandatory = $true)]
  [string]$ControlPipe
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Send-LauncherFailure {
  param([string]$Message)

  try {
    $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
      ".",
      $ControlPipe,
      [System.IO.Pipes.PipeDirection]::Out,
      [System.IO.Pipes.PipeOptions]::WriteThrough
    )
    $pipe.Connect(5000)
    try {
      $writer = [System.IO.StreamWriter]::new(
        $pipe,
        [System.Text.UTF8Encoding]::new($false),
        1024,
        $true
      )
      try {
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Message))
        $writer.WriteLine("error:$encoded")
        $writer.Flush()
      }
      finally {
        $writer.Dispose()
      }
    }
    finally {
      $pipe.Dispose()
    }
  }
  catch {
    # The Node owner may already have gone away. In that case process exit still closes
    # every native handle held by this launcher.
  }
}

$launcherSource = @'
using System;
using System.ComponentModel;
using System.Globalization;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class CtxWindowsJobLauncher
{
    private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;
    private const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
    private const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
    private const uint CREATE_NO_WINDOW = 0x08000000;
    private const uint DUPLICATE_SAME_ACCESS = 0x00000002;
    private const uint GENERIC_READ = 0x80000000;
    private const uint FILE_SHARE_READ = 0x00000001;
    private const uint FILE_SHARE_WRITE = 0x00000002;
    private const uint OPEN_EXISTING = 3;
    private const uint FILE_ATTRIBUTE_NORMAL = 0x00000080;
    private const int STD_OUTPUT_HANDLE = -11;
    private const int STD_ERROR_HANDLE = -12;
    private const int WAIT_OBJECT_0 = 0;
    private const int WAIT_FAILED = -1;
    private const int ERROR_INSUFFICIENT_BUFFER = 122;
    private const int STARTUP_ATTRIBUTE_COUNT = 2;
    private const long PROC_THREAD_ATTRIBUTE_HANDLE_LIST = 0x00020002;
    private const long PROC_THREAD_ATTRIBUTE_JOB_LIST = 0x0002000D;
    private const int DESCENDANT_GRACE_MS = 750;
    private const int TEARDOWN_WAIT_MS = 1000;
    private static readonly IntPtr InvalidHandle = new IntPtr(-1);

    [StructLayout(LayoutKind.Sequential)]
    private struct SECURITY_ATTRIBUTES
    {
        public int nLength;
        public IntPtr lpSecurityDescriptor;
        [MarshalAs(UnmanagedType.Bool)] public bool bInheritHandle;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct STARTUPINFO
    {
        public int cb;
        public string lpReserved;
        public string lpDesktop;
        public string lpTitle;
        public int dwX;
        public int dwY;
        public int dwXSize;
        public int dwYSize;
        public int dwXCountChars;
        public int dwYCountChars;
        public int dwFillAttribute;
        public int dwFlags;
        public short wShowWindow;
        public short cbReserved2;
        public IntPtr lpReserved2;
        public IntPtr hStdInput;
        public IntPtr hStdOutput;
        public IntPtr hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct STARTUPINFOEX
    {
        public STARTUPINFO StartupInfo;
        public IntPtr lpAttributeList;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_INFORMATION
    {
        public IntPtr hProcess;
        public IntPtr hThread;
        public int dwProcessId;
        public int dwThreadId;
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

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_BASIC_ACCOUNTING_INFORMATION
    {
        public long TotalUserTime;
        public long TotalKernelTime;
        public long ThisPeriodTotalUserTime;
        public long ThisPeriodTotalKernelTime;
        public uint TotalPageFaultCount;
        public uint TotalProcesses;
        public uint ActiveProcesses;
        public uint TotalTerminatedProcesses;
    }

    private enum JOBOBJECTINFOCLASS
    {
        JobObjectBasicAccountingInformation = 1,
        JobObjectExtendedLimitInformation = 9
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateJobObject(IntPtr jobAttributes, string name);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetInformationJobObject(
        IntPtr job,
        JOBOBJECTINFOCLASS infoClass,
        IntPtr info,
        uint infoLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryInformationJobObject(
        IntPtr job,
        JOBOBJECTINFOCLASS infoClass,
        out JOBOBJECT_BASIC_ACCOUNTING_INFORMATION info,
        uint infoLength,
        IntPtr returnLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool TerminateJobObject(IntPtr job, uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool InitializeProcThreadAttributeList(
        IntPtr attributeList,
        int attributeCount,
        int flags,
        ref UIntPtr size);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UpdateProcThreadAttribute(
        IntPtr attributeList,
        uint flags,
        IntPtr attribute,
        IntPtr value,
        UIntPtr size,
        IntPtr previousValue,
        IntPtr returnSize);

    [DllImport("kernel32.dll")]
    private static extern void DeleteProcThreadAttributeList(IntPtr attributeList);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CreateProcess(
        string applicationName,
        StringBuilder commandLine,
        IntPtr processAttributes,
        IntPtr threadAttributes,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandles,
        uint creationFlags,
        IntPtr environment,
        string currentDirectory,
        ref STARTUPINFOEX startupInfo,
        out PROCESS_INFORMATION processInformation);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern int WaitForSingleObject(IntPtr handle, int milliseconds);

    [DllImport("kernel32.dll")]
    private static extern ulong GetTickCount64();

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DuplicateHandle(
        IntPtr sourceProcess,
        IntPtr sourceHandle,
        IntPtr targetProcess,
        out IntPtr targetHandle,
        uint desiredAccess,
        [MarshalAs(UnmanagedType.Bool)] bool inheritHandle,
        uint options);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr GetStdHandle(int standardHandle);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateFile(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile);

    public static int Run(
        string controlPipe,
        string command,
        string[] arguments,
        string[] environmentKeys,
        string[] environmentValues,
        string currentDirectory)
    {
        using (NamedPipeClientStream pipe = new NamedPipeClientStream(
            ".", controlPipe, PipeDirection.Out, PipeOptions.WriteThrough))
        {
            pipe.Connect(5000);
            using (StreamWriter writer = new StreamWriter(
                pipe, new UTF8Encoding(false), 1024, true))
            {
                writer.AutoFlush = true;
                try
                {
                    return RunOwned(command, arguments, environmentKeys, environmentValues,
                        currentDirectory, writer);
                }
                catch (Exception error)
                {
                    string encoded = Convert.ToBase64String(
                        Encoding.UTF8.GetBytes(error.GetType().Name + ": " + error.Message));
                    writer.WriteLine("error:" + encoded);
                    return 127;
                }
            }
        }
    }

    private static int RunOwned(
        string command,
        string[] arguments,
        string[] environmentKeys,
        string[] environmentValues,
        string currentDirectory,
        StreamWriter control)
    {
        IntPtr job = IntPtr.Zero;
        IntPtr environment = IntPtr.Zero;
        IntPtr attributeList = IntPtr.Zero;
        IntPtr handleList = IntPtr.Zero;
        IntPtr jobList = IntPtr.Zero;
        IntPtr childInput = IntPtr.Zero;
        IntPtr childOutput = IntPtr.Zero;
        IntPtr childError = IntPtr.Zero;
        PROCESS_INFORMATION process = new PROCESS_INFORMATION();
        try
        {
            job = CreateJobObject(IntPtr.Zero, null);
            EnsureHandle(job, "CreateJobObject");
            ConfigureKillOnClose(job);

            childInput = OpenInheritedNullInput();
            childOutput = DuplicateInheritedHandle(GetStdHandle(STD_OUTPUT_HANDLE), "stdout");
            childError = DuplicateInheritedHandle(GetStdHandle(STD_ERROR_HANDLE), "stderr");
            environment = BuildEnvironmentBlock(environmentKeys, environmentValues);

            STARTUPINFOEX startup = new STARTUPINFOEX();
            startup.StartupInfo.cb = Marshal.SizeOf(typeof(STARTUPINFOEX));
            startup.StartupInfo.dwFlags = 0x00000100;
            startup.StartupInfo.hStdInput = childInput;
            startup.StartupInfo.hStdOutput = childOutput;
            startup.StartupInfo.hStdError = childError;

            UIntPtr attributeBytes = UIntPtr.Zero;
            bool initialized = InitializeProcThreadAttributeList(
                IntPtr.Zero, STARTUP_ATTRIBUTE_COUNT, 0, ref attributeBytes);
            int initializeError = Marshal.GetLastWin32Error();
            if (initialized || initializeError != ERROR_INSUFFICIENT_BUFFER)
            {
                throw new Win32Exception(initializeError,
                    "InitializeProcThreadAttributeList(size) failed");
            }
            attributeList = Marshal.AllocHGlobal(checked((int)attributeBytes.ToUInt64()));
            if (!InitializeProcThreadAttributeList(
                attributeList, STARTUP_ATTRIBUTE_COUNT, 0, ref attributeBytes))
            {
                ThrowLastWin32("InitializeProcThreadAttributeList");
            }
            startup.lpAttributeList = attributeList;

            IntPtr[] inheritedHandles = new IntPtr[] { childInput, childOutput, childError };
            handleList = Marshal.AllocHGlobal(IntPtr.Size * inheritedHandles.Length);
            for (int index = 0; index < inheritedHandles.Length; index++)
            {
                Marshal.WriteIntPtr(handleList, index * IntPtr.Size, inheritedHandles[index]);
            }
            if (!UpdateProcThreadAttribute(
                attributeList,
                0,
                new IntPtr(PROC_THREAD_ATTRIBUTE_HANDLE_LIST),
                handleList,
                new UIntPtr((uint)(IntPtr.Size * inheritedHandles.Length)),
                IntPtr.Zero,
                IntPtr.Zero))
            {
                ThrowLastWin32("UpdateProcThreadAttribute(handle list)");
            }

            jobList = Marshal.AllocHGlobal(IntPtr.Size);
            Marshal.WriteIntPtr(jobList, job);
            if (!UpdateProcThreadAttribute(
                attributeList,
                0,
                new IntPtr(PROC_THREAD_ATTRIBUTE_JOB_LIST),
                jobList,
                new UIntPtr((uint)IntPtr.Size),
                IntPtr.Zero,
                IntPtr.Zero))
            {
                ThrowLastWin32("UpdateProcThreadAttribute(job list)");
            }

            StringBuilder commandLine = BuildCommandLine(command, arguments);
            uint creationFlags = CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT
                | CREATE_NO_WINDOW;
            if (!CreateProcess(
                null,
                commandLine,
                IntPtr.Zero,
                IntPtr.Zero,
                true,
                creationFlags,
                environment,
                String.IsNullOrEmpty(currentDirectory) ? null : currentDirectory,
                ref startup,
                out process))
            {
                ThrowLastWin32("CreateProcess(" + command + ")");
            }
            control.WriteLine("started:" + process.dwProcessId.ToString(CultureInfo.InvariantCulture));

            int waitResult = WaitForSingleObject(process.hProcess, -1);
            if (waitResult == WAIT_FAILED)
            {
                ThrowLastWin32("WaitForSingleObject");
            }
            if (waitResult != WAIT_OBJECT_0)
            {
                throw new InvalidOperationException("unexpected process wait result: " + waitResult);
            }

            uint rootExitCode;
            if (!GetExitCodeProcess(process.hProcess, out rootExitCode))
            {
                ThrowLastWin32("GetExitCodeProcess");
            }
            control.WriteLine("exit:" + rootExitCode.ToString(CultureInfo.InvariantCulture));

            WaitForDescendantsOrTerminate(job);
            return unchecked((int)rootExitCode);
        }
        finally
        {
            CloseIfValid(process.hThread);
            CloseIfValid(process.hProcess);
            CloseIfValid(childInput);
            CloseIfValid(childOutput);
            CloseIfValid(childError);
            if (attributeList != IntPtr.Zero)
            {
                DeleteProcThreadAttributeList(attributeList);
                Marshal.FreeHGlobal(attributeList);
            }
            if (handleList != IntPtr.Zero) Marshal.FreeHGlobal(handleList);
            if (jobList != IntPtr.Zero) Marshal.FreeHGlobal(jobList);
            if (environment != IntPtr.Zero) Marshal.FreeHGlobal(environment);
            // KILL_ON_JOB_CLOSE is the final, crash-safe ownership boundary. If any exception
            // occurs after atomic process creation, closing this handle terminates the job.
            CloseIfValid(job);
        }
    }

    private static void ConfigureKillOnClose(IntPtr job)
    {
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits =
            new JOBOBJECT_EXTENDED_LIMIT_INFORMATION();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        int bytes = Marshal.SizeOf(typeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION));
        IntPtr buffer = Marshal.AllocHGlobal(bytes);
        try
        {
            Marshal.StructureToPtr(limits, buffer, false);
            if (!SetInformationJobObject(
                job,
                JOBOBJECTINFOCLASS.JobObjectExtendedLimitInformation,
                buffer,
                (uint)bytes))
            {
                ThrowLastWin32("SetInformationJobObject");
            }
        }
        finally
        {
            Marshal.FreeHGlobal(buffer);
        }
    }

    private static void WaitForDescendantsOrTerminate(IntPtr job)
    {
        ulong gracefulDeadline = GetTickCount64() + DESCENDANT_GRACE_MS;
        while (ActiveProcesses(job) != 0 && GetTickCount64() < gracefulDeadline)
        {
            Thread.Sleep(10);
        }
        if (ActiveProcesses(job) == 0) return;

        if (!TerminateJobObject(job, 1))
        {
            ThrowLastWin32("TerminateJobObject");
        }
        ulong teardownDeadline = GetTickCount64() + TEARDOWN_WAIT_MS;
        while (ActiveProcesses(job) != 0 && GetTickCount64() < teardownDeadline)
        {
            Thread.Sleep(10);
        }
    }

    private static uint ActiveProcesses(IntPtr job)
    {
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting;
        if (!QueryInformationJobObject(
            job,
            JOBOBJECTINFOCLASS.JobObjectBasicAccountingInformation,
            out accounting,
            (uint)Marshal.SizeOf(typeof(JOBOBJECT_BASIC_ACCOUNTING_INFORMATION)),
            IntPtr.Zero))
        {
            ThrowLastWin32("QueryInformationJobObject");
        }
        return accounting.ActiveProcesses;
    }

    private static IntPtr OpenInheritedNullInput()
    {
        IntPtr nullInput = CreateFile(
            "NUL",
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            IntPtr.Zero);
        EnsureHandle(nullInput, "CreateFile(NUL)");
        try
        {
            return DuplicateInheritedHandle(nullInput, "stdin");
        }
        finally
        {
            CloseHandle(nullInput);
        }
    }

    private static IntPtr DuplicateInheritedHandle(IntPtr source, string label)
    {
        EnsureHandle(source, "GetStdHandle(" + label + ")");
        IntPtr duplicate;
        IntPtr current = GetCurrentProcess();
        if (!DuplicateHandle(
            current,
            source,
            current,
            out duplicate,
            0,
            true,
            DUPLICATE_SAME_ACCESS))
        {
            ThrowLastWin32("DuplicateHandle(" + label + ")");
        }
        return duplicate;
    }

    private static IntPtr BuildEnvironmentBlock(string[] keys, string[] values)
    {
        if (keys == null || values == null || keys.Length != values.Length)
        {
            throw new ArgumentException("invalid environment payload");
        }
        string[] entries = new string[keys.Length];
        for (int index = 0; index < keys.Length; index++)
        {
            if (String.IsNullOrEmpty(keys[index]) || keys[index].IndexOf('\0') >= 0
                || values[index] == null || values[index].IndexOf('\0') >= 0)
            {
                throw new ArgumentException("invalid environment entry");
            }
            entries[index] = keys[index] + "=" + values[index];
        }
        Array.Sort(entries, StringComparer.OrdinalIgnoreCase);
        string block = String.Join("\0", entries) + "\0\0";
        char[] characters = block.ToCharArray();
        IntPtr pointer = Marshal.AllocHGlobal(characters.Length * sizeof(char));
        Marshal.Copy(characters, 0, pointer, characters.Length);
        return pointer;
    }

    private static StringBuilder BuildCommandLine(string command, string[] arguments)
    {
        if (String.IsNullOrEmpty(command) || command.IndexOf('\0') >= 0)
        {
            throw new ArgumentException("invalid empty command");
        }
        StringBuilder line = new StringBuilder(QuoteArgument(command));
        if (arguments != null)
        {
            foreach (string argument in arguments)
            {
                if (argument == null || argument.IndexOf('\0') >= 0)
                {
                    throw new ArgumentException("invalid command argument");
                }
                line.Append(' ');
                line.Append(QuoteArgument(argument));
            }
        }
        return line;
    }

    private static string QuoteArgument(string argument)
    {
        if (argument.Length != 0
            && argument.IndexOfAny(new char[] { ' ', '\t', '\n', '\v', '"' }) < 0)
        {
            return argument;
        }

        StringBuilder quoted = new StringBuilder();
        quoted.Append('"');
        int backslashes = 0;
        foreach (char character in argument)
        {
            if (character == '\\')
            {
                backslashes++;
            }
            else if (character == '"')
            {
                quoted.Append('\\', backslashes * 2 + 1);
                quoted.Append('"');
                backslashes = 0;
            }
            else
            {
                quoted.Append('\\', backslashes);
                quoted.Append(character);
                backslashes = 0;
            }
        }
        quoted.Append('\\', backslashes * 2);
        quoted.Append('"');
        return quoted.ToString();
    }

    private static void EnsureHandle(IntPtr handle, string operation)
    {
        if (handle == IntPtr.Zero || handle == InvalidHandle)
        {
            ThrowLastWin32(operation);
        }
    }

    private static void CloseIfValid(IntPtr handle)
    {
        if (handle != IntPtr.Zero && handle != InvalidHandle) CloseHandle(handle);
    }

    private static void ThrowLastWin32(string operation)
    {
        throw new Win32Exception(Marshal.GetLastWin32Error(), operation + " failed");
    }
}
'@

try {
  $encodedPayload = [Console]::In.ReadToEnd()
  $payloadBytes = [Convert]::FromBase64String($encodedPayload)
  $payload = [Text.Encoding]::UTF8.GetString($payloadBytes) | ConvertFrom-Json

  $environmentKeys = [string[]]@($payload.environment | ForEach-Object { $_.key })
  $environmentValues = [string[]]@($payload.environment | ForEach-Object { $_.value })
  $null = Add-Type -TypeDefinition $launcherSource -Language CSharp
  $exitCode = [CtxWindowsJobLauncher]::Run(
    $ControlPipe,
    [string]$payload.command,
    [string[]]$payload.arguments,
    $environmentKeys,
    $environmentValues,
    [string]$payload.currentDirectory
  )
  [Environment]::Exit($exitCode)
}
catch {
  Send-LauncherFailure -Message ("PowerShell launcher failure: " + $_.Exception.Message)
  [Environment]::Exit(127)
}
