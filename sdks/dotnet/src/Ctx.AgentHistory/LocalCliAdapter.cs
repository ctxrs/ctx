using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Local-only agent-history-v1 transport backed by the ctx CLI.</summary>
public sealed class LocalCliAdapter : IAgentHistoryTransport
{
    private const int StdoutCapBytes = 2 * 1024 * 1024;
    private const int StderrCapBytes = 256 * 1024;
    private const int ReadBufferBytes = 64 * 1024;
    private static readonly TimeSpan DrainGrace = TimeSpan.FromMilliseconds(100);
    private static readonly TimeSpan TeardownLimit = TimeSpan.FromSeconds(1);
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);

    public LocalCliAdapter(LocalAgentHistoryConfig? config = null)
    {
        Config = config ?? new LocalAgentHistoryConfig();
    }

    public string Name => "local-cli";
    public LocalAgentHistoryConfig Config { get; }

    public JsonObject Backend(JsonObject? raw = null)
    {
        var dataRoot = Config.DataRoot
            ?? JsonHelpers.GetString(raw, "data_root")
            ?? JsonHelpers.GetString(raw, "dataRoot");

        var backend = new JsonObject { ["kind"] = "local" };
        if (!string.IsNullOrWhiteSpace(dataRoot))
        {
            backend["dataRoot"] = dataRoot;
        }
        return backend;
    }

    public async Task<JsonObject> ExecuteJsonAsync(
        string operation,
        IReadOnlyList<string> args,
        CancellationToken cancellationToken = default)
    {
        var result = await ExecuteAsync(args, cancellationToken).ConfigureAwait(false);
        var stdout = result.Stdout.Trim();
        if (stdout.Length == 0)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx returned no JSON on stdout",
                new JsonObject
                {
                    ["operation"] = operation,
                    ["command"] = JsonHelpers.ToJsonArray(result.Command),
                    ["stderr"] = result.Stderr
                });
        }

        try
        {
            var node = JsonNode.Parse(stdout);
            if (node is not JsonObject obj)
            {
                throw new CtxAgentHistoryProtocolException(
                    "ctx returned a non-object JSON value",
                    new JsonObject
                    {
                        ["operation"] = operation,
                        ["command"] = JsonHelpers.ToJsonArray(result.Command),
                        ["stdout"] = result.Stdout
                    });
            }
            return obj;
        }
        catch (JsonException ex)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx returned invalid JSON",
                new JsonObject
                {
                    ["operation"] = operation,
                    ["command"] = JsonHelpers.ToJsonArray(result.Command),
                    ["stdout"] = result.Stdout,
                    ["stderr"] = result.Stderr
                },
                ex);
        }
    }

    public async Task<string?> GetCtxVersionAsync(CancellationToken cancellationToken = default)
    {
        try
        {
            var result = await ExecuteAsync(["--version"], cancellationToken).ConfigureAwait(false);
            return result.Stdout.Trim();
        }
        catch (CtxAgentHistoryException)
        {
            return null;
        }
    }

    private async Task<CommandResult> ExecuteAsync(IReadOnlyList<string> args, CancellationToken cancellationToken)
    {
        if (string.IsNullOrWhiteSpace(Config.CtxBinary))
        {
            throw new CtxAgentHistoryValidationException("local ctx CLI path is empty");
        }

        var command = BuildCommand(args);
        var startInfo = new ProcessStartInfo
        {
            FileName = Config.CtxBinary,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false
        };
        if (!string.IsNullOrWhiteSpace(Config.WorkingDirectory))
        {
            startInfo.WorkingDirectory = Config.WorkingDirectory;
        }
        foreach (var arg in command.Skip(1))
        {
            startInfo.ArgumentList.Add(arg);
        }
        if (Config.Environment is not null)
        {
            foreach (var pair in Config.Environment)
            {
                if (pair.Value is null)
                {
                    startInfo.Environment.Remove(pair.Key);
                }
                else
                {
                    startInfo.Environment[pair.Key] = pair.Value;
                }
            }
        }

        using var process = new Process { StartInfo = startInfo };
        try
        {
            process.Start();
        }
        catch (Win32Exception ex)
        {
            throw new CtxAgentHistoryCliException("failed to execute ctx CLI", command, -1, "", ex.Message, innerException: ex);
        }
        using var processScope = OwnedProcessScope.Attach(process);

        using var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        if (Config.Timeout is { } timeout)
        {
            linked.CancelAfter(timeout);
        }
        using var captureCancellation = new CancellationTokenSource();
        var stdoutTask = ReadBoundedAsync(
            process.StandardOutput.BaseStream,
            "stdout",
            StdoutCapBytes,
            captureCancellation.Token);
        var stderrTask = ReadBoundedAsync(
            process.StandardError.BaseStream,
            "stderr",
            StderrCapBytes,
            captureCancellation.Token);
        var captureTask = Task.WhenAll(stdoutTask, stderrTask);
        var exitTask = process.WaitForExitAsync();
        var cancellationSignal = Task.Delay(Timeout.InfiniteTimeSpan, linked.Token);

        var completed = await Task.WhenAny(exitTask, captureTask, cancellationSignal).ConfigureAwait(false);
        if (completed == captureTask)
        {
            try
            {
                await captureTask.ConfigureAwait(false);
            }
            catch (Exception error)
            {
                await AbortAsync(process, processScope, captureCancellation, captureTask).ConfigureAwait(false);
                throw CaptureError(error);
            }
            completed = await Task.WhenAny(exitTask, cancellationSignal).ConfigureAwait(false);
        }

        if (completed == cancellationSignal)
        {
            await AbortAsync(process, processScope, captureCancellation, captureTask).ConfigureAwait(false);
            var cancelled = cancellationToken.IsCancellationRequested;
            throw new CtxAgentHistoryCliException(
                cancelled ? "ctx CLI command was cancelled" : "ctx CLI timed out",
                command,
                -1,
                "",
                "",
                code: cancelled ? "cancelled" : "timeout",
                retryable: !cancelled,
                innerException: new OperationCanceledException(linked.Token));
        }

        try
        {
            await captureTask.WaitAsync(DrainGrace).ConfigureAwait(false);
        }
        catch (TimeoutException error)
        {
            await AbortAsync(process, processScope, captureCancellation, captureTask).ConfigureAwait(false);
            throw CaptureFailure("pipe", error);
        }
        catch (Exception error)
        {
            await AbortAsync(process, processScope, captureCancellation, captureTask).ConfigureAwait(false);
            throw CaptureError(error);
        }

        string outText;
        string errText;
        try
        {
            outText = StrictUtf8.GetString(await stdoutTask.ConfigureAwait(false));
            errText = StrictUtf8.GetString(await stderrTask.ConfigureAwait(false));
        }
        catch (DecoderFallbackException error)
        {
            throw CaptureFailure("utf8", error);
        }
        if (process.ExitCode != 0)
        {
            throw new CtxAgentHistoryCliException("ctx CLI command failed", command, process.ExitCode, outText, errText);
        }

        return new CommandResult(command, outText, errText, process.ExitCode);
    }

    private IReadOnlyList<string> BuildCommand(IReadOnlyList<string> args)
    {
        var command = new List<string> { Config.CtxBinary };
        if (!string.IsNullOrWhiteSpace(Config.DataRoot))
        {
            command.Add("--data-root");
            command.Add(Config.DataRoot);
        }
        command.AddRange(args);
        return command;
    }

    private static async Task<byte[]> ReadBoundedAsync(
        Stream stream,
        string name,
        int capBytes,
        CancellationToken cancellationToken)
    {
        using var output = new MemoryStream(Math.Min(capBytes, ReadBufferBytes));
        var buffer = new byte[ReadBufferBytes];
        try
        {
            while (true)
            {
                var read = await stream.ReadAsync(buffer.AsMemory(), cancellationToken).ConfigureAwait(false);
                if (read == 0)
                {
                    return output.ToArray();
                }
                var remaining = capBytes - checked((int)output.Length);
                if (read > remaining)
                {
                    if (remaining > 0)
                    {
                        output.Write(buffer, 0, remaining);
                    }
                    throw new CaptureLimitException(name, capBytes);
                }
                output.Write(buffer, 0, read);
            }
        }
        catch (CaptureLimitException)
        {
            throw;
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch (Exception error)
        {
            throw new CaptureFailureException(name, error);
        }
    }

    private static async Task AbortAsync(
        Process process,
        OwnedProcessScope processScope,
        CancellationTokenSource captureCancellation,
        Task captureTask)
    {
        processScope.Terminate();
        captureCancellation.Cancel();
        try
        {
            await captureTask.WaitAsync(TeardownLimit).ConfigureAwait(false);
        }
        catch
        {
            // Teardown is deliberately bounded; the owned process tree is already dead.
        }
        try
        {
            await process.WaitForExitAsync().WaitAsync(TeardownLimit).ConfigureAwait(false);
        }
        catch
        {
            // Never extend the caller's deadline while reaping a terminated child.
        }
    }

    private static CtxAgentHistoryException CaptureError(Exception error)
    {
        var cause = error is AggregateException aggregate ? aggregate.GetBaseException() : error;
        return cause switch
        {
            CaptureLimitException limit => new CtxAgentHistoryException(
                $"ctx CLI {limit.Stream} exceeded its capture limit",
                "capture_limit",
                details: new JsonObject
                {
                    ["stream"] = limit.Stream,
                    ["capBytes"] = limit.CapBytes
                },
                innerException: limit),
            CaptureFailureException failure => CaptureFailure(failure.Stream, failure),
            _ => CaptureFailure("pipe", cause)
        };
    }

    private static CtxAgentHistoryException CaptureFailure(string stream, Exception error) => new(
        "ctx CLI output capture failed",
        "capture_failure",
        details: new JsonObject { ["stream"] = stream },
        innerException: error);

    private sealed record CommandResult(IReadOnlyList<string> Command, string Stdout, string Stderr, int ExitCode);

    private sealed class OwnedProcessScope : IDisposable
    {
        private const uint JobObjectExtendedLimitInformationClass = 9;
        private const uint JobObjectLimitKillOnJobClose = 0x00002000;
        private readonly Process process;
        private readonly int processId;
        private readonly bool ownsUnixProcessGroup;
        private IntPtr jobHandle;
        private bool terminated;

        private OwnedProcessScope(Process process, bool ownsUnixProcessGroup, IntPtr jobHandle)
        {
            this.process = process;
            processId = process.Id;
            this.ownsUnixProcessGroup = ownsUnixProcessGroup;
            this.jobHandle = jobHandle;
        }

        public static OwnedProcessScope Attach(Process process)
        {
            if (OperatingSystem.IsWindows())
            {
                return new OwnedProcessScope(process, false, CreateWindowsJob(process));
            }
            return new OwnedProcessScope(process, SetProcessGroup(process.Id, process.Id) == 0, IntPtr.Zero);
        }

        public void Terminate()
        {
            if (terminated)
            {
                return;
            }
            terminated = true;
            if (ownsUnixProcessGroup)
            {
                if (Kill(-processId, 15) == 0)
                {
                    Thread.Sleep(DrainGrace);
                    _ = Kill(-processId, 9);
                }
            }
            CloseJob();
            try
            {
                if (!process.HasExited)
                {
                    process.Kill(entireProcessTree: true);
                }
            }
            catch
            {
                // The process scope may already have exited.
            }
        }

        public void Dispose()
        {
            Terminate();
        }

        private void CloseJob()
        {
            if (jobHandle == IntPtr.Zero)
            {
                return;
            }
            _ = CloseHandle(jobHandle);
            jobHandle = IntPtr.Zero;
        }

        private static IntPtr CreateWindowsJob(Process process)
        {
            var job = CreateJobObject(IntPtr.Zero, null);
            if (job == IntPtr.Zero)
            {
                return IntPtr.Zero;
            }
            var limits = new JobObjectExtendedLimitInformation
            {
                BasicLimitInformation = new JobObjectBasicLimitInformation
                {
                    LimitFlags = JobObjectLimitKillOnJobClose
                }
            };
            var size = Marshal.SizeOf<JobObjectExtendedLimitInformation>();
            var pointer = Marshal.AllocHGlobal(size);
            try
            {
                Marshal.StructureToPtr(limits, pointer, false);
                if (!SetInformationJobObject(job, JobObjectExtendedLimitInformationClass, pointer, (uint)size)
                    || !AssignProcessToJobObject(job, process.Handle))
                {
                    _ = CloseHandle(job);
                    return IntPtr.Zero;
                }
                return job;
            }
            catch
            {
                _ = CloseHandle(job);
                return IntPtr.Zero;
            }
            finally
            {
                Marshal.FreeHGlobal(pointer);
            }
        }

        [DllImport("libc", EntryPoint = "setpgid", SetLastError = true)]
        private static extern int SetProcessGroup(int processId, int processGroupId);

        [DllImport("libc", EntryPoint = "kill", SetLastError = true)]
        private static extern int Kill(int processId, int signal);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr CreateJobObject(IntPtr jobAttributes, string? name);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetInformationJobObject(
            IntPtr job,
            uint informationClass,
            IntPtr information,
            uint informationLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool CloseHandle(IntPtr handle);

        [StructLayout(LayoutKind.Sequential)]
        private struct JobObjectBasicLimitInformation
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
        private struct IoCounters
        {
            public ulong ReadOperationCount;
            public ulong WriteOperationCount;
            public ulong OtherOperationCount;
            public ulong ReadTransferCount;
            public ulong WriteTransferCount;
            public ulong OtherTransferCount;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct JobObjectExtendedLimitInformation
        {
            public JobObjectBasicLimitInformation BasicLimitInformation;
            public IoCounters IoInfo;
            public UIntPtr ProcessMemoryLimit;
            public UIntPtr JobMemoryLimit;
            public UIntPtr PeakProcessMemoryUsed;
            public UIntPtr PeakJobMemoryUsed;
        }
    }

    private sealed class CaptureLimitException(string stream, int capBytes) : IOException
    {
        public string Stream { get; } = stream;
        public int CapBytes { get; } = capBytes;
    }

    private sealed class CaptureFailureException(string stream, Exception innerException)
        : IOException("ctx CLI output capture failed", innerException)
    {
        public string Stream { get; } = stream;
    }
}
