// Closed optional diagnostics measured at engine boundaries. Shared HTTP/Queue validation.
import { requireBoolean, type TelemetryScalar } from "./telemetry-contract";
import { vocabulary, PRODUCT_COUNTS, PRODUCT_BYTES, NATIVE_DURATIONS, invalid, noGreater } from "./product-operation-contract";
type Props = Record<string, TelemetryScalar>;
export const GRAPH_COUNTS = ["graph_polls_count_bucket", "graph_retries_count_bucket", "graph_query_unresolved_count_bucket", "graph_query_seeds_count_bucket", "graph_index_parsed_count_bucket", "graph_index_rejected_count_bucket", "graph_index_unchanged_count_bucket", "graph_index_deleted_count_bucket", "graph_index_diagnostics_count_bucket", "graph_analysis_communities_count_bucket", "graph_analysis_passes_count_bucket", "graph_analysis_unsatisfied_constraints_count_bucket", "graph_semantic_reserved_generations_count_bucket", "graph_semantic_reserved_output_tokens_count_bucket", "graph_semantic_receipts_count_bucket", "graph_semantic_input_known_count_bucket", "graph_semantic_input_reporters_count_bucket", "graph_semantic_output_known_count_bucket", "graph_semantic_output_reporters_count_bucket", "graph_semantic_total_known_count_bucket", "graph_semantic_total_reporters_count_bucket", "graph_semantic_cache_read_known_count_bucket", "graph_semantic_cache_read_reporters_count_bucket", "graph_semantic_cache_create_known_count_bucket", "graph_semantic_cache_create_reporters_count_bucket", "graph_semantic_reasoning_known_count_bucket", "graph_semantic_reasoning_reporters_count_bucket"];
export const GRAPH_BOOLEANS = ["graph_bound_seed", "graph_bound_node", "graph_bound_work", "graph_bound_depth", "graph_bound_unresolved", "graph_bound_token", "graph_bound_other", "graph_index_fresh", "graph_analysis_pagerank_converged", "graph_artifact_snapshot_cache_hit", "graph_artifact_analysis_cache_hit", "graph_artifact_committed", "graph_semantic_configured", "graph_semantic_usage_unavailable"];
export const GRAPH_TIMINGS = ["graph_query_duration_bucket", "graph_index_capture_duration_bucket", "graph_index_detect_duration_bucket", "graph_index_extract_duration_bucket", "graph_index_commit_duration_bucket", "graph_analysis_duration_bucket"];
export const GRAPH_BYTES = ["graph_artifact_bytes_bucket"];
export const GRAPH_ENUMS: Record<string, readonly string[]> = {
  "graph_invocation": [
    "cli",
    "scoped_search",
    "unified_mcp",
    "native_stdio",
    "native_http",
    "library"
  ],
  "graph_phase": [
    "parse",
    "prepare",
    "discover",
    "open",
    "capture",
    "detect",
    "extract",
    "commit",
    "post_commit",
    "query",
    "snapshot",
    "analysis",
    "render",
    "artifact_write",
    "output_write",
    "output_flush",
    "registration",
    "bind",
    "protocol",
    "admission",
    "worker",
    "shutdown"
  ],
  "graph_output_boundary": [
    "unobserved",
    "cli_flush",
    "stdio_flush",
    "http_body"
  ],
  "graph_failure_phase": [
    "parse",
    "prepare",
    "discover",
    "open",
    "capture",
    "detect",
    "extract",
    "commit",
    "post_commit",
    "query",
    "snapshot",
    "analysis",
    "render",
    "artifact_write",
    "output_write",
    "output_flush",
    "registration",
    "bind",
    "protocol",
    "admission",
    "worker",
    "shutdown"
  ],
  "graph_failure_kind": [
    "invalid_input",
    "missing_index",
    "not_found",
    "permission",
    "io",
    "store_open",
    "store_busy",
    "invalid_store",
    "unsupported_store",
    "concurrent_change",
    "endpoint_not_found",
    "endpoint_ambiguous",
    "work_limit",
    "response_limit",
    "input_rejected",
    "unknown_project",
    "capacity",
    "worker",
    "serialize",
    "broken_pipe",
    "authentication",
    "protocol",
    "unknown"
  ],
  "graph_query_path": [
    "found",
    "not_found_within_scope",
    "incomplete"
  ],
  "graph_index_disposition": [
    "no_op",
    "committed"
  ],
  "graph_analysis_algorithm": [
    "leiden",
    "louvain"
  ],
  "graph_analysis_convergence": [
    "converged",
    "not_converged",
    "unknown"
  ],
  "graph_artifact_format": [
    "snapshot_json",
    "graphify_json",
    "graph_ml",
    "cypher",
    "mermaid",
    "svg",
    "html",
    "markdown",
    "canvas",
    "callflow_html",
    "tree_html",
    "wiki",
    "obsidian"
  ]
};
export const SIFT_ENUMS: Record<string, readonly string[]> = {
  "sift_entry": [
    "mcp",
    "direct",
    "completion_hook",
    "pre_hook",
    "json_protocol",
    "pi_session_v1",
    "pi_session_v2"
  ],
  "sift_terminal": [
    "invocation",
    "protocol_request",
    "protocol_session"
  ],
  "sift_outcome": [
    "success",
    "skipped",
    "fail_open",
    "failure"
  ],
  "sift_delivery": [
    "not_applicable",
    "not_attempted",
    "flushed",
    "unchanged",
    "failed"
  ],
  "sift_skip": [
    "explicit_raw",
    "disabled",
    "excluded",
    "settings_unavailable",
    "interactive",
    "small",
    "binary",
    "streaming_deadline",
    "capture_limit",
    "envelope_limit",
    "unsupported_host",
    "unsupported_tool",
    "unsupported_event",
    "unsupported_shell",
    "unsupported_syntax",
    "unsupported_metadata",
    "malformed_input",
    "already_wrapped",
    "tokenizer_unavailable",
    "not_smaller",
    "no_selection"
  ],
  "sift_failure_phase": [
    "arguments",
    "settings",
    "input",
    "process_setup",
    "spawn",
    "child_read",
    "child_wait",
    "codec",
    "render",
    "protocol",
    "output",
    "other"
  ],
  "sift_failure_kind": [
    "invalid_input",
    "not_found",
    "permission_denied",
    "broken_pipe",
    "interrupted",
    "timed_out",
    "io",
    "tokenizer",
    "other"
  ],
  "sift_child": [
    "not_applicable",
    "unknown",
    "exited_zero",
    "exited_nonzero",
    "signalled",
    "cancelled",
    "spawn_not_found",
    "spawn_denied",
    "spawn_failed"
  ],
  "sift_missingness": [
    "inherited",
    "streaming",
    "small",
    "binary",
    "view_not_tokenized",
    "tokenizer_unavailable",
    "incomplete",
    "not_applicable",
    "unknown"
  ]
};
export const GRAPH_DETAIL_KEYS = [...GRAPH_COUNTS, ...GRAPH_BOOLEANS, ...GRAPH_TIMINGS, ...GRAPH_BYTES, ...Object.keys(GRAPH_ENUMS)];
export const SIFT_COHORT_KEYS = Object.keys(SIFT_ENUMS);
function enumFields(p: Record<string, unknown>, fields: Record<string, readonly string[]>): Props {
  const out: Props = {};
  for (const [key, values] of Object.entries(fields)) if (Object.hasOwn(p,key)) out[key] = vocabulary(p[key],new Set(values));
  return out;
}
export function parseGraphDetails(p: Record<string, unknown>): Props {
  const out = enumFields(p,GRAPH_ENUMS);
  for (const [keys,values] of [[GRAPH_COUNTS,PRODUCT_COUNTS],[GRAPH_TIMINGS,NATIVE_DURATIONS],[GRAPH_BYTES,PRODUCT_BYTES]] as const)
    for (const key of keys) if (Object.hasOwn(p,key)) out[key]=vocabulary(p[key],values);
  for (const key of GRAPH_BOOLEANS) if (Object.hasOwn(p,key)) out[key]=requireBoolean(p[key],"invalid_product_value");
  const bounds = GRAPH_BOOLEANS.filter((key)=>key.startsWith("graph_bound_"));
  if (bounds.some((k)=>Object.hasOwn(p,k)) && !bounds.every((k)=>Object.hasOwn(p,k))) invalid();
  if (Object.hasOwn(p,"graph_failure_phase") !== Object.hasOwn(p,"graph_failure_kind")) invalid();
  if (Object.hasOwn(p,"graph_failure_kind") && p.execution_result !== "failure" && p.output_delivery !== "failed") invalid();
  if (Object.keys(p).some((k)=>k.startsWith("graph_semantic_"))) {
    out.graph_semantic_usage_unavailable=requireBoolean(p.graph_semantic_usage_unavailable,"invalid_product_value");
    for (const kind of ["input","output","total","cache_read","cache_create","reasoning"]) {
      const reporters=`graph_semantic_${kind}_reporters_count_bucket`, known=`graph_semantic_${kind}_known_count_bucket`;
      out[reporters]=vocabulary(p[reporters],PRODUCT_COUNTS);
      if ((out[reporters] !== "0") !== Object.hasOwn(out,known)) invalid();
      if (Object.hasOwn(out,"graph_semantic_receipts_count_bucket")) noGreater(out[reporters],out.graph_semantic_receipts_count_bucket);
    }
  }
  return out;
}
export function parseSiftCohort(p: Record<string, unknown>): Props {
  const out=enumFields(p,SIFT_ENUMS);
  if (Object.hasOwn(p,"sift_failure_phase") !== Object.hasOwn(p,"sift_failure_kind")) invalid();
  // A child nonzero exit is not itself fail-open. Preserve both closed facts.
  if (p.sift_outcome === "success" && Object.hasOwn(p,"sift_failure_kind")) invalid();
  return out;
}
