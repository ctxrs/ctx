import java.util.LinkedHashMap;
import java.util.Map;
import rs.ctx.agenthistory.LocateEventResponse;
import rs.ctx.agenthistory.AgentHistoryClient;
import rs.ctx.agenthistory.AgentHistoryOperation;
import rs.ctx.agenthistory.AgentHistoryOptions;
import rs.ctx.agenthistory.AgentHistoryTransport;
import rs.ctx.agenthistory.SearchResponse;
import rs.ctx.agenthistory.SearchClause;
import rs.ctx.agenthistory.SearchQuery;
import rs.ctx.agenthistory.ShowEventResponse;
import rs.ctx.agenthistory.StatusResponse;

public final class ToyAgentHistoryApp {
    public static void main(String[] args) {
        AgentHistoryClient client = AgentHistoryClient.withTransport(new FakeAgentHistoryTransport());

        StatusResponse status = client.status();
        SearchResponse search = client.search(AgentHistoryOptions.search()
                .query(SearchQuery.builder()
                        .any(SearchClause.all("local agent history"))
                        .must(SearchClause.all("codex"))
                        .build())
                .provider("codex")
                .historySource("codex/default")
                .providerKey("codex")
                .sourceId("default")
                .sourceFormat("codex_session_jsonl")
                .refresh("off")
                .limit(Integer.valueOf(5)));
        ShowEventResponse shown = client.showEvent("evt-toy-1", AgentHistoryOptions.showEvent().window(Integer.valueOf(1)));
        LocateEventResponse located = client.locateEvent("evt-toy-1");

        System.out.println("status.initialized=" + status.getStatus().getInitialized());
        System.out.println("search.results=" + search.getSearch().getResults().size());
        System.out.println("show.event=" + shown.getEvent().getEvent().getCtxEventId());
        System.out.println("locate.path=" + located.getLocation().getSource().getPath());
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
                    + "\"schema_version\":2,"
                    + "\"query\":{\"version\":\"ctx-search-v1\",\"any\":[{\"all\":\"local agent history\"}],\"must\":[{\"all\":\"codex\"}]},"
                    + "\"query_execution\":" + queryExecution() + ","
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
                    + "\"pagination\":{\"limit\":5},"
                    + "\"truncation\":{\"truncated\":false}"
                    + "}");
            responses.put("showEvent", "{"
                    + "\"event\":{\"ctx_event_id\":\"evt-toy-1\",\"ctx_session_id\":\"ses-toy-1\","
                    + "\"sequence\":1,\"event_type\":\"message\",\"role\":\"assistant\","
                    + "\"source\":\"codex\",\"text\":\"toy local agent history result\"},"
                    + "\"events\":[{\"ctx_event_id\":\"evt-toy-1\",\"ctx_session_id\":\"ses-toy-1\",\"sequence\":1}],"
                    + "\"source\":{\"path\":\"/tmp/ctx-jvm-toy/session.jsonl\",\"cursor\":\"line:1\",\"exists\":false}"
                    + "}");
            responses.put("locateEvent", "{"
                    + "\"ctx_session_id\":\"ses-toy-1\","
                    + "\"ctx_event_id\":\"evt-toy-1\","
                    + "\"provider\":\"codex\","
                    + "\"provider_session_id\":\"provider-toy-1\","
                    + "\"source\":{\"path\":\"/tmp/ctx-jvm-toy/session.jsonl\",\"cursor\":\"line:1\",\"exists\":false},"
                    + "\"resume\":{\"cursor\":\"line:1\"}"
                    + "}");
        }

        private static String queryExecution() {
            String limits = "{\"query_bytes\":8192,\"clauses\":32,\"analyzed_tokens_per_clause\":32,\"candidates_per_positive_seed\":1024,\"candidate_rows\":16384,\"retained_candidate_ids\":8192,\"residual_rows\":8192,\"verification_bytes\":16777216,\"verification_lookup_bytes\":16384,\"hydrated_rows\":256,\"hydration_input_bytes\":8388608,\"hydration_input_bytes_per_event\":65536,\"snippet_input_bytes\":8388608,\"returned_text_bytes\":524288,\"serialized_response_bytes\":2097152,\"results\":200,\"elapsed_ms\":1000}";
            String used = "{\"query_bytes\":24,\"clauses\":2,\"analyzed_tokens\":4,\"largest_analyzed_tokens_per_clause\":3,\"largest_positive_seed_candidates\":1,\"candidate_rows\":1,\"retained_candidate_ids\":1,\"residual_rows\":1,\"verification_bytes\":16,\"largest_verification_lookup_bytes\":16,\"hydrated_rows\":1,\"legacy_fallback_rows\":0,\"hydration_input_bytes\":32,\"largest_hydration_input_bytes\":32,\"snippet_input_bytes\":32,\"returned_results\":1,\"returned_text_bytes\":30,\"serialized_response_bytes\":1000,\"elapsed_ms\":2}";
            return "{\"query_version\":\"ctx-search-v1\",\"candidate_strategy\":\"bounded_fts\",\"resolved\":" + limits + ",\"consumed\":" + used + ",\"semantic\":{\"attempted\":false,\"required\":false,\"readiness\":\"unavailable\",\"effective_backend\":\"lexical\",\"requested_candidates\":0,\"eligible_candidates\":0,\"candidates_supplied\":0,\"candidates_consumed\":0,\"candidates_used\":0,\"coverage\":{},\"completeness\":\"not_attempted\",\"positive_text_rule_version\":\"ctx-search-positive-text-v1\"},\"rrf_k\":60,\"per_branch_candidate_rows\":1024,\"requested_result_limit\":5,\"result_limit\":5,\"max_result_limit\":200,\"clauses_executed\":2,\"verification_dropped\":0,\"filter_verification_dropped\":0,\"candidate_budget_exhausted\":false,\"timed_out\":false,\"truncated\":false}";
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
