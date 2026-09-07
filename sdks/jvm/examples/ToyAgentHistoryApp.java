import java.util.LinkedHashMap;
import java.util.Map;
import rs.ctx.agenthistory.AgentHistoryClient;
import rs.ctx.agenthistory.AgentHistoryOperation;
import rs.ctx.agenthistory.AgentHistoryOptions;
import rs.ctx.agenthistory.AgentHistoryTransport;
import rs.ctx.agenthistory.SearchResponse;
import rs.ctx.agenthistory.ShowEventResponse;
import rs.ctx.agenthistory.StatusResponse;

public final class ToyAgentHistoryApp {
    public static void main(String[] args) {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeAgentHistoryTransport());

        StatusResponse status = client.status();
        SearchResponse search = client.search(AgentHistoryOptions.search()
                .query("local agent history")
                .provider("codex")
                .refresh("off")
                .limit(Integer.valueOf(5)));
        ShowEventResponse shown = client.showEvent("evt-toy-1", AgentHistoryOptions.showEvent().window(Integer.valueOf(1)));

        System.out.println("status.initialized=" + status.getStatus().getInitialized());
        System.out.println("search.results=" + search.getSearch().getResults().size());
        System.out.println("search.window=" + search.getSearch().getResultWindow().getReturned()
                + "/" + search.getSearch().getResultWindow().getLimit()
                + " more=" + search.getSearch().getResultWindow().getMoreAvailable());
        System.out.println("show.event=" + shown.getEvent().getEvent().getCtxEventId());
        System.out.println("show.providerSession=" + shown.getEvent().getEvent().getProviderSessionId());
    }

    private static final class FakeAgentHistoryTransport implements AgentHistoryTransport {
        private final Map<String, String> responses = new LinkedHashMap<>();

        FakeAgentHistoryTransport() {
            responses.put("status", "{"
                    + "\"schema_version\":1,"
                    + "\"initialized\":true,"
                    + "\"local_only\":true,"
                    + "\"indexed_items\":1,"
                    + "\"indexed_sources\":1"
                    + "}");
            responses.put("search", "{"
                    + "\"query\":\"local agent history\","
                    + "\"filters\":{\"provider\":\"codex\"},"
                    + "\"freshness\":{\"mode\":\"off\",\"status\":\"skipped\",\"source_count\":0},"
                    + "\"results\":[{"
                    + "\"ctx_event_id\":\"evt-toy-1\","
                    + "\"ctx_session_id\":\"ses-toy-1\","
                    + "\"result_type\":\"event\","
                    + "\"result_scope\":\"event\","
                    + "\"provider\":\"codex\","
                    + "\"snippet\":\"toy local agent history result\","
                    + "\"citations\":[{\"target_type\":\"event\",\"label\":\"toy event\",\"ctx_event_id\":\"evt-toy-1\"}]"
                    + "}],"
                    + "\"result_window\":{\"limit\":1,\"returned\":1,\"more_available\":true},"
                    + "\"truncation\":{\"truncated\":false}"
                    + "}");
            responses.put("showEvent", "{"
                    + "\"event\":{\"ctx_event_id\":\"evt-toy-1\",\"ctx_session_id\":\"ses-toy-1\","
                    + "\"provider\":\"codex\",\"provider_session_id\":\"provider-toy-1\","
                    + "\"sequence\":1,\"event_type\":\"message\",\"role\":\"assistant\","
                    + "\"text\":\"toy local agent history result\"},"
                    + "\"events\":[{\"ctx_event_id\":\"evt-toy-1\",\"ctx_session_id\":\"ses-toy-1\",\"sequence\":1}]"
                    + "}");
        }

        @Override
        public String name() {
            return "local-fake";
        }

        @Override
        public String execute(AgentHistoryOperation operation) {
            String response = responses.get(operation.name());
            if (response == null) {
                throw new IllegalArgumentException("unsupported toy operation: " + operation.name());
            }
            return response;
        }
    }
}
