package rs.ctx.agenthistory;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicInteger;

/** agent-history-v1 transport backed by a local ctx CLI. */
public final class LocalCliAdapter implements AgentHistoryTransport {
    private static final String ANALYTICS_ENABLED_ENV = "CTX_ANALYTICS_ENABLED";
    static final int MAX_RETAINED_STDOUT_BYTES = 64 * 1024 * 1024;
    static final int MAX_RETAINED_STDERR_BYTES = 16 * 1024 * 1024;
    private static final long PROCESS_POLL_MILLIS = 10;
    private static final long TERMINATION_GRACE_MILLIS = 100;
    private static final long POST_FORCE_SETTLE_MILLIS = 20;
    private static final long FORCE_CLEANUP_MILLIS = 2_000;

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
        } catch (CtxAgentHistoryException error) {
            throw error;
        } catch (Exception cause) {
            throw cliError("ctx command could not be executed", args, new CommandResult("", "", -1), cause);
        }
    }

    static String decodeUtf8Output(byte[] data) {
        try {
            return StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(data))
                    .toString();
        } catch (CharacterCodingException cause) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("stream", "stdout");
            throw new CtxAgentHistoryException.Protocol(
                    "ctx command returned invalid UTF-8 on stdout",
                    details,
                    cause);
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
            ProcessTreeScope scope = ProcessTreeScope.start(command, request.cwd(), request.env());
            Process process = scope.process();
            ExecutorService readers = newReaderExecutor();
            Future<CapturedOutput> stdout = readers.submit(() -> read(
                    process.getInputStream(), MAX_RETAINED_STDOUT_BYTES));
            Future<CapturedOutput> stderr = readers.submit(() -> read(
                    process.getErrorStream(), MAX_RETAINED_STDERR_BYTES));
            Map<Long, ProcessHandle> descendants = new LinkedHashMap<>();
            long deadline = deadlineNanos(request.timeoutMillis());
            boolean cleaned = false;
            try {
                while (process.isAlive() || !stdout.isDone() || !stderr.isDone()) {
                    rememberDescendants(process, descendants);
                    if (remainingNanos(deadline) <= 0) {
                        terminateAndReap(scope, descendants, stdout, stderr);
                        cleaned = true;
                        return new CommandResult(
                                decodeLeniently(completedCapture(stdout).bytes),
                                "ctx command timed out",
                                -1);
                    }
                    TimeUnit.NANOSECONDS.sleep(Math.min(
                            remainingNanos(deadline),
                            TimeUnit.MILLISECONDS.toNanos(PROCESS_POLL_MILLIS)));
                }

                CapturedOutput stdoutOutput = stdout.get();
                CapturedOutput stderrOutput = stderr.get();
                int exitCode = process.exitValue();
                if (exitCode == 0 && stdoutOutput.truncated) {
                    Map<String, Object> details = new LinkedHashMap<>();
                    details.put("stream", "stdout");
                    details.put("maximumBytes", Integer.valueOf(MAX_RETAINED_STDOUT_BYTES));
                    throw new CtxAgentHistoryException.Protocol(
                            "ctx command stdout exceeded the retained output limit",
                            details,
                            null);
                }
                return new CommandResult(
                        exitCode == 0
                                ? decodeUtf8Output(stdoutOutput.bytes)
                                : decodeLeniently(stdoutOutput.bytes),
                        decodeLeniently(stderrOutput.bytes),
                        exitCode);
            } catch (InterruptedException error) {
                terminateAndReap(scope, descendants, stdout, stderr);
                cleaned = true;
                Thread.currentThread().interrupt();
                throw error;
            } finally {
                if (!cleaned) {
                    terminateAndReap(scope, descendants, stdout, stderr);
                }
                readers.shutdownNow();
                scope.close();
            }
        }

        private static ExecutorService newReaderExecutor() {
            AtomicInteger sequence = new AtomicInteger();
            return Executors.newFixedThreadPool(2, task -> {
                Thread thread = new Thread(task, "ctx-jvm-cli-reader-" + sequence.incrementAndGet());
                thread.setDaemon(true);
                return thread;
            });
        }

        private static CapturedOutput read(InputStream stream, int maximumBytes) {
            ByteArrayOutputStream retained = new ByteArrayOutputStream();
            byte[] buffer = new byte[8 * 1024];
            boolean truncated = false;
            try {
                while (true) {
                    int count = stream.read(buffer);
                    if (count < 0) {
                        break;
                    }
                    int remaining = maximumBytes - retained.size();
                    int keep = Math.min(count, Math.max(remaining, 0));
                    if (keep > 0) {
                        retained.write(buffer, 0, keep);
                    }
                    if (keep < count) {
                        truncated = true;
                    }
                }
            } catch (IOException ignored) {
                // Match the prior adapter behavior: a closed/broken pipe contributes what was read.
            }
            return new CapturedOutput(retained.toByteArray(), truncated);
        }

        private static long deadlineNanos(long timeoutMillis) {
            long now = System.nanoTime();
            if (timeoutMillis <= 0) {
                return now;
            }
            long duration = TimeUnit.MILLISECONDS.toNanos(timeoutMillis);
            return duration >= Long.MAX_VALUE - now ? Long.MAX_VALUE : now + duration;
        }

        private static long remainingNanos(long deadline) {
            return Math.max(0, deadline - System.nanoTime());
        }

        private static String decodeLeniently(byte[] bytes) {
            return new String(bytes, StandardCharsets.UTF_8);
        }

        private static CapturedOutput completedCapture(Future<CapturedOutput> future) {
            if (!future.isDone()) {
                return CapturedOutput.EMPTY;
            }
            try {
                return future.get();
            } catch (Exception ignored) {
                return CapturedOutput.EMPTY;
            }
        }

        private static void rememberDescendants(Process process, Map<Long, ProcessHandle> descendants) {
            List<ProcessHandle> roots = new ArrayList<>();
            roots.add(process.toHandle());
            roots.addAll(descendants.values());
            for (ProcessHandle root : roots) {
                try {
                    root.descendants().forEach(handle -> descendants.putIfAbsent(
                            Long.valueOf(handle.pid()), handle));
                } catch (RuntimeException ignored) {
                    // ProcessHandle tree enumeration is best effort on restricted runtimes.
                }
            }
        }

        private static void terminateAndReap(
                ProcessTreeScope scope,
                Map<Long, ProcessHandle> descendants,
                Future<CapturedOutput> stdout,
                Future<CapturedOutput> stderr) {
            Process process = scope.process();
            rememberDescendants(process, descendants);
            boolean scopedTermination = scope.terminate(false);
            destroyDescendants(descendants, false);
            if (process.isAlive()) {
                process.destroy();
            }
            waitForTermination(
                    process,
                    descendants,
                    TERMINATION_GRACE_MILLIS,
                    scopedTermination ? TERMINATION_GRACE_MILLIS : 0);

            rememberDescendants(process, descendants);
            boolean scopedForce = scope.terminate(true);
            destroyDescendants(descendants, true);
            if (process.isAlive()) {
                process.destroyForcibly();
            }
            long cleanupDeadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(FORCE_CLEANUP_MILLIS);
            long minimumWaitDeadline = scopedForce
                    ? System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(POST_FORCE_SETTLE_MILLIS)
                    : 0;
            waitForTerminationUntil(process, descendants, cleanupDeadline, minimumWaitDeadline);

            try {
                process.getInputStream().close();
            } catch (IOException ignored) {
                // Closing is only a final reader-unblock fallback after forceful termination.
            }
            try {
                process.getErrorStream().close();
            } catch (IOException ignored) {
                // Closing is only a final reader-unblock fallback after forceful termination.
            }

            awaitReader(stdout, cleanupDeadline);
            awaitReader(stderr, cleanupDeadline);
        }

        private static void destroyDescendants(Map<Long, ProcessHandle> descendants, boolean forcibly) {
            List<ProcessHandle> handles = new ArrayList<>(descendants.values());
            handles.sort(Comparator.comparingLong(ProcessHandle::pid).reversed());
            for (ProcessHandle handle : handles) {
                if (!handle.isAlive()) {
                    continue;
                }
                try {
                    if (forcibly) {
                        handle.destroyForcibly();
                    } else {
                        handle.destroy();
                    }
                } catch (RuntimeException ignored) {
                    // A concurrent exit is equivalent to successful cleanup.
                }
            }
        }

        private static void waitForTermination(
                Process process,
                Map<Long, ProcessHandle> descendants,
                long timeoutMillis,
                long minimumWaitMillis) {
            long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMillis);
            long minimumWaitDeadline = minimumWaitMillis <= 0
                    ? 0
                    : System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(minimumWaitMillis);
            waitForTerminationUntil(process, descendants, deadline, minimumWaitDeadline);
        }

        private static void waitForTerminationUntil(
                Process process,
                Map<Long, ProcessHandle> descendants,
                long deadline,
                long minimumWaitDeadline) {
            while (remainingNanos(deadline) > 0) {
                rememberDescendants(process, descendants);
                boolean descendantAlive = descendants.values().stream().anyMatch(ProcessHandle::isAlive);
                if (!process.isAlive()
                        && !descendantAlive
                        && (minimumWaitDeadline == 0 || remainingNanos(minimumWaitDeadline) == 0)) {
                    try {
                        process.waitFor(0, TimeUnit.MILLISECONDS);
                    } catch (InterruptedException error) {
                        Thread.currentThread().interrupt();
                    }
                    return;
                }
                try {
                    TimeUnit.NANOSECONDS.sleep(Math.min(
                            remainingNanos(deadline),
                            TimeUnit.MILLISECONDS.toNanos(PROCESS_POLL_MILLIS)));
                } catch (InterruptedException error) {
                    Thread.currentThread().interrupt();
                    return;
                }
            }
        }

        private static void awaitReader(Future<CapturedOutput> reader, long deadline) {
            long remaining = remainingNanos(deadline);
            if (remaining <= 0) {
                return;
            }
            try {
                reader.get(remaining, TimeUnit.NANOSECONDS);
            } catch (TimeoutException ignored) {
                reader.cancel(true);
            } catch (Exception ignored) {
                // Reader failures are intentionally suppressed during cleanup.
            }
        }

        private static final class CapturedOutput {
            private static final CapturedOutput EMPTY = new CapturedOutput(new byte[0], false);

            private final byte[] bytes;
            private final boolean truncated;

            CapturedOutput(byte[] bytes, boolean truncated) {
                this.bytes = bytes;
                this.truncated = truncated;
            }
        }
    }
}
