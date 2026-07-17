package rs.ctx.agenthistory;

import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/** agent-history-v1 transport backed by a local ctx CLI. */
public final class LocalCliAdapter implements AgentHistoryTransport {
    private static final int STDOUT_CAP_BYTES = 2 * 1024 * 1024;
    private static final int STDERR_CAP_BYTES = 256 * 1024;
    private static final int READ_BUFFER_BYTES = 64 * 1024;
    private static final long POLL_MILLIS = 5;
    private static final long DRAIN_GRACE_MILLIS = 100;
    private static final long TEARDOWN_MILLIS = 1_000;
    private static final int WINDOWS_DRAIN_FAILURE_EXIT = 252;
    private static final byte WINDOWS_LAUNCHER_ACK = 0x06;
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
        CommandRequest request = new CommandRequest(
                command,
                new ArrayList<>(args),
                config.cwd(),
                env,
                config.timeoutMillis());
        try {
            CommandResult result = runner.run(request);
            int stdoutBytes = utf8Length(result.stdout());
            int stderrBytes = utf8Length(result.stderr());
            if (stdoutBytes > STDOUT_CAP_BYTES) {
                throw new CaptureLimitException("stdout", STDOUT_CAP_BYTES);
            }
            if (stderrBytes > STDERR_CAP_BYTES) {
                throw new CaptureLimitException("stderr", STDERR_CAP_BYTES);
            }
            return result;
        } catch (CaptureLimitException cause) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("command", command);
            details.put("args", new ArrayList<>(args));
            details.put("stream", cause.stream);
            details.put("capBytes", Integer.valueOf(cause.capBytes));
            throw new CtxAgentHistoryException(
                    "capture_limit",
                    "ctx CLI " + cause.stream + " exceeded its capture limit",
                    false,
                    details,
                    cause);
        } catch (CaptureFailureException cause) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("command", command);
            details.put("args", new ArrayList<>(args));
            details.put("stream", cause.stream);
            throw new CtxAgentHistoryException(
                    "capture_failure",
                    "ctx CLI output capture failed",
                    false,
                    details,
                    cause);
        } catch (CaptureCancelledException cause) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("command", command);
            details.put("args", new ArrayList<>(args));
            throw new CtxAgentHistoryException(
                    "cancelled",
                    "ctx CLI command was cancelled",
                    false,
                    details,
                    cause);
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

    private static int utf8Length(String value) {
        long bytes = 0;
        for (int index = 0; index < value.length(); ) {
            int codePoint = value.codePointAt(index);
            if (codePoint <= 0x7f) {
                bytes += 1;
            } else if (codePoint <= 0x7ff) {
                bytes += 2;
            } else if (codePoint <= 0xffff) {
                bytes += 3;
            } else {
                bytes += 4;
            }
            if (bytes > Integer.MAX_VALUE) return Integer.MAX_VALUE;
            index += Character.charCount(codePoint);
        }
        return (int) bytes;
    }

    private static final class ProcessCommandRunner implements CommandRunner {
        @Override
        public CommandResult run(CommandRequest request) throws Exception {
            ProcessLaunch launch = ProcessLaunch.forRequest(request);
            ProcessBuilder builder = new ProcessBuilder(launch.command);
            if (request.cwd() != null) {
                builder.directory(request.cwd().toFile());
            }
            builder.environment().putAll(request.env());

            Process process = builder.start();
            OwnedProcessScope scope = new OwnedProcessScope(process, launch.ownsProcessGroup);
            if (!launch.windowsHandshake) close(process.getOutputStream());
            ExecutorService readers = Executors.newFixedThreadPool(2, runnable -> {
                Thread thread = new Thread(runnable, "ctx-sdk-cli-capture");
                thread.setDaemon(true);
                return thread;
            });
            Future<CapturedBytes> stdout = readers.submit(
                    () -> readBounded(process.getInputStream(), "stdout", STDOUT_CAP_BYTES));
            Future<CapturedBytes> stderr = readers.submit(
                    () -> readBounded(process.getErrorStream(), "stderr", STDERR_CAP_BYTES));
            try {
                long deadlineNanos = System.nanoTime()
                        + TimeUnit.MILLISECONDS.toNanos(Math.max(1L, request.timeoutMillis()));
                boolean timedOut = false;
                boolean launcherAcknowledged = false;
                Throwable captureFailure = null;
                while (process.isAlive()) {
                    captureFailure = completedFailure(stdout);
                    if (captureFailure == null) captureFailure = completedFailure(stderr);
                    if (captureFailure != null) break;
                    if (launch.windowsHandshake
                            && !launcherAcknowledged
                            && stdout.isDone()
                            && stderr.isDone()) {
                        try {
                            acknowledgeLauncher(process);
                        } catch (IOException error) {
                            scope.terminate();
                            close(process.getInputStream());
                            close(process.getErrorStream());
                            finishReaders(readers, scope);
                            throw new CaptureFailureException("pipe", error);
                        }
                        launcherAcknowledged = true;
                    }
                    if (System.nanoTime() >= deadlineNanos) {
                        timedOut = true;
                        break;
                    }
                    Thread.sleep(POLL_MILLIS);
                }
                if (timedOut || captureFailure != null) {
                    scope.terminate();
                    close(process.getInputStream());
                    close(process.getErrorStream());
                    finishReaders(readers, scope);
                    if (captureFailure != null) rethrowCapture(captureFailure);
                    return new CommandResult("", "ctx command timed out", -1);
                }

                CapturedBytes stdoutBytes;
                CapturedBytes stderrBytes;
                String stdoutText;
                String stderrText;
                try {
                    stdoutBytes = stdout.get(DRAIN_GRACE_MILLIS, TimeUnit.MILLISECONDS);
                    stderrBytes = stderr.get(DRAIN_GRACE_MILLIS, TimeUnit.MILLISECONDS);
                    if (process.exitValue() != 0) scope.terminate();
                    stdoutText = decodeUtf8(stdoutBytes, "stdout");
                    stderrText = decodeUtf8(stderrBytes, "stderr");
                } catch (CaptureFailureException error) {
                    scope.terminate();
                    throw error;
                } catch (TimeoutException error) {
                    scope.terminate();
                    throw new CaptureFailureException("pipe", error);
                } catch (ExecutionException error) {
                    scope.terminate();
                    rethrowCapture(error.getCause());
                    throw new CaptureFailureException("pipe", error);
                } finally {
                    close(process.getInputStream());
                    close(process.getErrorStream());
                    finishReaders(readers, scope);
                }
                if (launch.windowsHandshake && process.exitValue() == WINDOWS_DRAIN_FAILURE_EXIT) {
                    throw new CaptureFailureException(
                            "pipe", new IOException("a descendant retained a CLI output pipe"));
                }
                return new CommandResult(
                        stdoutText,
                        stderrText,
                        process.exitValue());
            } catch (InterruptedException error) {
                Thread.interrupted();
                scope.terminate();
                close(process.getInputStream());
                close(process.getErrorStream());
                close(process.getOutputStream());
                finishReadersAfterInterruption(readers, scope);
                Thread.currentThread().interrupt();
                throw new CaptureCancelledException(error);
            }
        }

        private static void acknowledgeLauncher(Process process) throws IOException {
            process.getOutputStream().write(WINDOWS_LAUNCHER_ACK);
            process.getOutputStream().flush();
            close(process.getOutputStream());
        }

        private static CapturedBytes readBounded(InputStream stream, String name, int capBytes)
                throws IOException {
            byte[] output = new byte[capBytes];
            byte[] buffer = new byte[READ_BUFFER_BYTES];
            int captured = 0;
            int read;
            while ((read = stream.read(buffer)) >= 0) {
                if (read == 0) continue;
                int remaining = capBytes - captured;
                if (read > remaining) {
                    if (remaining > 0) {
                        System.arraycopy(buffer, 0, output, captured, remaining);
                    }
                    throw new CaptureLimitException(name, capBytes);
                }
                System.arraycopy(buffer, 0, output, captured, read);
                captured += read;
            }
            return new CapturedBytes(output, captured);
        }

        private static Throwable completedFailure(Future<CapturedBytes> future)
                throws InterruptedException {
            if (!future.isDone()) return null;
            try {
                future.get();
                return null;
            } catch (ExecutionException error) {
                return error.getCause();
            } catch (java.util.concurrent.CancellationException error) {
                return error;
            }
        }

        private static void rethrowCapture(Throwable cause) throws Exception {
            if (cause instanceof CaptureLimitException) throw (CaptureLimitException) cause;
            if (cause instanceof Exception) throw new CaptureFailureException("pipe", (Exception) cause);
            throw new CaptureFailureException("pipe", new RuntimeException(cause));
        }

        private static String decodeUtf8(CapturedBytes data, String stream) throws CaptureFailureException {
            try {
                return StandardCharsets.UTF_8.newDecoder()
                        .onMalformedInput(CodingErrorAction.REPORT)
                        .onUnmappableCharacter(CodingErrorAction.REPORT)
                        .decode(ByteBuffer.wrap(data.bytes, 0, data.length))
                        .toString();
            } catch (CharacterCodingException error) {
                throw new CaptureFailureException(stream, error);
            }
        }

        private static void close(InputStream stream) {
            try {
                stream.close();
            } catch (IOException ignored) {
                // The process scope has already been terminated.
            }
        }

        private static void close(java.io.OutputStream stream) {
            try {
                stream.close();
            } catch (IOException ignored) {
                // The process scope has already been terminated.
            }
        }

        private static void finishReaders(ExecutorService readers, OwnedProcessScope scope)
                throws InterruptedException {
            scope.terminate();
            readers.shutdownNow();
            try {
                readers.awaitTermination(TEARDOWN_MILLIS, TimeUnit.MILLISECONDS);
            } finally {
                scope.close();
            }
        }

        private static void finishReadersAfterInterruption(
                ExecutorService readers, OwnedProcessScope scope) {
            scope.terminate();
            readers.shutdownNow();
            long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(TEARDOWN_MILLIS);
            while (!readers.isTerminated() && System.nanoTime() < deadline) {
                try {
                    long remaining = Math.max(1L, deadline - System.nanoTime());
                    readers.awaitTermination(remaining, TimeUnit.NANOSECONDS);
                } catch (InterruptedException ignored) {
                    Thread.interrupted();
                }
            }
            scope.close();
        }

        private static final class CapturedBytes {
            private final byte[] bytes;
            private final int length;

            CapturedBytes(byte[] bytes, int length) {
                this.bytes = bytes;
                this.length = length;
            }
        }

        private static final class ProcessLaunch {
            private static final String LAUNCHER_ARG = "__ctx_sdk_process_scope_v1";
            private static final String LAUNCHER_ENV = "CTX_SDK_PROCESS_SCOPE_LAUNCHER";
            private final List<String> command;
            private final boolean ownsProcessGroup;
            private final boolean windowsHandshake;

            private ProcessLaunch(
                    List<String> command,
                    boolean ownsProcessGroup,
                    boolean windowsHandshake) {
                this.command = command;
                this.ownsProcessGroup = ownsProcessGroup;
                this.windowsHandshake = windowsHandshake;
            }

            static ProcessLaunch forRequest(CommandRequest request)
                    throws CaptureFailureException {
                List<String> command = new ArrayList<>();
                String launcher = launcherPath(request);
                if (launcher == null) {
                    throw new CaptureFailureException(
                            "process_scope",
                            new IOException("local CLI process containment is unavailable"));
                }
                command.add(launcher);
                command.add(LAUNCHER_ARG);
                command.add("--");
                command.add(request.command());
                command.addAll(request.args());
                boolean windowsHandshake = launcher != null && isWindows();
                boolean ownsProcessGroup = !isWindows();
                return new ProcessLaunch(command, ownsProcessGroup, windowsHandshake);
            }

            private static String launcherPath(CommandRequest request) {
                String override = request.env().get(LAUNCHER_ENV);
                if (override == null || override.isEmpty()) override = System.getenv(LAUNCHER_ENV);
                if (override != null && !override.isEmpty()) return override;
                String name = new File(request.command()).getName().toLowerCase(java.util.Locale.ROOT);
                if (name.equals("ctx")
                        || name.equals("ctx.exe")
                        || name.startsWith("ctx-")
                        || name.startsWith("ctx_")) {
                    return request.command();
                }
                return null;
            }

            private static boolean isWindows() {
                return System.getProperty("os.name", "")
                        .toLowerCase(java.util.Locale.ROOT)
                        .contains("win");
            }

        }
    }

    private static final class OwnedProcessScope implements AutoCloseable {
        private final Process process;
        private final boolean ownsProcessGroup;
        private boolean terminated;

        OwnedProcessScope(Process process, boolean ownsProcessGroup) {
            this.process = process;
            this.ownsProcessGroup = ownsProcessGroup;
        }

        synchronized void terminate() {
            if (terminated) return;
            terminated = true;
            if (ownsProcessGroup) {
                signalProcessGroup("-TERM");
                try {
                    Thread.sleep(DRAIN_GRACE_MILLIS);
                } catch (InterruptedException ignored) {
                    Thread.currentThread().interrupt();
                }
                signalProcessGroup("-KILL");
                try {
                    process.waitFor(TEARDOWN_MILLIS / 2, TimeUnit.MILLISECONDS);
                } catch (InterruptedException ignored) {
                    Thread.currentThread().interrupt();
                }
                return;
            }
            if (process.isAlive()) process.destroyForcibly();
            try {
                process.waitFor(TEARDOWN_MILLIS, TimeUnit.MILLISECONDS);
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
        }

        private void signalProcessGroup(String signal) {
            Process signaler = null;
            try {
                signaler = new ProcessBuilder("/bin/kill", signal, "-" + process.pid()).start();
                signaler.waitFor(DRAIN_GRACE_MILLIS, TimeUnit.MILLISECONDS);
            } catch (IOException ignored) {
                // Fall through to direct process destruction below.
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            } finally {
                if (signaler != null && signaler.isAlive()) signaler.destroyForcibly();
            }
            if (process.isAlive() && "-KILL".equals(signal)) process.destroyForcibly();
        }

        @Override
        public void close() {
            terminate();
        }
    }

    private static final class CaptureLimitException extends IOException {
        private final String stream;
        private final int capBytes;

        CaptureLimitException(String stream, int capBytes) {
            super("ctx CLI " + stream + " exceeded " + capBytes + " bytes");
            this.stream = stream;
            this.capBytes = capBytes;
        }
    }

    private static final class CaptureFailureException extends IOException {
        private final String stream;

        CaptureFailureException(String stream, Throwable cause) {
            super("ctx CLI " + stream + " capture failed", cause);
            this.stream = stream;
        }
    }

    private static final class CaptureCancelledException extends IOException {
        CaptureCancelledException(InterruptedException cause) {
            super("ctx CLI command was cancelled", cause);
        }
    }
}
