using System.ComponentModel;
using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Local-only agent-history-v1 transport backed by the ctx CLI.</summary>
public sealed class LocalCliAdapter : IAgentHistoryTransport
{
    private const string AnalyticsEnabledEnvironment = "CTX_ANALYTICS_ENABLED";

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
        startInfo.Environment[AnalyticsEnabledEnvironment] = "false";

        using var process = new Process { StartInfo = startInfo };
        try
        {
            process.Start();
        }
        catch (Win32Exception ex)
        {
            throw new CtxAgentHistoryCliException("failed to execute ctx CLI", command, -1, "", ex.Message, innerException: ex);
        }

        var stdoutTask = process.StandardOutput.ReadToEndAsync();
        var stderrTask = process.StandardError.ReadToEndAsync();

        using var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        if (Config.Timeout is { } timeout)
        {
            linked.CancelAfter(timeout);
        }

        try
        {
            await process.WaitForExitAsync(linked.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException ex)
        {
            TryKill(process);
            var stdout = await SafeReadAsync(stdoutTask).ConfigureAwait(false);
            var stderr = await SafeReadAsync(stderrTask).ConfigureAwait(false);
            throw new CtxAgentHistoryCliException("ctx CLI timed out", command, -1, stdout, stderr, code: "timeout", retryable: true, innerException: ex);
        }

        var outText = await stdoutTask.ConfigureAwait(false);
        var errText = await stderrTask.ConfigureAwait(false);
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

    private static async Task<string> SafeReadAsync(Task<string> task)
    {
        try
        {
            return await task.ConfigureAwait(false);
        }
        catch
        {
            return "";
        }
    }

    private static void TryKill(Process process)
    {
        try
        {
            if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
            }
        }
        catch
        {
            // The process may have exited between cancellation and kill.
        }
    }

    private sealed record CommandResult(IReadOnlyList<string> Command, string Stdout, string Stderr, int ExitCode);
}
