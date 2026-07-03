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
        decodesAllCanonicalFixturesThroughTypedResponses();
        normalizesRawShowAndLocateResponses();
        buildsSearchCommand();
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

        SearchResponse response = client.search(AgentHistoryOptions.search().query("local agent history").refresh("off"));

        assertEquals("search", response.operation());
        assertEquals("/tmp/ctx-sdk-fixture", response.getBackend().getDataRoot());
        assertEquals("local agent history", response.getSearch().getQuery());
        assertEquals("codex", response.getSearch().getFilters().getProvider());
        assertEquals(Integer.valueOf(20), response.getSearch().getPagination().getLimit());
        assertEquals(Boolean.FALSE, response.getSearch().getTruncation().getTruncated());
        assertEquals(Integer.valueOf(1), Integer.valueOf(response.getSearch().getResults().size()));
        SearchHit hit = response.getSearch().getResults().get(0);
        assertEquals("11111111-1111-4111-8111-111111111111", hit.getCtxEventId());
        assertEquals("event", hit.getResultScope());
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
                "{\"schema_version\":1,\"query\":\"client\",\"results\":[]}");
        AgentHistoryClient client = AgentHistoryClient.withTransport(transport);

        client.search(AgentHistoryOptions.search().query("agent history").term("ctx").limit(5).refresh("off"));

        assertEquals("search", transport.lastOperation.name());
        assertContainsInOrder(transport.lastOperation.args(), "search", "agent history", "--json");
        assertContainsInOrder(transport.lastOperation.args(), "--limit", "5");
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
