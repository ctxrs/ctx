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
