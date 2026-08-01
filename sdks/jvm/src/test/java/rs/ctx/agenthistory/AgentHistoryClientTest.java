package rs.ctx.agenthistory;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class AgentHistoryClientTest {
    public static void main(String[] args) throws Exception {
        wrapsRawStatusAsTypedEnvelope();
        normalizesSetupJsonAsInitStatus();
        acceptsCanonicalSearchFixture();
        camelizesSearchRetrievalJson();
        decodesAllCanonicalFixturesThroughTypedResponses();
        normalizesRawShowResponses();
        preservesLegitimateNestedSourceSemantics();
        buildsSearchCommand();
        localCliForcesAnalyticsOffAfterAmbientAndUserEnvironment();
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
                .refresh("off"));

        assertEquals("search", transport.lastOperation.name());
        assertContainsInOrder(transport.lastOperation.args(), "search", "agent history", "--format=json");
        assertContainsInOrder(transport.lastOperation.args(), "--limit", "5");
        assertContainsInOrder(transport.lastOperation.args(), "--backend", "hybrid");
        assertContainsInOrder(transport.lastOperation.args(), "--semantic-weight", "0.35");
        assertContainsInOrder(transport.lastOperation.args(), "--term", "ctx");
        assertContainsInOrder(transport.lastOperation.args(), "--refresh", "off");
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
