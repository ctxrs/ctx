package rs.ctx.agenthistory;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class AgentHistoryClientTest {
    private static final String PROCESS_FIXTURE_MODE = "CTX_MCP289_JVM_PROCESS_FIXTURE_MODE";
    private static final String PROCESS_FIXTURE_ROOT_PID = "CTX_MCP289_JVM_PROCESS_FIXTURE_ROOT_PID";
    private static final String PROCESS_FIXTURE_DESCENDANT_PID = "CTX_MCP289_JVM_PROCESS_FIXTURE_DESCENDANT_PID";
    private static final String PROCESS_FIXTURE_COMPLETION = "CTX_MCP289_JVM_PROCESS_FIXTURE_COMPLETION";

    public static void main(String[] args) throws Exception {
        wrapsRawStatusAsTypedEnvelope();
        normalizesSetupJsonAsInitStatus();
        rejectsStatusCountersOutsideExactCrossSDKDomain();
        rejectsMalformedJsonGrammar();
        acceptsCanonicalSearchFixture();
        exposesOptionalMcpToolCallMetadata();
        rejectsRawMcpToolCallDuplicateMembers();
        strictlyDecodesSpawnedProcessStdoutUtf8();
        camelizesSearchRetrievalJson();
        decodesAllCanonicalFixturesThroughTypedResponses();
        normalizesRawShowResponses();
        preservesLegitimateNestedSourceSemantics();
        buildsSearchCommand();
        searchContentScopeValuesAreClosed();
        searchForwardsContentScopeOnce();
        searchRejectsContentScopeEventTypeConflictBeforeTransport();
        localCliForcesAnalyticsOffAfterAmbientAndUserEnvironment();
        localCliPreservesLaunchFailureContract();
        localCliDoesNotLaunchThroughTheParentPath();
        localCliDrainsLargeOutputWithinTheRetentionBound();
        localCliBoundsOutputWhileContinuingToDrain();
        localCliAcceptsValidOutputAboveLegacyRetentionLimit();
        localCliRejectsOversizeSuccessfulOutputAsProtocolFailure();
        localCliWaitsForPipeEofAfterSuccessfulRootExit();
        posixProcessScopeOwnsRootExitedClosedPipeDescendantWithoutPolling();
        localCliSuccessOwnsClosedPipeDescendantAfterRootExit();
        localCliDeadlineOwnsPipeDescendantsAndForcesCleanup();
        searchRequiresIntent();
        hostedIsExplicitlyUnsupported();
    }

    private static void localCliForcesAnalyticsOffAfterAmbientAndUserEnvironment() {
        assertEquals("true", System.getenv("CTX_ANALYTICS_ENABLED"));
        CommandRequest[] captured = new CommandRequest[1];
        LocalCliConfig config = LocalCliConfig.builder()
                .env("CTX_ANALYTICS_ENABLED", "true")
                .runner(request -> {
                    captured[0] = request;
                    return new CommandResult("{}", "", 0);
                })
                .build();

        new LocalCliAdapter(config).execute(
                new AgentHistoryOperation("status", java.util.Arrays.asList("status", "--format=json")));

        assertEquals("true", config.env().get("CTX_ANALYTICS_ENABLED"));
        assertEquals("false", captured[0].env().get("CTX_ANALYTICS_ENABLED"));
    }

    private static void localCliPreservesLaunchFailureContract() {
        Path missing = Paths.get(
                System.getProperty("java.io.tmpdir"),
                "ctx-mcp289-jvm-missing-" + System.nanoTime());
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(missing.toString())
                .timeoutMillis(1_000)
                .build());

        try {
            adapter.execute(new AgentHistoryOperation("status", List.of("status")));
            throw new AssertionError("expected missing CLI launch to fail");
        } catch (CtxAgentHistoryException.Cli error) {
            assertEquals("adapter_error", error.code());
            assertEquals(Integer.valueOf(-1), Integer.valueOf(error.exitCode()));
            assertEquals(Boolean.FALSE, Boolean.valueOf(error.retryable()));
        }
    }

    private static void localCliDoesNotLaunchThroughTheParentPath() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        Path emptyPath = Files.createTempDirectory("ctx-jvm-empty-path-");
        Path marker = emptyPath.resolve("escaped.marker");
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath("sh")
                .env("PATH", emptyPath.toString())
                .timeoutMillis(1_000)
                .build());

        try {
            adapter.execute(new AgentHistoryOperation(
                    "status",
                    List.of(
                            "-c",
                            "printf escaped > \"$1\"; printf '{}'",
                            "ctx-jvm-parent-path-regression",
                            marker.toString())));
            throw new AssertionError("expected unresolved child PATH command to fail closed");
        } catch (CtxAgentHistoryException.Cli error) {
            assertEquals("adapter_error", error.code());
            assertEquals(Integer.valueOf(-1), Integer.valueOf(error.exitCode()));
        } finally {
            boolean escaped = Files.exists(marker);
            Files.deleteIfExists(marker);
            Files.deleteIfExists(emptyPath);
            assertEquals(Boolean.FALSE, Boolean.valueOf(escaped));
        }
    }

    private static void localCliDrainsLargeOutputWithinTheRetentionBound() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(mcp289ProcessFixture().toString())
                .env(PROCESS_FIXTURE_MODE, "large-stdout")
                .timeoutMillis(5_000)
                .build());

        String output = adapter.execute(new AgentHistoryOperation("status", List.of("status")));

        assertEquals(Integer.valueOf(2 * 1024 * 1024), Integer.valueOf(output.length()));
    }

    private static void localCliBoundsOutputWhileContinuingToDrain() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(mcp289ProcessFixture().toString())
                .env(PROCESS_FIXTURE_MODE, "oversize-nonzero")
                .timeoutMillis(10_000)
                .build());

        long started = System.nanoTime();
        try {
            adapter.execute(new AgentHistoryOperation("status", List.of("status")));
            throw new AssertionError("expected oversized nonzero fixture to fail");
        } catch (CtxAgentHistoryException.Cli error) {
            assertEquals(Integer.valueOf(23), Integer.valueOf(error.exitCode()));
            assertEquals(
                    Integer.valueOf(16 * 1024 * 1024 + 64 * 1024),
                    Integer.valueOf(error.stdout().getBytes(StandardCharsets.UTF_8).length));
            assertEquals(
                    Integer.valueOf(LocalCliAdapter.MAX_RETAINED_STDERR_BYTES),
                    Integer.valueOf(error.stderr().getBytes(StandardCharsets.UTF_8).length));
        }
        assertElapsedLessThan(started, 8_000, "oversize output drain");
    }

    private static void localCliAcceptsValidOutputAboveLegacyRetentionLimit() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(mcp289ProcessFixture().toString())
                .env(PROCESS_FIXTURE_MODE, "valid-above-legacy-stdout-cap")
                .timeoutMillis(10_000)
                .build());

        String output = adapter.execute(new AgentHistoryOperation("status", List.of("status")));

        assertEquals(Boolean.TRUE, Boolean.valueOf(output.startsWith("{\"payload\":\"")));
        assertEquals(Boolean.TRUE, Boolean.valueOf(output.endsWith("\"}")));
        assertEquals(Boolean.TRUE, Boolean.valueOf(
                output.getBytes(StandardCharsets.UTF_8).length > 16 * 1024 * 1024));
        assertEquals(Boolean.TRUE, Boolean.valueOf(
                output.getBytes(StandardCharsets.UTF_8).length <= LocalCliAdapter.MAX_RETAINED_STDOUT_BYTES));
    }

    private static void localCliRejectsOversizeSuccessfulOutputAsProtocolFailure() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        Path completion = Files.createTempFile("ctx-mcp289-jvm-overflow-", ".complete");
        Files.delete(completion);
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(mcp289ProcessFixture().toString())
                .env(PROCESS_FIXTURE_MODE, "stdout-overflow-success")
                .env(PROCESS_FIXTURE_COMPLETION, completion.toString())
                .timeoutMillis(15_000)
                .build());

        try {
            adapter.execute(new AgentHistoryOperation("status", List.of("status")));
            throw new AssertionError("expected oversized successful output to fail closed");
        } catch (CtxAgentHistoryException.Protocol error) {
            assertEquals("decode_error", error.code());
            assertEquals(
                    Integer.valueOf(LocalCliAdapter.MAX_RETAINED_STDOUT_BYTES),
                    error.details().get("maximumBytes"));
        }
        assertEquals("completed", Files.readString(completion, StandardCharsets.UTF_8));
        completion.toFile().deleteOnExit();
    }

    private static void localCliSuccessOwnsClosedPipeDescendantAfterRootExit() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        Path descendantPid = Files.createTempFile("ctx-mcp289-jvm-closed-pipe-descendant-", ".pid");
        Files.delete(descendantPid);
        long descendant = -1;
        try {
            LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                    .ctxPath(mcp289ProcessFixture().toString())
                    .env(PROCESS_FIXTURE_MODE, "root-exits-closed-pipe-descendant")
                    .env(PROCESS_FIXTURE_DESCENDANT_PID, descendantPid.toString())
                    .timeoutMillis(2_000)
                    .build());

            String output = adapter.execute(new AgentHistoryOperation("status", List.of("status")));

            assertEquals("{}", output);
            descendant = readPid(descendantPid);
            assertProcessStops(descendant, "root-exited closed-pipe descendant");
        } finally {
            forceProcessStop(descendant);
        }
    }

    private static void posixProcessScopeOwnsRootExitedClosedPipeDescendantWithoutPolling() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        Path descendantPid = Files.createTempFile("ctx-mcp289-jvm-scope-descendant-", ".pid");
        Files.delete(descendantPid);
        Map<String, String> environment = new LinkedHashMap<>();
        environment.put(PROCESS_FIXTURE_MODE, "root-exits-closed-pipe-descendant");
        environment.put(PROCESS_FIXTURE_DESCENDANT_PID, descendantPid.toString());
        ProcessTreeScope scope = null;
        long descendant = -1;
        try {
            scope = ProcessTreeScope.start(
                    List.of(mcp289ProcessFixture().toString(), "status"),
                    null,
                    environment);
            Process root = scope.process();
            String stdout = new String(root.getInputStream().readAllBytes(), StandardCharsets.UTF_8).trim();
            if (!root.waitFor(1, java.util.concurrent.TimeUnit.SECONDS)) {
                throw new AssertionError("fixture root did not exit");
            }

            assertEquals(Integer.valueOf(0), Integer.valueOf(root.exitValue()));
            assertEquals("{}", stdout);
            descendant = readPid(descendantPid);
            assertEquals(Boolean.TRUE, Boolean.valueOf(
                    ProcessHandle.of(descendant).map(ProcessHandle::isAlive).orElse(false)));
            assertEquals(Boolean.TRUE, Boolean.valueOf(scope.terminate(true)));
            assertProcessStops(descendant, "directly scoped root-exited descendant");
        } finally {
            if (scope != null) {
                scope.terminate(true);
                scope.close();
            }
            forceProcessStop(descendant);
        }
    }

    private static void localCliWaitsForPipeEofAfterSuccessfulRootExit() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(mcp289ProcessFixture().toString())
                .env(PROCESS_FIXTURE_MODE, "short-lived-pipe-owner")
                .timeoutMillis(2_000)
                .build());

        long started = System.nanoTime();
        String output = adapter.execute(new AgentHistoryOperation("status", List.of("status")));

        assertEquals("{}", output);
        assertElapsedAtLeast(started, 150, "pipe EOF wait");
        assertElapsedLessThan(started, 1_500, "pipe EOF wait");
    }

    private static void localCliDeadlineOwnsPipeDescendantsAndForcesCleanup() throws Exception {
        if (java.io.File.separatorChar == '\\') {
            return;
        }
        Path rootPid = Files.createTempFile("ctx-mcp289-jvm-root-", ".pid");
        Path descendantPid = Files.createTempFile("ctx-mcp289-jvm-descendant-", ".pid");
        Files.delete(rootPid);
        Files.delete(descendantPid);
        long root = -1;
        long descendant = -1;
        try {
            LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                    .ctxPath(mcp289ProcessFixture().toString())
                    .env(PROCESS_FIXTURE_MODE, "detached-pipe-owner")
                    .env(PROCESS_FIXTURE_ROOT_PID, rootPid.toString())
                    .env(PROCESS_FIXTURE_DESCENDANT_PID, descendantPid.toString())
                    .timeoutMillis(900)
                    .build());

            long started = System.nanoTime();
            try {
                adapter.execute(new AgentHistoryOperation("status", List.of("status")));
                throw new AssertionError("expected pipe-owning descendant fixture to time out");
            } catch (CtxAgentHistoryException.Cli error) {
                assertEquals("timeout", error.code());
                assertEquals(Boolean.TRUE, Boolean.valueOf(error.retryable()));
            }
            assertElapsedLessThan(started, 4_000, "absolute process/EOF deadline");

            root = readPid(rootPid);
            descendant = readPid(descendantPid);
            assertProcessStops(root, "fixture root");
            assertProcessStops(descendant, "pipe-owning descendant");
        } finally {
            forceProcessStop(root);
            forceProcessStop(descendant);
        }
    }

    private static Path mcp289ProcessFixture() throws Exception {
        Path script = Files.createTempFile("ctx-mcp289-jvm-process-fixture-", ".sh");
        String source = "#!/bin/sh\n"
                + "case \"$" + PROCESS_FIXTURE_MODE + "\" in\n"
                + "  large-stdout)\n"
                + "    head -c 2097152 /dev/zero | tr '\\000' 'j'\n"
                + "    ;;\n"
                + "  oversize-nonzero)\n"
                + "    head -c 16842752 /dev/zero | tr '\\000' 'o'\n"
                + "    head -c 16842752 /dev/zero | tr '\\000' 'e' >&2\n"
                + "    exit 23\n"
                + "    ;;\n"
                + "  valid-above-legacy-stdout-cap)\n"
                + "    printf '{\"payload\":\"'\n"
                + "    head -c 17825792 /dev/zero | tr '\\000' 's'\n"
                + "    printf '\"}'\n"
                + "    ;;\n"
                + "  stdout-overflow-success)\n"
                + "    printf '{\"payload\":\"'\n"
                + "    head -c 67174400 /dev/zero | tr '\\000' 's'\n"
                + "    printf '\"}'\n"
                + "    printf completed > \"$" + PROCESS_FIXTURE_COMPLETION + "\"\n"
                + "    ;;\n"
                + "  root-exits-closed-pipe-descendant)\n"
                + "    sh -c 'exec >/dev/null 2>/dev/null; trap \"\" TERM; echo $$ > \"$1\"; exec sleep 300' sh \"$"
                + PROCESS_FIXTURE_DESCENDANT_PID + "\" &\n"
                + "    while [ ! -s \"$" + PROCESS_FIXTURE_DESCENDANT_PID + "\" ]; do :; done\n"
                + "    printf '{}\\n'\n"
                + "    exit 0\n"
                + "    ;;\n"
                + "  short-lived-pipe-owner)\n"
                + "    sh -c 'sleep 0.25' &\n"
                + "    printf '{}\\n'\n"
                + "    exit 0\n"
                + "    ;;\n"
                + "  detached-pipe-owner)\n"
                + "    echo $$ > \"$" + PROCESS_FIXTURE_ROOT_PID + "\"\n"
                + "    sh -c 'trap \"\" TERM; echo $$ > \"$1\"; exec sleep 300' sh \"$"
                + PROCESS_FIXTURE_DESCENDANT_PID + "\" &\n"
                + "    while [ ! -s \"$" + PROCESS_FIXTURE_DESCENDANT_PID + "\" ]; do sleep 0.01; done\n"
                + "    sleep 0.25\n"
                + "    exit 0\n"
                + "    ;;\n"
                + "  *) exit 64 ;;\n"
                + "esac\n";
        Files.write(script, source.getBytes(StandardCharsets.UTF_8));
        if (!script.toFile().setExecutable(true)) {
            throw new IllegalStateException("failed to make MCP #289 JVM process fixture executable");
        }
        script.toFile().deleteOnExit();
        return script;
    }

    private static long readPid(Path path) throws Exception {
        if (!Files.isRegularFile(path)) {
            throw new AssertionError("fixture did not write PID file " + path);
        }
        path.toFile().deleteOnExit();
        return Long.parseLong(Files.readString(path, StandardCharsets.UTF_8).trim());
    }

    private static void assertProcessStops(long pid, String label) throws Exception {
        long deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(2);
        while (System.nanoTime() < deadline) {
            if (ProcessHandle.of(pid).map(ProcessHandle::isAlive).orElse(false) == false) {
                return;
            }
            Thread.sleep(10);
        }
        throw new AssertionError(label + " remained alive: " + pid);
    }

    private static void forceProcessStop(long pid) throws Exception {
        if (pid <= 1) {
            return;
        }
        ProcessHandle.of(pid).ifPresent(ProcessHandle::destroyForcibly);
        long deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(2);
        while (System.nanoTime() < deadline
                && ProcessHandle.of(pid).map(ProcessHandle::isAlive).orElse(false)) {
            Thread.sleep(10);
        }
    }

    private static void assertElapsedLessThan(long started, long maximumMillis, String label) {
        long elapsed = java.util.concurrent.TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - started);
        if (elapsed >= maximumMillis) {
            throw new AssertionError(label + " exceeded " + maximumMillis + "ms: " + elapsed + "ms");
        }
    }

    private static void assertElapsedAtLeast(long started, long minimumMillis, String label) {
        long elapsed = java.util.concurrent.TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - started);
        if (elapsed < minimumMillis) {
            throw new AssertionError(label + " completed before " + minimumMillis + "ms: " + elapsed + "ms");
        }
    }

    private static void normalizesSetupJsonAsInitStatus() {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli",
                "{\"schema_version\":2,\"initialized\":true,\"data_root\":\"/tmp/ctx\","
                        + "\"indexed_items\":9007199254740991,\"indexed_sessions\":9007199254740991,"
                        + "\"indexed_events\":9007199254740991,\"indexed_sources\":9007199254740991,"
                        + "\"lexical\":{\"status\":\"ready\",\"generation_id\":\"gen-9\"},"
                        + "\"refresh\":{\"status\":\"ready\",\"generation_id\":\"gen-9\"},"
                        + "\"network_required\":false}"));

        InitResponse response = client.init(AgentHistoryOptions.init());

        assertEquals("init", response.operation());
        assertEquals(Boolean.TRUE, response.getStatus().getInitialized());
        assertEquals(Boolean.TRUE, response.getStatus().getLocalOnly());
        assertEquals(Long.valueOf(StatusRecord.MAX_SAFE_COUNTER), response.getStatus().getIndexedItems());
        assertEquals(Long.valueOf(StatusRecord.MAX_SAFE_COUNTER), response.getStatus().getIndexedSessions());
        assertEquals(Long.valueOf(StatusRecord.MAX_SAFE_COUNTER), response.getStatus().getIndexedEvents());
        assertEquals(Long.valueOf(StatusRecord.MAX_SAFE_COUNTER), response.getStatus().getIndexedSources());
    }

    private static void rejectsStatusCountersOutsideExactCrossSDKDomain() {
        for (String rejected : new String[] {
                "9007199254740993", "9223372036854775807", "1.00000000000000001", "1.5"
        }) {
            AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                    "local-cli",
                    "{\"initialized\":true,\"indexed_items\":" + rejected + "}"));
            assertProtocol(client::status);
        }
    }

    private static void rejectsMalformedJsonGrammar() {
        for (String invalid : new String[] {
                "{\"initialized\":true,\"future\":01}",
                "{\"initialized\":true,\"future\":-01}",
                "{\"initialized\":true,\"future\":\"line\nbreak\"}",
                "{\"initialized\":true,\"future\":\"control" + ((char) 1) + "character\"}"
        }) {
            AgentHistoryClient client = AgentHistoryClient.withTransport(
                    new FakeTransport("local-cli", invalid));
            assertProtocol(client::status);
        }

        assertEquals(
                "line\nbreak",
                Json.parseObject("{\"future\":\"line\\nbreak\"}").get("future"));
    }

    private static void wrapsRawStatusAsTypedEnvelope() {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"initialized\":true,\"indexed_items\":2,"
                        + "\"lexical\":{\"status\":\"ready\",\"generation_id\":\"gen-2\"},"
                        + "\"refresh\":{\"status\":\"ready\"},\"future_counter\":7}"));

        StatusResponse response = client.status();

        assertEquals("agent-history-v1", response.contractVersion());
        assertEquals(Integer.valueOf(1), Integer.valueOf(response.schemaVersion()));
        assertEquals("status", response.operation());
        assertEquals("local", response.getBackend().getKind());
        assertEquals(Boolean.TRUE, response.getStatus().getInitialized());
        assertEquals(Boolean.TRUE, response.getStatus().getLocalOnly());
        assertEquals(Long.valueOf(2), response.getStatus().getIndexedItems());
        assertEquals(Long.valueOf(2), AgentHistoryValue.longValue(response.asMap().get("status") instanceof Map
                ? ((Map<?, ?>) response.asMap().get("status")).get("indexedItems")
                : null));
        assertEquals(null, response.getStatus().asMap().get("futureCounter"));
    }

    private static void acceptsCanonicalSearchFixture() throws Exception {
        String fixture = readFixture("search.results.json");
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport("local-cli", fixture));

        SearchResponse response = client.search(AgentHistoryOptions.search().query("local agent history").refresh("off"));

        assertEquals("search", response.operation());
        assertEquals("/tmp/ctx-sdk-fixture", response.getBackend().getDataRoot());
        assertEquals("local agent history", response.getSearch().getQuery());
        assertEquals("codex", response.getSearch().getFilters().getProvider());
        assertEquals(Integer.valueOf(20), response.getSearch().getResultWindow().getLimit());
        assertEquals(Integer.valueOf(1), response.getSearch().getResultWindow().getReturned());
        assertEquals(Boolean.FALSE, response.getSearch().getResultWindow().getMoreAvailable());
        assertEquals(Integer.valueOf(20), response.getSearch().getPagination().getLimit());
        assertEquals(Boolean.FALSE, response.getSearch().getPagination().getHasMore());
        assertEquals(null, response.getSearch().getPagination().getNextCursor());
        assertEquals(Boolean.FALSE, response.getSearch().getTruncation().getTruncated());
        assertEquals(Integer.valueOf(1), Integer.valueOf(response.getSearch().getResults().size()));
        SearchHit hit = response.getSearch().getResults().get(0);
        assertEquals("11111111-1111-4111-8111-111111111111", hit.getCtxEventId());
        assertEquals(Double.valueOf(1.0), hit.getRank());
        assertEquals(Double.valueOf(0.98), hit.getRetrievalScore());
        assertEquals("event", hit.getResultType());
        assertEquals("event", hit.getResultScope());
        assertEquals("codex-fixture-session", hit.getProviderSessionId());
        assertEquals("codex_session_jsonl", hit.getSourceFormat());
        assertEquals("event", hit.getCitations().get(0).getTargetType());
        assertEquals("codex event", hit.getCitations().get(0).getLabel());
    }

    private static void exposesOptionalMcpToolCallMetadata() throws Exception {
        ShowEventResponse oldResponse = new ShowEventResponse(
                Json.parseObject(readFixture("show-event.window.json")));
        Event oldEvent = oldResponse.getEvent().getEvent();
        assertEquals(null, oldEvent.getMcpToolCall());
        assertEquals(Boolean.FALSE, Boolean.valueOf(oldEvent.asMap().containsKey("mcpToolCall")));

        ShowEventResponse newResponse = new ShowEventResponse(
                Json.parseObject(readFixture("show-event.mcp-tool-call.json")));
        McpToolCall call = newResponse.getEvent().getEvent().getMcpToolCall();
        assertEquals("mcp-サーバー-🦀", call.getServer());
        assertEquals("検索/工具/🛠️", call.getTool());
        assertEquals(Boolean.TRUE, newResponse.getEvent().getEvent()
                .asMap().get("futureEventField") instanceof Map<?, ?>);

        Map<String, Object> normalized = AgentHistoryEnvelope.normalize(
                "showEvent",
                new Backend("local", null, null),
                Json.parseObject("{\"event\":{\"mcp_tool_call\":{"
                        + "\"server\":\"mcp-サーバー-🦀\","
                        + "\"tool\":\"検索/工具/🛠️\"},"
                        + "\"future_event_field\":true},\"events\":[{}]}"));
        McpToolCall normalizedCall = new ShowEventResponse(normalized)
                .getEvent().getEvent().getMcpToolCall();
        assertEquals("mcp-サーバー-🦀", normalizedCall.getServer());
        assertEquals("検索/工具/🛠️", normalizedCall.getTool());
        assertEquals(Boolean.TRUE, new ShowEventResponse(normalized)
                .getEvent().getEvent().asMap().get("futureEventField"));

        String exact = "🦀".repeat(McpToolCall.MAX_COMPONENT_BYTES / 4);
        assertEquals(Integer.valueOf(McpToolCall.MAX_COMPONENT_BYTES), Integer.valueOf(
                new McpToolCall(Map.of("server", " ", "tool", exact))
                        .getTool().getBytes(StandardCharsets.UTF_8).length));

        for (String invalid : new String[] {
                "{\"mcpToolCall\":{\"server\":\"only-server\"}}",
                "{\"mcpToolCall\":{\"tool\":\"only-tool\"}}",
                "{\"mcpToolCall\":{\"server\":\"server\",\"tool\":\"tool\",\"future\":true}}",
                "{\"mcpToolCall\":{\"server\":\"\",\"tool\":\"tool\"}}",
                "{\"mcpToolCall\":{\"server\":\"server\",\"tool\":7}}",
                "{\"mcpToolCall\":{\"server\":\"server\",\"tool\":\"\\ud800\"}}",
                "{\"mcpToolCall\":null}"
        }) {
            assertProtocol(() -> new Event(Json.parseObject(invalid)));
        }
        assertProtocol(() -> new McpToolCall(Map.of(
                "server", "server",
                "tool", "a".repeat(McpToolCall.MAX_COMPONENT_BYTES + 1))));
    }

    private static void rejectsRawMcpToolCallDuplicateMembers() throws Exception {
        for (String name : new String[] {
                "duplicate-event-mcp-tool-call-snake.json",
                "duplicate-event-mcp-tool-call-camel.json",
                "duplicate-mcp-tool-call-server.json",
                "duplicate-mcp-tool-call-tool.json",
                "invalid-mcp-tool-call-transformed-server.json",
                "invalid-mcp-tool-call-transformed-tool.json",
                "invalid-mcp-tool-call-transformed-collision.json",
                "invalid-mcp-tool-call-outer-alias-collision.json",
                "invalid-mcp-tool-call-outer-mixed-case.json",
                "invalid-mcp-tool-call-outer-repeated-separator.json",
                "invalid-mcp-tool-call-outer-trailing-separator.json",
                "invalid-mcp-tool-call-outer-camel-snake.json"
        }) {
            AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                    "local-cli", readAdversarialFixture(name)));
            assertProtocol(() -> client.showEvent("event-1"));
        }

        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli", readAdversarialFixture("valid-repeated-string-contents.json")));
        Event event = client.showEvent("event-1").getEvent().getEvent();
        assertEquals("server server", event.getMcpToolCall().getServer());
        assertEquals("tool tool", event.getMcpToolCall().getTool());
        assertEquals(
                "server tool mcpToolCall mcp_tool_call server tool mcpToolCall mcp_tool_call",
                event.getText());

        AgentHistoryClient aliasesClient = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli", readAdversarialFixture("valid-mcp-tool-call-outer-aliases.json")));
        EventResult aliases = aliasesClient.showEvent("event-1").getEvent();
        assertEquals("snake-server", aliases.getEvent().getMcpToolCall().getServer());
        assertEquals("snake-extra", aliases.getEvent().asMap().get("futureEventField"));
        assertEquals("camel-server", aliases.getEvents().get(0).getMcpToolCall().getServer());
        assertEquals("camel-extra", aliases.getEvents().get(0).asMap().get("futureEventField"));
    }

    private static void strictlyDecodesSpawnedProcessStdoutUtf8() throws Exception {
        assertProtocol(() -> LocalCliAdapter.decodeUtf8Output(new byte[] {(byte) 0xff}));
        assertEquals("�", LocalCliAdapter.decodeUtf8Output("�".getBytes(StandardCharsets.UTF_8)));

        if (java.io.File.separatorChar == '\\') {
            return;
        }
        byte[] prefix = "{\"event\":{\"mcp_tool_call\":{\"server\":\""
                .getBytes(StandardCharsets.UTF_8);
        byte[] suffix = "\",\"tool\":\"tool\"}},\"events\":[]}"
                .getBytes(StandardCharsets.UTF_8);
        byte[] invalid = new byte[prefix.length + 1 + suffix.length];
        System.arraycopy(prefix, 0, invalid, 0, prefix.length);
        invalid[prefix.length] = (byte) 0xff;
        System.arraycopy(suffix, 0, invalid, prefix.length + 1, suffix.length);
        AgentHistoryClient invalidClient = AgentHistoryClient.local(LocalCliConfig.builder()
                .ctxPath(spawnedStdoutCli(invalid).toString())
                .build());
        assertProtocol(() -> invalidClient.showEvent("event-1"));

        byte[] valid = "{\"event\":{\"mcp_tool_call\":{\"server\":\"�\",\"tool\":\"tool\"}},\"events\":[]}"
                .getBytes(StandardCharsets.UTF_8);
        AgentHistoryClient validClient = AgentHistoryClient.local(LocalCliConfig.builder()
                .ctxPath(spawnedStdoutCli(valid).toString())
                .build());
        assertEquals("�", validClient.showEvent("event-1")
                .getEvent().getEvent().getMcpToolCall().getServer());
    }

    private static Path spawnedStdoutCli(byte[] payload) throws Exception {
        StringBuilder octal = new StringBuilder();
        for (byte value : payload) {
            String encoded = Integer.toOctalString(value & 0xff);
            octal.append('\\');
            for (int index = encoded.length(); index < 3; index++) {
                octal.append('0');
            }
            octal.append(encoded);
        }
        Path script = Files.createTempFile("ctx-agent-history-jvm-stdout-", ".sh");
        Files.write(
                script,
                ("#!/bin/sh\nprintf '" + octal + "'\n").getBytes(StandardCharsets.UTF_8));
        if (!script.toFile().setExecutable(true)) {
            throw new IllegalStateException("failed to make spawned stdout fixture executable");
        }
        script.toFile().deleteOnExit();
        return script;
    }

    private static void camelizesSearchRetrievalJson() {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli",
                "{"
                        + "\"schema_version\":1,"
                        + "\"query\":\"agent history\","
                        + "\"retrieval\":{"
                        + "\"requested_mode\":\"hybrid\","
                        + "\"effective_mode\":\"lexical\","
                        + "\"semantic_weight\":0.0,"
                        + "\"semantic_fallback_code\":\"semantic_retrieval_failed\","
                        + "\"semantic_fallback\":\"semantic_retrieval_failed\","
                        + "\"coverage\":{\"embedded_items\":4,\"indexed_now\":1},"
                        + "\"diagnostics\":{\"query_embed_ms\":2}"
                        + "},"
                        + "\"results\":[{\"result_scope\":\"event\",\"rank\":1,\"retrieval_score\":0.98}],"
                        + "\"result_window\":{\"limit\":1,\"returned\":1,\"more_available\":true}"
                        + "}"));

        SearchResponse response = client.search(AgentHistoryOptions.search().query("agent history"));
        SearchHit hit = response.getSearch().getResults().get(0);
        assertEquals(Double.valueOf(1.0), hit.getRank());
        assertEquals(Double.valueOf(0.98), hit.getRetrievalScore());
        Map<String, Object> retrieval = AgentHistoryValue.object(response.getSearch().getRetrieval());
        assertEquals("hybrid", retrieval.get("requestedMode"));
        assertEquals("lexical", retrieval.get("effectiveMode"));
        assertEquals(Double.valueOf(0.0), AgentHistoryValue.doubleValue(retrieval.get("semanticWeight")));
        assertEquals("semantic_retrieval_failed", retrieval.get("semanticFallbackCode"));
        assertEquals("semantic_retrieval_failed", retrieval.get("semanticFallback"));
        Map<String, Object> coverage = AgentHistoryValue.object(retrieval.get("coverage"));
        assertEquals(Integer.valueOf(4), AgentHistoryValue.integer(coverage.get("embeddedItems")));
        assertEquals(Integer.valueOf(1), AgentHistoryValue.integer(coverage.get("indexedNow")));
        Map<String, Object> diagnostics = AgentHistoryValue.object(retrieval.get("diagnostics"));
        assertEquals(Integer.valueOf(2), AgentHistoryValue.integer(diagnostics.get("queryEmbedMs")));
        assertEquals(Integer.valueOf(1), response.getSearch().getResultWindow().getLimit());
        assertEquals(Integer.valueOf(1), response.getSearch().getResultWindow().getReturned());
        assertEquals(Boolean.TRUE, response.getSearch().getResultWindow().getMoreAvailable());
        assertEquals(Integer.valueOf(1), response.getSearch().getPagination().getLimit());
        assertEquals(Boolean.TRUE, response.getSearch().getPagination().getHasMore());
        assertEquals(null, response.getSearch().getPagination().getNextCursor());
    }

    private static void normalizesRawShowResponses() {
        Map<String, String> responses = new LinkedHashMap<>();
        responses.put("showEvent", "{"
                + "\"event\":{\"ctx_event_id\":\"event-1\",\"ctx_session_id\":\"session-1\","
                + "\"provider\":\"codex\",\"provider_session_id\":\"provider-session\","
                + "\"source_format\":\"codex_session_jsonl\","
                + "\"sequence\":7,\"event_type\":\"message\",\"role\":\"assistant\","
                + "\"text\":\"hello\",\"content\":{\"complete\":true,\"policy_status\":\"selected\"}},"
                + "\"events\":[{\"ctx_event_id\":\"event-1\",\"ctx_session_id\":\"session-1\",\"sequence\":7}]"
                + "}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport("local-cli", responses));

        ShowEventResponse shown = client.showEvent("event-1");
        assertEquals("showEvent", shown.operation());
        assertEquals("event-1", shown.getEvent().getEvent().getCtxEventId());
        assertEquals(Integer.valueOf(7), shown.getEvent().getEvents().get(0).getSequence());
        assertEquals("provider-session", shown.getEvent().getEvent().getProviderSessionId());
        assertEquals("codex_session_jsonl", shown.getEvent().getEvent().getSourceFormat());
        assertEquals(Boolean.TRUE, shown.getEvent().getEvent().getContent().getComplete());
        assertEquals(CoreContentPolicyStatus.SELECTED, shown.getEvent().getEvent().getContent().getPolicyStatus());
    }

    private static void preservesLegitimateNestedSourceSemantics() {
        Map<String, Object> sourceRaw = Json.parseObject("{"
                + "\"sources\":[{\"provider\":\"codex\",\"path\":\"/configured/root\","
                + "\"status\":\"available\",\"importable\":true,"
                + "\"acquisition\":{\"source\":\"local_scan\",\"cursor\":\"opaque-checkpoint\"}}]}");
        Map<String, Object> sources = AgentHistoryEnvelope.normalize(
                "sources", new Backend("local", null, null), sourceRaw);
        List<Object> sourceList = AgentHistoryValue.rawList(sources.get("sources"));
        Map<String, Object> acquisition = AgentHistoryValue.objectAt(
                AgentHistoryValue.objectOrNull(sourceList.get(0)), "acquisition");
        assertEquals("local_scan", acquisition.get("source"));
        assertEquals("opaque-checkpoint", acquisition.get("cursor"));

        Map<String, Object> importRaw = Json.parseObject("{"
                + "\"resume\":false,\"totals\":{},"
                + "\"sources\":[{\"source\":{\"source\":\"provider\","
                + "\"cursor\":\"provider-checkpoint\"}}]}");
        Map<String, Object> imported = AgentHistoryEnvelope.normalize(
                "import", new Backend("local", null, null), importRaw);
        Map<String, Object> importPayload = AgentHistoryValue.objectAt(imported, "import");
        Map<String, Object> importSource = AgentHistoryValue.objectAt(
                AgentHistoryValue.objectOrNull(AgentHistoryValue.rawList(importPayload.get("sources")).get(0)),
                "source");
        assertEquals("provider", importSource.get("source"));
        assertEquals("provider-checkpoint", importSource.get("cursor"));
    }

    private static void decodesAllCanonicalFixturesThroughTypedResponses() throws Exception {
        java.nio.file.Path root = Paths.get("../../contracts/agent-history-v1/fixtures");
        try (java.util.stream.Stream<java.nio.file.Path> paths = Files.list(root)) {
            paths
                    .filter(path -> path.getFileName().toString().endsWith(".json"))
                    .forEach(path -> {
                        try {
                            Map<String, Object> canonical = Json.parseObject(new String(Files.readAllBytes(path), StandardCharsets.UTF_8));
                            String operation = String.valueOf(canonical.get("operation"));
                            switch (operation) {
                                case "status":
                                    assertEquals(Boolean.TRUE, new StatusResponse(canonical).getStatus().getInitialized());
                                    break;
                                case "init":
                                    assertEquals(Boolean.TRUE, new InitResponse(canonical).getStatus().getInitialized());
                                    break;
                                case "sources":
                                    new SourcesResponse(canonical).getSources();
                                    break;
                                case "import":
                                case "sync":
                                    new ImportResponse(canonical).getImportResult().getTotals();
                                    break;
                                case "search":
                                    new SearchResponse(canonical).getSearch().getResults();
                                    break;
                                case "showEvent":
                                    Event event = new ShowEventResponse(canonical).getEvent().getEvent();
                                    assertEquals("codex-fixture-session", event.getProviderSessionId());
                                    assertEquals("codex_session_jsonl", event.getSourceFormat());
                                    assertEquals(CoreContentPolicyStatus.SELECTED, event.getContent().getPolicyStatus());
                                    break;
                                case "showSession":
                                    SessionSummary summary = new ShowSessionResponse(canonical).getSession().getSession();
                                    assertEquals("codex-fixture-session", summary.getProviderSessionId());
                                    assertEquals("codex_session_jsonl", summary.getSourceFormat());
                                    break;
                                case "error":
                                    ErrorResponse error = new ErrorResponse(canonical);
                                    assertEquals("error", error.operation());
                                    if (error.getError().getCode() == null) {
                                        throw new AssertionError("missing typed error code in " + path);
                                    }
                                    break;
                                default:
                                    throw new AssertionError("unknown fixture operation " + operation + " in " + path);
                            }
                        } catch (Exception error) {
                            throw new RuntimeException("decode fixture " + path, error);
                        }
                    });
        }
    }

    private static void buildsSearchCommand() {
        FakeTransport transport = new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"query\":\"client\",\"results\":[]}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        client.search(AgentHistoryOptions.search()
                .query("agent history")
                .term("ctx")
                .limit(5)
                .backend("hybrid")
                .semanticWeight(Double.valueOf(0.35))
                .eventType("message")
                .refresh("off"));

        assertEquals("search", transport.lastOperation.name());
        assertContainsInOrder(transport.lastOperation.args(), "search", "agent history", "--format=json");
        assertContainsInOrder(transport.lastOperation.args(), "--limit", "5");
        assertContainsInOrder(transport.lastOperation.args(), "--backend", "hybrid");
        assertContainsInOrder(transport.lastOperation.args(), "--semantic-weight", "0.35");
        assertContainsInOrder(transport.lastOperation.args(), "--term", "ctx");
        assertContainsInOrder(transport.lastOperation.args(), "--event-type", "message");
        assertContainsInOrder(transport.lastOperation.args(), "--refresh", "off");
    }

    private static void searchContentScopeValuesAreClosed() {
        assertEquals(Integer.valueOf(4), Integer.valueOf(SearchContentScope.values().length));
        assertEquals(
                List.of("all", "transcript", "calls", "outputs"),
                List.of(
                        SearchContentScope.ALL.wireName(),
                        SearchContentScope.TRANSCRIPT.wireName(),
                        SearchContentScope.CALLS.wireName(),
                        SearchContentScope.OUTPUTS.wireName()));
    }

    private static void searchForwardsContentScopeOnce() {
        FakeTransport transport = new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"query\":\"client\",\"results\":[]}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        client.search(AgentHistoryOptions.search()
                .query("agent history")
                .contentScope(SearchContentScope.CALLS));

        assertContainsInOrder(transport.lastOperation.args(), "--content-scope", "calls");
        int count = 0;
        for (String arg : transport.lastOperation.args()) {
            if ("--content-scope".equals(arg)) {
                count += 1;
            }
        }
        assertEquals(Integer.valueOf(1), Integer.valueOf(count));
    }

    private static void searchRejectsContentScopeEventTypeConflictBeforeTransport() {
        FakeTransport transport = new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"query\":\"client\",\"results\":[]}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        assertValidation(() -> client.search(AgentHistoryOptions.search()
                .query("agent history")
                .eventType("message")
                .contentScope(SearchContentScope.ALL)));
        if (transport.lastOperation != null) {
            throw new AssertionError(
                    "conflicting search filters invoked transport: " + transport.lastOperation.args());
        }
    }

    private static void searchRequiresIntent() {
        FakeTransport transport = new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"query\":\"client\",\"results\":[]}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        assertValidation(() -> client.search());
        assertValidation(() -> client.search(AgentHistoryOptions.search().refresh("off").limit(5)));
        assertValidation(() -> client.search("   "));
        assertValidation(() -> client.search(AgentHistoryOptions.search().term("   ")));
        if (transport.lastOperation != null) {
            throw new AssertionError("invalid search invoked transport: " + transport.lastOperation.args());
        }
    }

    private static void hostedIsExplicitlyUnsupported() {
        AgentHistoryClient client = AgentHistoryClient.hosted(HostedConfig.builder().baseUrl("https://ctx.example.invalid").build());
        try {
            client.status();
            throw new AssertionError("expected hosted placeholder failure");
        } catch (CtxAgentHistoryException.Unsupported error) {
            assertEquals("not_supported", error.code());
            assertEquals("hosted", error.details().get("backend"));
            assertEquals("https://ctx.example.invalid", error.details().get("baseUrl"));
        }
    }

    private static String readFixture(String name) throws Exception {
        byte[] bytes = Files.readAllBytes(Paths.get("../../contracts/agent-history-v1/fixtures", name));
        return new String(bytes, StandardCharsets.UTF_8);
    }

    private static String readAdversarialFixture(String name) throws Exception {
        byte[] bytes = Files.readAllBytes(Paths.get(
                "../../contracts/agent-history-v1/fixtures/adversarial", name));
        return new String(bytes, StandardCharsets.UTF_8);
    }

    private static void assertContainsInOrder(List<String> values, String first, String second) {
        for (int i = 0; i + 1 < values.size(); i++) {
            if (first.equals(values.get(i)) && second.equals(values.get(i + 1))) {
                return;
            }
        }
        throw new AssertionError("expected adjacent args " + first + " " + second + " in " + values);
    }

    private static void assertContainsInOrder(List<String> values, String first, String second, String third) {
        for (int i = 0; i + 2 < values.size(); i++) {
            if (first.equals(values.get(i)) && second.equals(values.get(i + 1)) && third.equals(values.get(i + 2))) {
                return;
            }
        }
        throw new AssertionError("expected adjacent args " + first + " " + second + " " + third + " in " + values);
    }

    private static void assertEquals(Object want, Object got) {
        if (want == null ? got != null : !want.equals(got)) {
            throw new AssertionError("want " + want + " got " + got);
        }
    }

    private static void assertValidation(Runnable action) {
        try {
            action.run();
        } catch (CtxAgentHistoryException.Validation error) {
            assertEquals("invalid_request", error.code());
            return;
        }
        throw new AssertionError("expected validation error");
    }

    private static void assertProtocol(Runnable action) {
        try {
            action.run();
        } catch (CtxAgentHistoryException.Protocol error) {
            assertEquals("decode_error", error.code());
            return;
        }
        throw new AssertionError("expected protocol error");
    }

    private static final class FakeTransport implements AgentHistoryTransport {
        private final String name;
        private final String response;
        private final Map<String, String> responses;
        private AgentHistoryOperation lastOperation;

        FakeTransport(String name, String response) {
            this.name = name;
            this.response = response;
            this.responses = null;
        }

        FakeTransport(String name, Map<String, String> responses) {
            this.name = name;
            this.response = null;
            this.responses = responses;
        }

        @Override
        public String name() {
            return name;
        }

        @Override
        public String execute(AgentHistoryOperation operation) {
            this.lastOperation = operation;
            if (responses != null && responses.containsKey(operation.name())) {
                return responses.get(operation.name());
            }
            return response;
        }
    }
}
