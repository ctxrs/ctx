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
    private const string ProcessScopeLauncherArgument = "__ctx_sdk_process_scope_v1";
    private const string ProcessScopeLauncherEnvironment = "CTX_SDK_PROCESS_SCOPE_LAUNCHER";
    private const int WindowsDrainFailureExit = 252;
    private const byte WindowsLauncherAck = 0x06;

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
        var launcher = FindProcessScopeLauncher(Config.CtxBinary, Config.Environment);
        if (launcher is null)
        {
            throw CaptureFailure(
                "process_scope",
                new PlatformNotSupportedException("local CLI process containment is unavailable"));
        }
        var usesWindowsLauncher = OperatingSystem.IsWindows();
        var startInfo = new ProcessStartInfo
        {
            FileName = launcher,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            RedirectStandardInput = usesWindowsLauncher,
            UseShellExecute = false
        };
        if (!string.IsNullOrWhiteSpace(Config.WorkingDirectory))
        {
            startInfo.WorkingDirectory = Config.WorkingDirectory;
        }
        startInfo.ArgumentList.Add(ProcessScopeLauncherArgument);
        startInfo.ArgumentList.Add("--");
        startInfo.ArgumentList.Add(Config.CtxBinary);
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
        using var processScope = OwnedProcessScope.Attach(
            process,
            !OperatingSystem.IsWindows());

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
        var captureFailureTask = FirstCaptureFailureAsync(stdoutTask, stderrTask);
        var exitTask = process.WaitForExitAsync();
        var cancellationSignal = Task.Delay(Timeout.InfiniteTimeSpan, linked.Token);

        var completed = await Task.WhenAny(exitTask, captureFailureTask, cancellationSignal).ConfigureAwait(false);
        if (completed == captureFailureTask)
        {
            var captureFailure = await captureFailureTask.ConfigureAwait(false);
            if (captureFailure is not null)
            {
                await AbortAsync(process, processScope, captureCancellation, captureTask).ConfigureAwait(false);
                throw CaptureError(captureFailure);
            }
            if (usesWindowsLauncher)
            {
                try
                {
                    await process.StandardInput.BaseStream
                        .WriteAsync(new[] { WindowsLauncherAck }, CancellationToken.None)
                        .ConfigureAwait(false);
                    await process.StandardInput.BaseStream.FlushAsync(CancellationToken.None).ConfigureAwait(false);
                    process.StandardInput.Close();
                }
                catch (IOException)
                {
                    // The launcher may have already reported a bounded drain failure.
                }
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
            var stdout = await stdoutTask.ConfigureAwait(false);
            var stderr = await stderrTask.ConfigureAwait(false);
            outText = StrictUtf8.GetString(stdout.Buffer, 0, stdout.Length);
            errText = StrictUtf8.GetString(stderr.Buffer, 0, stderr.Length);
        }
        catch (DecoderFallbackException error)
        {
            processScope.Terminate();
            throw CaptureFailure("utf8", error);
        }
        if (process.ExitCode != 0)
        {
            processScope.Terminate();
            if (usesWindowsLauncher && process.ExitCode == WindowsDrainFailureExit)
            {
                throw CaptureFailure("pipe", new IOException("a descendant retained a CLI output pipe"));
            }
            throw new CtxAgentHistoryCliException("ctx CLI command failed", command, process.ExitCode, outText, errText);
        }

        return new CommandResult(command, outText, errText, process.ExitCode);
    }

    private static async Task<Exception?> FirstCaptureFailureAsync(params Task<CapturedBytes>[] captures)
    {
        var pending = captures.ToList();
        while (pending.Count > 0)
        {
            var completed = await Task.WhenAny(pending).ConfigureAwait(false);
            pending.Remove(completed);
            try
            {
                _ = await completed.ConfigureAwait(false);
            }
            catch (Exception error)
            {
                return error;
            }
        }
        return null;
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

    private static string? FindProcessScopeLauncher(
        string command,
        IReadOnlyDictionary<string, string?>? environment)
    {
        if (environment is not null
            && environment.TryGetValue(ProcessScopeLauncherEnvironment, out var configured)
            && !string.IsNullOrWhiteSpace(configured))
        {
            return configured;
        }
        var inherited = Environment.GetEnvironmentVariable(ProcessScopeLauncherEnvironment);
        if (!string.IsNullOrWhiteSpace(inherited))
        {
            return inherited;
        }
        var name = Path.GetFileName(command).ToLowerInvariant();
        return name is "ctx" or "ctx.exe"
            || name.StartsWith("ctx-", StringComparison.Ordinal)
            || name.StartsWith("ctx_", StringComparison.Ordinal)
            ? command
            : null;
    }

    private static async Task<CapturedBytes> ReadBoundedAsync(
        Stream stream,
        string name,
        int capBytes,
        CancellationToken cancellationToken)
    {
        var output = new byte[capBytes];
        var captured = 0;
        var buffer = new byte[ReadBufferBytes];
        try
        {
            while (true)
            {
                var read = await stream.ReadAsync(buffer.AsMemory(), cancellationToken).ConfigureAwait(false);
                if (read == 0)
                {
                    return new CapturedBytes(output, captured);
                }
                var remaining = capBytes - captured;
                if (read > remaining)
                {
                    if (remaining > 0)
                    {
                        Buffer.BlockCopy(buffer, 0, output, captured, remaining);
                    }
                    throw new CaptureLimitException(name, capBytes);
                }
                Buffer.BlockCopy(buffer, 0, output, captured, read);
                captured += read;
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

    private sealed record CapturedBytes(byte[] Buffer, int Length);

    private sealed class OwnedProcessScope : IDisposable
    {
        private readonly Process process;
        private readonly int processId;
        private readonly bool ownsUnixProcessGroup;
        private bool terminated;

        private OwnedProcessScope(Process process, bool ownsUnixProcessGroup)
        {
            this.process = process;
            processId = process.Id;
            this.ownsUnixProcessGroup = ownsUnixProcessGroup;
        }

        public static OwnedProcessScope Attach(Process process, bool ownsUnixProcessGroup)
        {
            return new OwnedProcessScope(process, ownsUnixProcessGroup);
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
                _ = Kill(-processId, 15);
                Thread.Sleep(DrainGrace);
                _ = Kill(-processId, 9);
            }
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

        [DllImport("libc", EntryPoint = "kill", SetLastError = true)]
        private static extern int Kill(int processId, int signal);

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
