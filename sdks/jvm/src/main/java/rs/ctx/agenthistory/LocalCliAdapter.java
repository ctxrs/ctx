package rs.ctx.agenthistory;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/** agent-history-v1 transport backed by a local ctx CLI. */
public final class LocalCliAdapter implements AgentHistoryTransport {
    private static final String ANALYTICS_ENABLED_ENV = "CTX_ANALYTICS_ENABLED";

    private final LocalCliConfig config;
    private final CommandRunner runner;

    public LocalCliAdapter() {
        this(LocalCliConfig.builder().build());
    }

    public LocalCliAdapter(LocalCliConfig config) {
        this.config = config == null ? LocalCliConfig.builder().build() : config;
        this.runner = this.config.runner() == null ? new ProcessCommandRunner() : this.config.runner();
    }

    public LocalCliConfig config() {
        return config;
    }

    @Override
    public String name() {
        return "local-cli";
    }

    @Override
    public String execute(AgentHistoryOperation operation) {
        CommandResult result = run(operation.args());
        if (result.exitCode() != 0) {
            throw cliError("ctx " + String.join(" ", operation.args()) + " failed", operation.args(), result, null);
        }
        String stdout = result.stdout().trim();
        if (stdout.isEmpty()) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("operation", operation.name());
            details.put("args", operation.args());
            throw new CtxAgentHistoryException.Protocol("ctx command returned empty stdout", details, null);
        }
        return stdout;
    }

    @Override
    public String ctxVersion() {
        try {
            CommandResult result = run(java.util.Collections.singletonList("--version"));
            if (result.exitCode() != 0) {
                return null;
            }
            return result.stdout().trim();
        } catch (CtxAgentHistoryException error) {
            return null;
        }
    }

    private CommandResult run(List<String> args) {
        String command = config.ctxPath();
        if (command == null || command.trim().isEmpty()) {
            throw new CtxAgentHistoryException.Validation("local ctx CLI path is empty");
        }
        Map<String, String> env = config.env();
        if (config.dataRoot() != null && !config.dataRoot().isEmpty()) {
            env.put("CTX_DATA_ROOT", config.dataRoot());
        }
        env.put(ANALYTICS_ENABLED_ENV, "false");
        CommandRequest request = new CommandRequest(
                command,
                new ArrayList<>(args),
                config.cwd(),
                env,
                config.timeoutMillis());
        try {
            return runner.run(request);
        } catch (Exception cause) {
            throw cliError("ctx command could not be executed", args, new CommandResult("", "", -1), cause);
        }
    }

    private CtxAgentHistoryException.Cli cliError(
            String message,
            List<String> args,
            CommandResult result,
            Throwable cause) {
        String stderr = result.stderr();
        if (!stderr.isEmpty()) {
            message = message + ": " + firstLine(stderr);
        } else if (cause != null && cause.getMessage() != null) {
            message = message + ": " + cause.getMessage();
        }
        boolean timeout = result.exitCode() == -1 && stderr.toLowerCase(java.util.Locale.ROOT).contains("timed out");
        return new CtxAgentHistoryException.Cli(
                timeout ? "timeout" : "adapter_error",
                message,
                timeout,
                config.ctxPath(),
                args,
                result.exitCode(),
                result.stdout(),
                stderr,
                cause);
    }

    private static String firstLine(String value) {
        int newline = value.indexOf('\n');
        int carriage = value.indexOf('\r');
        int end = -1;
        if (newline >= 0 && carriage >= 0) {
            end = Math.min(newline, carriage);
        } else if (newline >= 0) {
            end = newline;
        } else if (carriage >= 0) {
            end = carriage;
        }
        return end < 0 ? value : value.substring(0, end);
    }

    private static final class ProcessCommandRunner implements CommandRunner {
        @Override
        public CommandResult run(CommandRequest request) throws Exception {
            List<String> command = new ArrayList<>();
            command.add(request.command());
            command.addAll(request.args());
            ProcessBuilder builder = new ProcessBuilder(command);
            if (request.cwd() != null) {
                builder.directory(request.cwd().toFile());
            }
            builder.environment().putAll(request.env());

            Process process = builder.start();
            CompletableFuture<String> stdout = read(process.getInputStream());
            CompletableFuture<String> stderr = read(process.getErrorStream());
            boolean completed = process.waitFor(
                    Duration.ofMillis(request.timeoutMillis()).toMillis(),
                    TimeUnit.MILLISECONDS);
            if (!completed) {
                process.destroyForcibly();
                return new CommandResult(stdout.getNow(""), "ctx command timed out", -1);
            }
            return new CommandResult(stdout.get(), stderr.get(), process.exitValue());
        }

        private static CompletableFuture<String> read(InputStream stream) {
            return CompletableFuture.supplyAsync(() -> {
                try {
                    byte[] data = stream.readAllBytes();
                    return new String(data, StandardCharsets.UTF_8);
                } catch (IOException error) {
                    return "";
                }
            });
        }
    }
}
