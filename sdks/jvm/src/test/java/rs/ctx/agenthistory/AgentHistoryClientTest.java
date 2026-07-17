package rs.ctx.agenthistory;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

public final class AgentHistoryClientTest {
    public static void main(String[] args) throws Exception {
        if (args.length > 0 && "--ctx-sdk-helper".equals(args[0])) {
            runProcessHelper(java.util.Arrays.copyOfRange(args, 1, args.length));
            return;
        }
        wrapsRawStatusAsTypedEnvelope();
        normalizesSetupJsonAsInitStatus();
        acceptsCanonicalSearchFixture();
        decodesAllCanonicalFixturesThroughTypedResponses();
        normalizesRawShowAndLocateResponses();
        buildsSearchCommand();
        canonicalizesSearchQueriesBeforeBounds();
        validatesExplicitSearchLimits();
        omitsObsoleteRetrievalFields();
        rejectsNonCanonicalSearchResponses();
        rejectsOversizedLocalCliCapture();
        exercisesAdversarialLocalCliCapture();
        boundsInheritedPipeTeardown();
        killsSuccessfulSameScopeChild();
        interruptionCancelsAndReapsLocalCli();
        searchRequiresIntent();
        hostedIsExplicitlyUnsupported();
    }

    private static void normalizesSetupJsonAsInitStatus() {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"data_root\":\"/tmp/ctx\",\"mode\":\"ready\",\"indexed_items\":9,"
                        + "\"catalog\":{\"cataloged_sessions\":1},\"import\":{\"resume\":false,\"totals\":{}},"
                        + "\"network_required\":false}"));

        InitResponse response = client.init(AgentHistoryOptions.init().catalogOnly(true));

        assertEquals("init", response.operation());
        assertEquals(Boolean.TRUE, response.getStatus().getInitialized());
        assertEquals(Boolean.TRUE, response.getStatus().getLocalOnly());
        assertEquals(Integer.valueOf(9), response.getStatus().getIndexedItems());
    }

    private static void wrapsRawStatusAsTypedEnvelope() {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli",
                "{\"schema_version\":1,\"initialized\":true,\"indexed_items\":2,\"local_only\":true}"));

        StatusResponse response = client.status();

        assertEquals("agent-history-v1", response.contractVersion());
        assertEquals(Integer.valueOf(1), Integer.valueOf(response.schemaVersion()));
        assertEquals("status", response.operation());
        assertEquals("local", response.getBackend().getKind());
        assertEquals(Boolean.TRUE, response.getStatus().getInitialized());
        assertEquals(Boolean.TRUE, response.getStatus().getLocalOnly());
        assertEquals(Integer.valueOf(2), response.getStatus().getIndexedItems());
        assertEquals(Integer.valueOf(2), AgentHistoryValue.integer(response.asMap().get("status") instanceof Map
                ? ((Map<?, ?>) response.asMap().get("status")).get("indexedItems")
                : null));
    }

    private static void acceptsCanonicalSearchFixture() throws Exception {
        String fixture = readFixture("search.results.json");
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport("local-cli", fixture));

        SearchResponse response = client.search(AgentHistoryOptions.search().query(SearchQuery.all("local agent history")).refresh("off"));

        assertEquals("search", response.operation());
        assertEquals("/tmp/ctx-sdk-fixture", response.getBackend().getDataRoot());
        assertEquals("local agent history", response.getSearch().getQuery().any().get(0).value());
        assertEquals("ctx-search-v1", response.getSearch().getQueryExecution().queryVersion());
        assertEquals("codex", response.getSearch().getFilters().getProvider());
        assertEquals(Integer.valueOf(20), response.getSearch().getPagination().getLimit());
        assertEquals(Boolean.FALSE, response.getSearch().getTruncation().getTruncated());
        assertEquals(Integer.valueOf(1), Integer.valueOf(response.getSearch().getResults().size()));
        SearchHit hit = response.getSearch().getResults().get(0);
        assertEquals("11111111-1111-4111-8111-111111111111", hit.getCtxEventId());
        assertEquals("event", hit.getResultType());
        assertEquals("event", hit.getResultScope());
        assertEquals("event", hit.getCitations().get(0).getTargetType());
        assertEquals("codex event", hit.getCitations().get(0).getLabel());
    }

    private static void normalizesRawShowAndLocateResponses() {
        Map<String, String> responses = new LinkedHashMap<>();
        responses.put("showEvent", "{"
                + "\"event\":{\"ctx_event_id\":\"event-1\",\"ctx_session_id\":\"session-1\","
                + "\"sequence\":7,\"event_type\":\"message\",\"role\":\"assistant\","
                + "\"source\":\"codex\",\"text\":\"hello\"},"
                + "\"events\":[{\"ctx_event_id\":\"event-1\",\"ctx_session_id\":\"session-1\",\"sequence\":7}],"
                + "\"source\":{\"path\":\"/tmp/session.jsonl\",\"exists\":true}"
                + "}");
        responses.put("locateEvent", "{"
                + "\"ctx_session_id\":\"session-1\","
                + "\"ctx_event_id\":\"event-1\","
                + "\"provider\":\"codex\","
                + "\"provider_session_id\":\"provider-session\","
                + "\"source\":{\"path\":\"/tmp/session.jsonl\",\"cursor\":\"line:7\",\"exists\":true},"
                + "\"resume\":{\"cursor\":\"line:7\"}"
                + "}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport("local-cli", responses));

        ShowEventResponse shown = client.showEvent("event-1");
        assertEquals("showEvent", shown.operation());
        assertEquals("event-1", shown.getEvent().getEvent().getCtxEventId());
        assertEquals(Integer.valueOf(7), shown.getEvent().getEvents().get(0).getSequence());
        assertEquals("/tmp/session.jsonl", shown.getEvent().getSource().getPath());

        LocateEventResponse located = client.locateEvent("event-1");
        assertEquals("locateEvent", located.operation());
        assertEquals("session-1", located.getLocation().getCtxSessionId());
        assertEquals("line:7", located.getLocation().getSource().getCursor());
        assertEquals("line:7", located.getLocation().getResume().getCursor());
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
                                    new ShowEventResponse(canonical).getEvent().getEvents();
                                    break;
                                case "showSession":
                                    new ShowSessionResponse(canonical).getSession().getEvents();
                                    break;
                                case "locateEvent":
                                    new LocateEventResponse(canonical).getLocation().getSource();
                                    break;
                                case "locateSession":
                                    new LocateSessionResponse(canonical).getLocation().getSource();
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
                emptySearchJson());
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        client.search(AgentHistoryOptions.search()
                .query(SearchQuery.builder()
                        .any(SearchClause.all("agent history"))
                        .any(SearchClause.semantic("find related ctx work"))
                        .must(SearchClause.all("ctx"))
                        .build())
                .limit(5)
                .backend("hybrid")
                .provider("custom")
                .historySource("dorkos/default")
                .providerKey("dorkos")
                .sourceId("default")
                .sourceFormat("dorkos-history-v1")
                .includeSubagents(true)
                .eventType("message")
                .refresh("off"));

        assertEquals("search", transport.lastOperation.name());
        assertContainsInOrder(transport.lastOperation.args(), "search", "--query-json");
        assertContains(transport.lastOperation.args(), "\"semantic\":\"find related ctx work\"");
        assertContainsInOrder(transport.lastOperation.args(), "--limit", "5");
        assertContainsInOrder(transport.lastOperation.args(), "--backend", "hybrid");
        assertContainsInOrder(transport.lastOperation.args(), "--provider", "custom");
        assertContainsInOrder(transport.lastOperation.args(), "--history-source", "dorkos/default");
        assertContainsInOrder(transport.lastOperation.args(), "--provider-key", "dorkos");
        assertContainsInOrder(transport.lastOperation.args(), "--source-id", "default");
        assertContainsInOrder(transport.lastOperation.args(), "--source-format", "dorkos-history-v1");
        assertContains(transport.lastOperation.args(), "--include-subagents");
        assertContainsInOrder(transport.lastOperation.args(), "--event-type", "message");
        assertContainsInOrder(transport.lastOperation.args(), "--refresh", "off");
    }

    private static void canonicalizesSearchQueriesBeforeBounds() {
        SearchQuery.Builder builder = SearchQuery.builder();
        for (int index = 0; index < 33; index++) {
            builder.any(SearchClause.all("cafe\u0301" + "\u00a0".repeat(index + 1) + "\u4e16\u754c"));
        }
        SearchQuery query = builder
                .any(SearchClause.literal("\u3000logs_2.db  raw\u00a0"))
                .any(SearchClause.semantic(" related\u202fctx\nwork "))
                .any(SearchClause.semantic("related ctx work"))
                .mustNot(SearchClause.phrase(" postgres\u2003vacuum "))
                .build();

        assertEquals(Integer.valueOf(3), Integer.valueOf(query.any().size()));
        assertEquals("cafe\u0301 \u4e16\u754c", query.any().get(0).value());
        assertEquals("logs_2.db  raw", query.any().get(1).value());
        assertEquals("related ctx work", query.any().get(2).value());
        assertEquals("postgres vacuum", query.mustNot().get(0).value());

        StringBuilder whitespace = new StringBuilder();
        for (int codePoint : new int[] {
                0x0009, 0x000a, 0x000b, 0x000c, 0x000d, 0x0020, 0x0085, 0x00a0,
                0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006,
                0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000
        }) {
            whitespace.appendCodePoint(codePoint);
        }
        assertEquals("cafe\u0301 \u4e16\u754c", SearchQuery.all(
                "cafe\u0301" + whitespace + "\u4e16\u754c").any().get(0).value());

        SearchQuery.Builder exactClauseLimit = SearchQuery.builder();
        for (int index = 0; index < SearchQuery.MAX_CLAUSES; index++) {
            exactClauseLimit.any(SearchClause.all("term" + index));
        }
        assertEquals(Integer.valueOf(SearchQuery.MAX_CLAUSES),
                Integer.valueOf(exactClauseLimit.build().any().size()));
        SearchQuery.Builder aboveClauseLimit = SearchQuery.builder();
        for (int index = 0; index <= SearchQuery.MAX_CLAUSES; index++) {
            aboveClauseLimit.any(SearchClause.all("term" + index));
        }
        assertValidation(() -> aboveClauseLimit.build());

        assertValidation(() -> SearchQuery.all("!!!"));
        StringBuilder boundedTokens = new StringBuilder();
        for (int index = 0; index < 32; index++) {
            if (index > 0) boundedTokens.append(' ');
            boundedTokens.append("a\u0301b");
        }
        SearchQuery.all(boundedTokens.toString());
        boundedTokens.append(" a\u0301b");
        assertValidation(() -> SearchQuery.all(boundedTokens.toString()));
        SearchQuery.all("\u00b2");

        SearchQuery.all("x".repeat(SearchQuery.MAX_CLAUSE_BYTES));
        assertValidation(() -> SearchQuery.all("x".repeat(SearchQuery.MAX_CLAUSE_BYTES + 1)));
        SearchQuery.builder()
                .any(SearchClause.literal("abc"))
                .any(SearchClause.literal("x".repeat(SearchQuery.MAX_LITERAL_BYTES)))
                .build();
        assertValidation(() -> SearchQuery.builder().any(SearchClause.literal("ab")).build());
        assertValidation(() -> SearchQuery.builder()
                .any(SearchClause.literal("x".repeat(SearchQuery.MAX_LITERAL_BYTES + 1)))
                .build());

        SearchQuery.Builder exactTotalBytes = SearchQuery.builder();
        for (int index = 0; index < 8; index++) {
            exactTotalBytes.any(SearchClause.all(index + "x".repeat(SearchQuery.MAX_CLAUSE_BYTES - 1)));
        }
        exactTotalBytes.build();
        exactTotalBytes.any(SearchClause.all("z"));
        assertValidation(() -> exactTotalBytes.build());
    }

    private static void validatesExplicitSearchLimits() {
        FakeTransport rejectedTransport = new FakeTransport("local-cli", emptySearchJson());
        AgentHistoryClient rejected = AgentHistoryClient.withTransport(rejectedTransport);
        for (int limit : new int[] {-1, 0, 201}) {
            assertValidation(() -> rejected.search(AgentHistoryOptions.search()
                    .query(SearchQuery.all("bounded limit"))
                    .limit(Integer.valueOf(limit))));
        }
        if (rejectedTransport.lastOperation != null) {
            throw new AssertionError("invalid limit invoked transport: " + rejectedTransport.lastOperation.args());
        }

        FakeTransport acceptedTransport = new FakeTransport("local-cli", emptySearchJson());
        AgentHistoryClient accepted = AgentHistoryClient.withTransport(acceptedTransport);
        accepted.search(AgentHistoryOptions.search()
                .query(SearchQuery.all("bounded limit"))
                .limit(Integer.valueOf(1)));
        assertContainsInOrder(acceptedTransport.lastOperation.args(), "--limit", "1");
        accepted.search(AgentHistoryOptions.search()
                .query(SearchQuery.all("bounded limit"))
                .limit(Integer.valueOf(200)));
        assertContainsInOrder(acceptedTransport.lastOperation.args(), "--limit", "200");
    }

    private static void omitsObsoleteRetrievalFields() {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeTransport(
                "local-cli",
                searchJsonWithObsoleteRetrieval()));

        SearchResult search = client.search(AgentHistoryOptions.search()
                .query(SearchQuery.all("agent history")))
                .getSearch();
        Map<String, Object> retrieval = AgentHistoryValue.object(search.getRetrieval());
        assertEquals("hybrid", retrieval.get("requestedMode"));
        assertEquals("lexical", retrieval.get("effectiveMode"));
        assertAbsent(retrieval, "semanticWeight");
        assertAbsent(retrieval, "semanticFallbackCode");
        assertAbsent(retrieval, "semanticFallback");
        assertEquals(Integer.valueOf(4), AgentHistoryValue.integer(
                AgentHistoryValue.object(retrieval.get("coverage")).get("embeddedItems")));
        assertEquals(Integer.valueOf(2), AgentHistoryValue.integer(
                AgentHistoryValue.object(retrieval.get("diagnostics")).get("queryEmbedMs")));
        assertAbsent(search.getResults().get(0).asMap(), "retrieval");
    }

    private static void rejectsNonCanonicalSearchResponses() {
        String canonicalQuery = "\"query\":{\"version\":\"ctx-search-v1\","
                + "\"any\":[{\"all\":\"agent history\"}]}";
        String[] invalid = new String[] {
                "{\"schema_version\":1," + canonicalQuery + ",\"query_execution\":{}}",
                "{\"schema_version\":2,\"query\":\"agent history\",\"query_execution\":{}}",
                "{\"schemaVersion\":2," + canonicalQuery + ",\"query_execution\":{}}",
                "{\"schema_version\":2," + canonicalQuery + ",\"queryExecution\":{}}",
                "{\"schema_version\":2,\"query_execution\":{},\"results\":[]}",
                "{\"schema_version\":2,\"query\":null,\"results\":[]}",
                "{\"schema_version\":2,\"query\":null,\"query_execution\":{}}",
                "{\"schema_version\":2,\"query\":null,\"query_execution\":{},\"results\":{}}"
        };
        for (String response : invalid) {
            AgentHistoryClient client = AgentHistoryClient.withTransport(
                    new FakeTransport("local-cli", response));
            assertProtocol(() -> client.search(AgentHistoryOptions.search()
                    .query(SearchQuery.all("agent history"))));
        }
    }

    private static void rejectsOversizedLocalCliCapture() {
        int stdoutCapBytes = 2 * 1024 * 1024;
        LocalCliAdapter adapter = new LocalCliAdapter(LocalCliConfig.builder()
                .runner(request -> new CommandResult("x".repeat(stdoutCapBytes + 1), "", 0))
                .build());
        try {
            adapter.execute(new AgentHistoryOperation("status", java.util.Collections.singletonList("status")));
            throw new AssertionError("expected capture-limit error");
        } catch (CtxAgentHistoryException error) {
            assertEquals("capture_limit", error.code());
            assertEquals("stdout", error.details().get("stream"));
            assertEquals(Integer.valueOf(stdoutCapBytes), error.details().get("capBytes"));
            assertAbsent(error.details(), "stdout");
            assertAbsent(error.details(), "stderr");
        }
    }

    private static void exercisesAdversarialLocalCliCapture() {
        if (!hasNativeProcessScope()) {
            try {
                processAdapter(2_000).execute(new AgentHistoryOperation("status", helperArgs("dual")));
                throw new AssertionError("expected unavailable process-scope failure");
            } catch (CtxAgentHistoryException error) {
                assertEquals("capture_failure", error.code());
                assertEquals("process_scope", error.details().get("stream"));
            }
            return;
        }
        LocalCliAdapter adapter = processAdapter(2_000);
        String dual = adapter.execute(new AgentHistoryOperation("status", helperArgs("dual")));
        assertEquals(Integer.valueOf(30 * 8192), Integer.valueOf(dual.length()));

        long started = System.nanoTime();
        try {
            adapter.execute(new AgentHistoryOperation("status", helperArgs("stderr-first")));
            throw new AssertionError("expected stderr capture limit");
        } catch (CtxAgentHistoryException error) {
            assertEquals("capture_limit", error.code());
            assertEquals("stderr", error.details().get("stream"));
        }
        if (TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - started) >= 2_000) {
            throw new AssertionError("stderr-first overflow exceeded bounded teardown");
        }
    }

    private static void boundsInheritedPipeTeardown() throws Exception {
        if (!hasNativeProcessScope()) return;
        Path directory = Files.createTempDirectory("ctx-jvm-scope-");
        Path alive = directory.resolve("child.alive");
        Path pid = directory.resolve("child.pid");
        long started = System.nanoTime();
        try {
            processAdapter(5_000).execute(new AgentHistoryOperation(
                    "status", helperArgs("inherit", alive.toString(), pid.toString())));
            throw new AssertionError("expected inherited-pipe failure");
        } catch (CtxAgentHistoryException error) {
            assertEquals("capture_failure", error.code());
            assertEquals("pipe", error.details().get("stream"));
        }
        if (TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - started) >= 2_000) {
            throw new AssertionError("inherited-pipe teardown exceeded its deadline");
        }
        Thread.sleep(700);
        if (Files.exists(alive)) {
            throw new AssertionError("owned inherited-handle descendant survived teardown");
        }
    }

    private static void killsSuccessfulSameScopeChild() throws Exception {
        if (!hasNativeProcessScope()) return;
        Path directory = Files.createTempDirectory("ctx-jvm-success-");
        Path alive = directory.resolve("child.alive");
        Path pid = directory.resolve("child.pid");
        String output = processAdapter(2_000).execute(new AgentHistoryOperation(
                "status", helperArgs("success-child", alive.toString(), pid.toString())));
        assertEquals("{}", output);
        Thread.sleep(700);
        if (Files.exists(alive)) {
            throw new AssertionError("successful scoped command left a silent child alive");
        }
    }

    private static void interruptionCancelsAndReapsLocalCli() throws Exception {
        if (!hasNativeProcessScope()) return;
        Path directory = Files.createTempDirectory("ctx-jvm-interrupt-");
        Path alive = directory.resolve("child.alive");
        AtomicReference<Throwable> failure = new AtomicReference<>();
        Thread caller = new Thread(() -> {
            try {
                processAdapter(60_000).execute(new AgentHistoryOperation(
                        "status", helperArgs("linger", alive.toString())));
                failure.set(new AssertionError("interrupted command unexpectedly succeeded"));
            } catch (Throwable error) {
                failure.set(error);
            }
        });
        caller.start();
        Thread.sleep(100);
        caller.interrupt();
        caller.join(2_000);
        if (caller.isAlive()) throw new AssertionError("interrupted command did not return boundedly");
        Throwable error = failure.get();
        if (!(error instanceof CtxAgentHistoryException)
                || !"cancelled".equals(((CtxAgentHistoryException) error).code())) {
            throw new AssertionError("interruption was not a typed cancellation", error);
        }
        Thread.sleep(700);
        if (Files.exists(alive)) {
            throw new AssertionError("interrupted command left its owned process alive");
        }
    }

    private static LocalCliAdapter processAdapter(long timeoutMillis) {
        return new LocalCliAdapter(LocalCliConfig.builder()
                .ctxPath(Paths.get(System.getProperty("java.home"), "bin", "java").toString())
                .timeoutMillis(timeoutMillis)
                .build());
    }

    private static List<String> helperArgs(String mode, String... extra) {
        List<String> args = new java.util.ArrayList<>();
        args.add("-cp");
        args.add(System.getProperty("java.class.path"));
        args.add(AgentHistoryClientTest.class.getName());
        args.add("--ctx-sdk-helper");
        args.add(mode);
        args.addAll(java.util.Arrays.asList(extra));
        return args;
    }

    private static boolean hasSetsid() {
        return Files.isExecutable(Paths.get("/usr/bin/setsid"))
                || Files.isExecutable(Paths.get("/bin/setsid"));
    }

    private static boolean hasNativeProcessScope() {
        String launcher = System.getenv("CTX_SDK_PROCESS_SCOPE_LAUNCHER");
        return hasSetsid() || (launcher != null && !launcher.isEmpty());
    }

    private static void runProcessHelper(String[] args) throws Exception {
        switch (args[0]) {
            case "dual":
                byte[] block = new byte[8192];
                java.util.Arrays.fill(block, (byte) 'x');
                Thread stdout = new Thread(() -> writeBlocks(System.out, block));
                Thread stderr = new Thread(() -> writeBlocks(System.err, block));
                stdout.start();
                stderr.start();
                stdout.join();
                stderr.join();
                return;
            case "stderr-first":
                System.err.write(new byte[256 * 1024 + 1]);
                System.err.flush();
                Thread.sleep(60_000);
                return;
            case "inherit":
            case "success-child":
                ProcessBuilder child;
                boolean windows = System.getProperty("os.name", "")
                        .toLowerCase(java.util.Locale.ROOT)
                        .contains("win");
                if (windows) {
                    List<String> childCommand = new java.util.ArrayList<>();
                    childCommand.add(Paths.get(System.getProperty("java.home"), "bin", "java").toString());
                    childCommand.addAll(helperArgs("linger", args[1]));
                    child = new ProcessBuilder(childCommand);
                } else {
                    child = new ProcessBuilder(
                            "/bin/sh", "-c", "echo $$ > \"$2\"; trap '' TERM; sleep .5; touch \"$1\"; sleep 60",
                            "ctx-sdk", args[1], args[2]);
                }
                if ("inherit".equals(args[0])) {
                    child.inheritIO();
                } else {
                    child.redirectOutput(ProcessBuilder.Redirect.DISCARD);
                    child.redirectError(ProcessBuilder.Redirect.DISCARD);
                }
                Process started = child.start();
                if (windows) {
                    Files.write(
                            Paths.get(args[2]),
                            Long.toString(started.pid()).getBytes(StandardCharsets.UTF_8));
                }
                if ("success-child".equals(args[0])) System.out.print("{}");
                return;
            case "linger":
                Thread.sleep(500);
                Files.write(Paths.get(args[1]), "alive".getBytes(StandardCharsets.UTF_8));
                Thread.sleep(60_000);
                return;
            default:
                throw new IllegalArgumentException("unknown process helper mode: " + args[0]);
        }
    }

    private static void writeBlocks(java.io.OutputStream stream, byte[] block) {
        try {
            for (int index = 0; index < 30; index++) stream.write(block);
            stream.flush();
        } catch (IOException error) {
            throw new RuntimeException(error);
        }
    }

    private static void searchRequiresIntent() {
        FakeTransport transport = new FakeTransport(
                "local-cli",
                emptySearchJson());
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        assertValidation(() -> client.search());
        assertValidation(() -> client.search(AgentHistoryOptions.search().refresh("off").limit(5)));
        assertValidation(() -> client.search(AgentHistoryOptions.search().query(SearchQuery.builder().mustNot(SearchClause.all("only negative")).build())));
        assertValidation(() -> client.search(AgentHistoryOptions.search().query(SearchQuery.builder().must(SearchClause.semantic("invalid placement")).build())));
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

    private static String emptySearchJson() {
        return searchJson("\"results\":[]");
    }

    private static String searchJsonWithObsoleteRetrieval() {
        return searchJson("\"retrieval\":{"
                + "\"requested_mode\":\"hybrid\",\"effective_mode\":\"lexical\","
                + "\"semantic_weight\":0.0,"
                + "\"semantic_fallback_code\":\"semantic_retrieval_failed\","
                + "\"semantic_fallback\":\"semantic_retrieval_failed\","
                + "\"coverage\":{\"embedded_items\":4},"
                + "\"diagnostics\":{\"query_embed_ms\":2}},"
                + "\"results\":[{\"result_scope\":\"event\",\"retrieval\":{\"score\":0.8}}]");
    }

    private static String searchJson(String resultFields) {
        return "{\"schema_version\":2,\"query\":{\"version\":\"ctx-search-v1\",\"any\":[{\"all\":\"agent history\"}]},"
                + "\"query_execution\":{\"query_version\":\"ctx-search-v1\",\"candidate_strategy\":\"bounded_fts\","
                + "\"resolved\":" + limitsJson() + ",\"consumed\":" + consumedJson() + ","
                + "\"semantic\":{\"attempted\":false,\"required\":false,\"readiness\":\"unavailable\",\"effective_backend\":\"lexical\",\"requested_candidates\":0,\"eligible_candidates\":0,\"candidates_supplied\":0,\"candidates_consumed\":0,\"candidates_used\":0,\"coverage\":{},\"completeness\":\"not_attempted\",\"positive_text_rule_version\":\"ctx-search-positive-text-v1\"},"
                + "\"rrf_k\":60,\"per_branch_candidate_rows\":0,\"requested_result_limit\":20,\"result_limit\":20,\"max_result_limit\":200,\"clauses_executed\":1,\"verification_dropped\":0,\"filter_verification_dropped\":0,\"candidate_budget_exhausted\":false,\"timed_out\":false,\"truncated\":false},"
                + resultFields + "}";
    }

    private static String limitsJson() {
        return "{\"query_bytes\":8192,\"clauses\":32,\"analyzed_tokens_per_clause\":32,\"candidates_per_positive_seed\":1024,\"candidate_rows\":16384,\"retained_candidate_ids\":8192,\"residual_rows\":8192,\"verification_bytes\":16777216,\"verification_lookup_bytes\":16384,\"hydrated_rows\":256,\"hydration_input_bytes\":8388608,\"hydration_input_bytes_per_event\":65536,\"snippet_input_bytes\":8388608,\"returned_text_bytes\":524288,\"serialized_response_bytes\":2097152,\"results\":200,\"elapsed_ms\":1000}";
    }

    private static String consumedJson() {
        return "{\"query_bytes\":13,\"clauses\":1,\"analyzed_tokens\":2,\"largest_analyzed_tokens_per_clause\":2,\"largest_positive_seed_candidates\":0,\"candidate_rows\":0,\"retained_candidate_ids\":0,\"residual_rows\":0,\"verification_bytes\":0,\"largest_verification_lookup_bytes\":0,\"hydrated_rows\":0,\"hydration_input_bytes\":0,\"largest_hydration_input_bytes\":0,\"snippet_input_bytes\":0,\"returned_results\":0,\"returned_text_bytes\":0,\"serialized_response_bytes\":0,\"elapsed_ms\":1}";
    }

    private static void assertContains(List<String> values, String fragment) {
        for (String value : values) if (value.contains(fragment)) return;
        throw new AssertionError("expected fragment " + fragment + " in " + values);
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

    private static void assertAbsent(Map<String, Object> values, String key) {
        if (values.containsKey(key)) {
            throw new AssertionError("unexpected key " + key + " in " + values);
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
