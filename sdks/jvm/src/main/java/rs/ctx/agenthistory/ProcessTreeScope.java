package rs.ctx.agenthistory;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.TimeUnit;

/** Platform process scope used to contain every process launched for one CLI call. */
final class ProcessTreeScope implements AutoCloseable {
    private static final long CONTROL_PROCESS_TIMEOUT_MILLIS = 500;
    private static final String BASH_GROUP_WRAPPER =
            "scope_pid_file=$1\n"
                    + "shift\n"
                    + "scope_child=\n"
                    + "terminate_scope() {\n"
                    + "  trap - TERM INT HUP\n"
                    + "  if [ -n \"$scope_child\" ]; then\n"
                    + "    kill -TERM -- \"-$scope_child\" 2>/dev/null || true\n"
                    + "  fi\n"
                    + "  exit 143\n"
                    + "}\n"
                    + "trap terminate_scope TERM INT HUP\n"
                    + "set -m\n"
                    + "\"$@\" &\n"
                    + "scope_child=$!\n"
                    + "printf '%s\\n' \"$scope_child\" > \"$scope_pid_file\"\n"
                    + "set +m\n"
                    + "scope_status=0\n"
                    + "wait \"$scope_child\" || scope_status=$?\n"
                    + "trap - TERM INT HUP\n"
                    + "exit \"$scope_status\"\n";

    private enum Kind {
        POSIX_GROUP,
        BASH_GROUP
    }

    private final Process process;
    private final Kind kind;
    private final Long processGroupId;
    private final Path processGroupFile;

    private ProcessTreeScope(Process process, Kind kind, Long processGroupId, Path processGroupFile) {
        this.process = process;
        this.kind = kind;
        this.processGroupId = processGroupId;
        this.processGroupFile = processGroupFile;
    }

    static ProcessTreeScope start(
            List<String> command,
            Path cwd,
            Map<String, String> environment) throws IOException {
        if (isWindows()) {
            throw new IOException(
                    "the ctx JVM local adapter requires race-free process-tree ownership; "
                            + "Windows is not supported");
        }

        // Never launch an unresolved command directly: ProcessBuilder can resolve a bare command
        // against the JVM's parent PATH even when the requested child PATH cannot resolve it.
        if (!canResolveExecutable(command.get(0), cwd, environment)) {
            throw new IOException(
                    "ctx CLI executable is unavailable or not executable: " + command.get(0));
        }

        Path setsid = firstExecutable("/usr/bin/setsid", "/bin/setsid", "/usr/local/bin/setsid");
        if (setsid != null) {
            List<String> scoped = new ArrayList<>();
            scoped.add(setsid.toString());
            scoped.addAll(command);
            ProcessTreeScope launched = start(scoped, cwd, environment, Kind.POSIX_GROUP, null, null);
            return new ProcessTreeScope(
                    launched.process,
                    Kind.POSIX_GROUP,
                    Long.valueOf(launched.process.pid()),
                    null);
        }

        Path bash = firstExecutable("/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash");
        if (bash != null) {
            Path groupFile = Files.createTempFile("ctx-jvm-process-group-", ".pid");
            try {
                List<String> scoped = new ArrayList<>();
                scoped.add(bash.toString());
                scoped.add("--noprofile");
                scoped.add("--norc");
                scoped.add("-c");
                scoped.add(BASH_GROUP_WRAPPER);
                scoped.add("ctx-jvm-process-scope");
                scoped.add(groupFile.toString());
                scoped.addAll(command);
                return start(scoped, cwd, environment, Kind.BASH_GROUP, null, groupFile);
            } catch (IOException | RuntimeException error) {
                Files.deleteIfExists(groupFile);
                throw error;
            }
        }

        // Java 11 exposes no portable process-group creation API. Starting directly here would
        // allow a child to escape ProcessHandle enumeration before cancellation, so fail closed.
        throw new IOException(
                "the ctx JVM local adapter requires setsid or bash to own the CLI process tree");
    }

