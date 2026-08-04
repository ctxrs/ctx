using System.ComponentModel;
using System.Diagnostics;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Local-only agent-history-v1 transport backed by the ctx CLI.</summary>
public sealed class LocalCliAdapter : IAgentHistoryTransport
{
    private const string AnalyticsEnabledEnvironment = "CTX_ANALYTICS_ENABLED";
    private const int MaxRetainedStdoutBytes = 64 * 1024 * 1024;
    private const int MaxRetainedStderrBytes = 16 * 1024 * 1024;
    private static readonly TimeSpan ForceCleanupTimeout = TimeSpan.FromSeconds(2);
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
            EnsureNoDuplicateObjectMembers(stdout);
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

    private static void EnsureNoDuplicateObjectMembers(string json)
    {
        var reader = new Utf8JsonReader(Encoding.UTF8.GetBytes(json));
        var objectMembers = new Stack<HashSet<string>>(capacity: 8);
        while (reader.Read())
        {
            switch (reader.TokenType)
            {
                case JsonTokenType.StartObject:
                    objectMembers.Push(new HashSet<string>(StringComparer.Ordinal));
                    break;
                case JsonTokenType.PropertyName:
                    var member = reader.GetString()
                        ?? throw new JsonException("JSON object member name was null");
                    if (objectMembers.Count == 0 || !objectMembers.Peek().Add(member))
                    {
                        throw new JsonException($"duplicate JSON object member {member}");
                    }
                    break;
                case JsonTokenType.EndObject:
                    if (objectMembers.Count == 0)
                    {
                        throw new JsonException("unexpected JSON object end");
                    }
                    objectMembers.Pop();
                    break;
            }
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
        startInfo.Environment[AnalyticsEnabledEnvironment] = "false";

        ProcessTree process;
        try
        {
            process = ProcessTree.Start(startInfo);
        }
        catch (Exception ex) when (ex is Win32Exception or PlatformNotSupportedException)
        {
            throw new CtxAgentHistoryCliException("failed to execute ctx CLI", command, -1, "", ex.Message, innerException: ex);
        }
        await using var processOwner = process;

        var stdoutTask = ReadBoundedAsync(process.StandardOutput, MaxRetainedStdoutBytes);
        var stderrTask = ReadBoundedAsync(process.StandardError, MaxRetainedStderrBytes);
        var exitTask = process.WaitForExitAsync(CancellationToken.None);

        using var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        if (Config.Timeout is { } timeout)
        {
            linked.CancelAfter(timeout);
        }

        try
        {
            await Task.WhenAll(exitTask, stdoutTask, stderrTask)
                .WaitAsync(linked.Token)
                .ConfigureAwait(false);
        }
        catch (OperationCanceledException ex)
        {
            var (timeoutStdout, timeoutStderr) = await StopAndCaptureAsync(
                    process,
                    stdoutTask,
                    stderrTask)
                .ConfigureAwait(false);
            var stdout = Encoding.UTF8.GetString(timeoutStdout.Bytes);
            var stderr = Encoding.UTF8.GetString(timeoutStderr.Bytes);
            throw new CtxAgentHistoryCliException("ctx CLI timed out", command, -1, stdout, stderr, code: "timeout", retryable: true, innerException: ex);
        }
        catch
        {
            await StopAndCaptureAsync(process, stdoutTask, stderrTask).ConfigureAwait(false);
            throw;
        }

        var stdoutCapture = await stdoutTask.ConfigureAwait(false);
        var stderrCapture = await stderrTask.ConfigureAwait(false);
        var stdoutBytes = stdoutCapture.Bytes;
        var stderrBytes = stderrCapture.Bytes;
        var errText = Encoding.UTF8.GetString(stderrBytes);
        if (process.ExitCode != 0)
        {
            var outText = Encoding.UTF8.GetString(stdoutBytes);
            throw new CtxAgentHistoryCliException("ctx CLI command failed", command, process.ExitCode, outText, errText);
        }
        if (stdoutCapture.Truncated)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx command stdout exceeded the retained output limit",
                new JsonObject
                {
                    ["stream"] = "stdout",
                    ["maximumBytes"] = MaxRetainedStdoutBytes
                });
        }

        string strictOutText;
        try
        {
            strictOutText = StrictUtf8.GetString(stdoutBytes);
        }
        catch (DecoderFallbackException ex)
        {
            throw new CtxAgentHistoryProtocolException(
                "ctx returned invalid UTF-8 on stdout",
                new JsonObject
                {
                    ["command"] = JsonHelpers.ToJsonArray(command),
                    ["stdout"] = Encoding.UTF8.GetString(stdoutBytes),
                    ["stderr"] = errText
                },
                ex);
        }

        return new CommandResult(command, strictOutText, errText, process.ExitCode);
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

    private static async Task<OutputCapture> ReadBoundedAsync(Stream stream, int maximumBytes)
    {
        using var retained = new MemoryStream();
        var buffer = new byte[8 * 1024];
        var truncated = false;
        try
        {
            while (true)
            {
                var count = await stream.ReadAsync(buffer).ConfigureAwait(false);
                if (count == 0)
                {
                    break;
                }

                var remaining = maximumBytes - checked((int)retained.Length);
                var keep = Math.Min(count, Math.Max(remaining, 0));
                if (keep > 0)
                {
                    retained.Write(buffer, 0, keep);
                }
                if (keep < count)
                {
                    truncated = true;
                }
            }
        }
        catch (IOException)
        {
            // Match the prior adapter behavior: a closed/broken pipe contributes what was read.
        }
        catch (ObjectDisposedException)
        {
            // Cleanup may close a pipe to unblock a reader after forceful termination.
        }
        return new OutputCapture(retained.ToArray(), truncated);
    }

    private static async Task<(OutputCapture Stdout, OutputCapture Stderr)> StopAndCaptureAsync(
        ProcessTree process,
        Task<OutputCapture> stdoutTask,
        Task<OutputCapture> stderrTask)
    {
        process.TryTerminateTree();
        using var cleanup = new CancellationTokenSource(ForceCleanupTimeout);
        try
        {
            await process.WaitForExitAsync(CancellationToken.None)
                .WaitAsync(cleanup.Token)
                .ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            process.TryTerminateTree();
        }

        try
        {
            await Task.WhenAll(stdoutTask, stderrTask, process.WaitForTreeExitAsync(cleanup.Token))
                .WaitAsync(cleanup.Token)
                .ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            TryClose(process.StandardOutput);
            TryClose(process.StandardError);
        }

        process.MarkCleanupWaitCompleted();
        return (CompletedCapture(stdoutTask), CompletedCapture(stderrTask));
    }

    private static OutputCapture CompletedCapture(Task<OutputCapture> task)
    {
        return task.IsCompletedSuccessfully ? task.Result : OutputCapture.Empty;
    }

    private static void TryClose(Stream stream)
    {
        try
        {
            stream.Close();
        }
        catch
        {
            // Closing is only a final reader-unblock fallback after forceful termination.
        }
    }

    private sealed record OutputCapture(byte[] Bytes, bool Truncated)
    {
        internal static readonly OutputCapture Empty = new([], false);
    }

    private sealed record CommandResult(IReadOnlyList<string> Command, string Stdout, string Stderr, int ExitCode);
}