    private static ProcessTreeScope start(
            List<String> command,
            Path cwd,
            Map<String, String> environment,
            Kind kind,
            Long groupId,
            Path groupFile) throws IOException {
        ProcessBuilder builder = new ProcessBuilder(command);
        if (cwd != null) {
            builder.directory(cwd.toFile());
        }
        builder.environment().putAll(environment);
        Process process = builder.start();
        return new ProcessTreeScope(process, kind, groupId, groupFile);
    }

    Process process() {
        return process;
    }

    /** Returns true when the platform accepted a tree-wide termination request. */
    boolean terminate(boolean forcibly) {
        if (kind == Kind.POSIX_GROUP || kind == Kind.BASH_GROUP) {
            Long groupId = readProcessGroupId();
            return groupId != null && signalPosixProcessGroup(groupId.longValue(), forcibly);
        }
        return false;
    }

    private Long readProcessGroupId() {
        if (processGroupId != null) {
            return processGroupId;
        }
        if (processGroupFile == null) {
            return null;
        }
        try {
            String value = Files.readString(processGroupFile, StandardCharsets.US_ASCII).trim();
            if (value.isEmpty()) {
                return null;
            }
            long parsed = Long.parseLong(value);
            return parsed > 1 ? Long.valueOf(parsed) : null;
        } catch (IOException | NumberFormatException ignored) {
            return null;
        }
    }

    private static boolean signalPosixProcessGroup(long processGroup, boolean forcibly) {
        String signal = forcibly ? "KILL" : "TERM";
        return runControlProcess(List.of(
                "/bin/sh",
                "-c",
                "kill -" + signal + " -\"$1\" 2>/dev/null",
                "ctx-jvm-process-scope-kill",
                Long.toString(processGroup)));
    }

    private static boolean runControlProcess(List<String> command) {
        Process control = null;
        try {
            control = new ProcessBuilder(command)
                    .redirectInput(ProcessBuilder.Redirect.PIPE)
                    .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                    .redirectError(ProcessBuilder.Redirect.DISCARD)
                    .start();
            control.getOutputStream().close();
            if (!control.waitFor(CONTROL_PROCESS_TIMEOUT_MILLIS, TimeUnit.MILLISECONDS)) {
                control.destroyForcibly();
                control.waitFor(CONTROL_PROCESS_TIMEOUT_MILLIS, TimeUnit.MILLISECONDS);
                return false;
            }
            return control.exitValue() == 0;
        } catch (IOException ignored) {
            return false;
        } catch (InterruptedException error) {
            if (control != null) {
                control.destroyForcibly();
            }
            Thread.currentThread().interrupt();
            return false;
        }
    }

    private static boolean canResolveExecutable(
            String command,
            Path cwd,
            Map<String, String> environment) {
        if (command.indexOf('/') >= 0) {
            Path path = Paths.get(command);
            if (!path.isAbsolute()) {
                Path base = cwd == null ? Paths.get("").toAbsolutePath() : cwd;
                path = base.resolve(path);
            }
            return Files.isRegularFile(path) && Files.isExecutable(path);
        }

        String pathValue = environment.get("PATH");
        if (pathValue == null) {
            pathValue = System.getenv("PATH");
        }
        if (pathValue == null) {
            return false;
        }
        for (String entry : pathValue.split(java.io.File.pathSeparator, -1)) {
            Path base = cwd == null ? Paths.get("").toAbsolutePath() : cwd;
            Path directory = entry.isEmpty() ? base : Paths.get(entry);
            if (!directory.isAbsolute()) {
                directory = base.resolve(directory);
            }
            Path candidate = directory.resolve(command);
            if (Files.isRegularFile(candidate) && Files.isExecutable(candidate)) {
                return true;
            }
        }
        return false;
    }

    private static Path firstExecutable(String... candidates) {
        for (String candidate : candidates) {
            Path path = Paths.get(candidate);
            if (Files.isRegularFile(path) && Files.isExecutable(path)) {
                return path;
            }
        }
        return null;
    }

    private static boolean isWindows() {
        return System.getProperty("os.name", "").toLowerCase(Locale.ROOT).contains("win");
    }

    @Override
    public void close() {
        if (processGroupFile != null) {
            try {
                Files.deleteIfExists(processGroupFile);
            } catch (IOException ignored) {
                processGroupFile.toFile().deleteOnExit();
            }
        }
    }
}
