import { HOSTED_MEASUREMENT_KEYS } from "./product-keys";
import { GRAPH_DETAIL_KEYS, parseGraphDetails, SIFT_COHORT_KEYS, parseSiftCohort } from "./product-fact-contract";
import { WINDOW_KEYS, SIFT_SEMANTIC_KEYS, parseWindow, parseSiftSemantic } from "./product-window-contract";
// Closed Graph, Sift summary and server observations. Shared by ingress and Queue replay.
import {
  rejectUnknownKeys, requireBoolean, requireEnum, requireRecord, schemaError,
  SHARED_PROPERTY_KEYS, validateSharedProperties, DURATION_BUCKETS, type TelemetryScalar,
} from "./telemetry-contract";

export const NATIVE_DURATIONS = new Set([
  "lt_1ms", "1ms-5ms", "5ms-10ms", "10ms-25ms", "25ms-50ms", "50ms-100ms",
  "100ms-250ms", "250ms-1s", "1s-5s", "5s-30s", "30s-2m", "2m-10m", "10m-1h", "1h+",
]);
export const GRAPH_OPERATIONS = new Set(["index", "update", "check_update", "compact", "watch", "add", "import", "clone", "search", "show", "callers", "callees", "impact", "path", "stats", "analyze", "communities", "hubs", "diagnose", "benchmark", "label", "tree", "report", "export", "merge", "global", "save_result", "reflect", "prs", "provider", "cache", "install", "uninstall", "hook", "hook_guard", "switch", "introspect", "push", "invalid_request", "parse", "global_add", "global_remove", "global_list", "global_refresh", "global_search", "global_path", "provider_list", "provider_detect", "provider_template", "provider_setup", "provider_show", "provider_add", "provider_remove", "cache_inspect", "cache_remove", "introspect_postgres", "push_neo4j", "push_falkor_db", "serve", "query_graph", "get_node", "get_neighbors", "shortest_path", "graph_stats", "god_nodes", "get_community", "list_prs", "get_pr_impact", "triage_prs", "resource_stats", "resource_graph", "resource_report", "resource_hubs", "resource_communities", "resource_computed_communities", "resource_surprises", "resource_audit", "resource_questions", "initialize", "tools_list", "resources_list", "ping", "protocol"]);
export const SIFT_OPERATIONS = new Set(["compact", "restore", "run", "proxy", "filter", "read", "json", "summary", "err", "test", "gain", "config", "semantic", "discover", "ccusage", "hook", "rewrite", "recall", "unknown", "help", "version", "errors", "usage"]);
export const SERVER_OPERATIONS = new Set(["indexer_health","enroll", "principals", "credentials", "revoke_principal", "revoke_credential", "whoami", "invite", "grants", "revoke", "begin_upload", "upload_status", "upload_chunk", "publish", "cancel_publish", "withdraw", "remove", "receipt", "publications", "publication_state", "status", "search", "event", "session", "unmatched", "health", "revoke_member", "publication", "unknown"]);
const DELIVERY = new Set(["known_complete", "failed", "unknown", "not_attempted"]);
const FAILURE_STAGES = new Set(["parse", "prepare", "execute", "render", "output"]);
const FAILURE_CLASSES = new Set([
  "invalid_request", "not_found", "permission", "unauthorized", "forbidden", "conflict",
  "capacity", "timeout", "io", "store", "unsupported", "cancelled", "other",
]);
export const PRODUCT_COUNTS = new Set([
  "0", "1", "2-5", "6-20", "21-100", "101-1k", "1k-10k", "10k-100k", "100k-1m", "1m+",
]);
export const PRODUCT_BYTES = new Set([
  "0", "lt_100kb", "100kb-1mb", "1mb-10mb", "10mb-100mb", "100mb-1gb", "1gb-2gb",
  "2gb-5gb", "5gb-10gb", "10gb-25gb", "25gb-50gb", "50gb-100gb", "100gb+",
]);
const FRACTIONS = new Set([
  "increased", "unchanged", "lt_10pct", "10pct-25pct", "25pct-50pct", "50pct-75pct",
  "75pct-90pct", "90pct-100pct", "100pct",
]);
const TIMINGS = ["native_total_duration_bucket", "prepare_duration_bucket", "work_duration_bucket", "output_duration_bucket"];
const RESULT = ["result_count_bucket", "result_empty", "result_truncated"];
const FAILURE = ["product_failure_stage", "product_failure_class"];

const COMPLETION = [...TIMINGS, ...RESULT, ...FAILURE, "execution_result", "output_delivery", "output_kind"];
function graphKeys(): string[] { return [...COMPLETION, ...GRAPH_DETAIL_KEYS, "graph_operation", "graph_node_count_bucket", "graph_edge_count_bucket", "graph_files_processed_bucket"]; }
const REMOTE_COUNTS = ["remote_page_count_bucket", "remote_limit_count_bucket", "remote_coverage_lag_count_bucket"];
const REMOTE_BOOLS = ["remote_client_limited", "remote_complete", "remote_exhaustive", "remote_has_more", "remote_response_limited"];
const REMOTE_KEYS = [...COMPLETION, "remote_operation", "remote_query_duration_bucket", ...REMOTE_COUNTS, ...REMOTE_BOOLS];
const SERVER_KEYS = [...COMPLETION, "server_operation", "response_class", "response_handoff"];
const RUNTIME_KEYS = ["runtime_kind", "runtime_phase", "uptime_bucket", "active_requests_bucket", "pending_work_bucket", ...FAILURE];
const SUMMARY_COUNTS = ["observed_count_bucket", "execution_failed_count_bucket", "output_failed_count_bucket", "complete_measurement_count_bucket", "partial_measurement_count_bucket", "unmeasured_count_bucket"];
const LATENCY_COUNTS = Array.from({ length: 14 }, (_, index) => `sift_latency_${index}_count_bucket`);
const PAIR_KEYS = (unit: string) => ["measured_count_bucket", "input_bucket", "output_bucket", "delta_bucket", "change", "savings_fraction_bucket"].map((suffix) => `sift_${unit}_${suffix}`);
function summaryKeys(): string[] { return [...SIFT_COHORT_KEYS, ...SIFT_SEMANTIC_KEYS,"sift_host", "sift_execution_outcome", "sift_operation", "sift_mode", "collection_scope", "collection_limited", "observation_window_bucket", "sift_token_basis", "latency_measured_count_bucket", ...SUMMARY_COUNTS, ...LATENCY_COUNTS, ...PAIR_KEYS("bytes"), ...PAIR_KEYS("tokens")]; }
export function isProductPropertyKey(key: string): boolean { return [...graphKeys(), ...SERVER_KEYS, ...RUNTIME_KEYS, ...summaryKeys(), ...WINDOW_KEYS, ...REMOTE_KEYS].includes(key); }
type Props = Record<string, TelemetryScalar>;
export function invalid(): never { throw schemaError("invalid_product_shape"); }
export function vocabulary(value: unknown, values: ReadonlySet<string>): string {
  return requireEnum(value, values, "invalid_product_value");
}
function optional(p: Record<string, unknown>, out: Props, key: string, values: ReadonlySet<string>): void {
  if (Object.hasOwn(p, key)) out[key] = vocabulary(p[key], values);
}
function optionalCounts(p: Record<string, unknown>, out: Props, keys: readonly string[]): void {
  for (const key of keys) optional(p, out, key, PRODUCT_COUNTS);
}
function result(p: Record<string, unknown>, out: Props): void {
  if (Object.hasOwn(p, "result_count_bucket") !== Object.hasOwn(p, "result_empty")) invalid();
  if (Object.hasOwn(p, "result_count_bucket")) {
    out.result_count_bucket = vocabulary(p.result_count_bucket, PRODUCT_COUNTS);
    out.result_empty = requireBoolean(p.result_empty, "invalid_product_value");
    if (out.result_empty !== (out.result_count_bucket === "0")) invalid();
  }
  if (Object.hasOwn(p, "result_truncated")) {
    if (!Object.hasOwn(p, "result_count_bucket")) invalid();
    out.result_truncated = requireBoolean(p.result_truncated, "invalid_product_value");
  }
}
function failure(p: Record<string, unknown>, out: Props, failed: boolean): void {
  if (FAILURE.some((key) => Object.hasOwn(p, key) !== failed)) invalid();
  if (failed) {
    out.product_failure_stage = vocabulary(p.product_failure_stage, FAILURE_STAGES);
    out.product_failure_class = vocabulary(p.product_failure_class, FAILURE_CLASSES);
  }
}
function timings(p: Record<string, unknown>, out: Props): void {
  out.native_total_duration_bucket = vocabulary(p.native_total_duration_bucket, NATIVE_DURATIONS);
  for (const key of TIMINGS.slice(1)) optional(p, out, key, NATIVE_DURATIONS);
}
function completion(p: Record<string, unknown>, out: Props, outcome: string): void {
  timings(p, out); result(p, out);
  out.execution_result = vocabulary(p.execution_result, new Set(["success", "failure"]));
  out.output_delivery = vocabulary(p.output_delivery, DELIVERY);
  out.output_kind = vocabulary(p.output_kind, new Set(["human", "json", "bytes", "mcp", "http", "none"]));
  const failed = out.execution_result === "failure" || out.output_delivery === "failed";
  if ((outcome === "failure") !== failed) invalid();
  failure(p, out, failed);
  if (failed && out.execution_result === "success" && (out.product_failure_stage !== "output" || out.product_failure_class !== "io")) invalid();
}
export function parseProductOperation(value: unknown, surface: string, operation: string, outcome: string): Props {
  const p = requireRecord(value, "invalid_properties");
  const graph = operation === "graph" && (surface === "cli" || surface === "mcp");
  const server = operation === "server_request" && surface === "server";
  const remote = operation === "remote" && (surface === "cli" || surface === "mcp");
  if (!graph && !server && !remote) invalid();
  rejectUnknownKeys(p, new Set([...SHARED_PROPERTY_KEYS, ...(graph ? graphKeys() : remote ? REMOTE_KEYS : SERVER_KEYS)]), "unknown_product_property");
  const out = validateSharedProperties(p);
  completion(p, out, outcome);
  if (graph) {
    Object.assign(out, parseGraphDetails(p));
    out.graph_operation = vocabulary(p.graph_operation, GRAPH_OPERATIONS);
    if (surface === "mcp" ? out.output_kind !== "mcp" : new Set(["mcp", "http"]).has(String(out.output_kind))) invalid();
    optionalCounts(p, out, ["graph_node_count_bucket", "graph_edge_count_bucket", "graph_files_processed_bucket"]);
  } else if (remote) {
    out.remote_operation = vocabulary(p.remote_operation, new Set(["search", "event", "session", "status", "unsupported"]));
    if (surface === "mcp" ? out.output_kind !== "mcp" : new Set(["mcp", "http"]).has(String(out.output_kind))) invalid();
    optionalCounts(p, out, REMOTE_COUNTS);
    for (const key of REMOTE_BOOLS) if (Object.hasOwn(p,key)) out[key] = requireBoolean(p[key], "invalid_product_value");
    optional(p,out,"remote_query_duration_bucket",NATIVE_DURATIONS);
  } else {
    out.server_operation = vocabulary(p.server_operation, SERVER_OPERATIONS);
    out.response_class = vocabulary(p.response_class, new Set(["2xx", "3xx", "4xx", "5xx", "not_produced", "unavailable", "other"]));
    out.response_handoff = vocabulary(p.response_handoff, DELIVERY);
    if (out.output_kind !== "http" || out.output_delivery !== out.response_handoff) invalid();
    if (new Set(["4xx", "5xx", "not_produced", "unavailable"]).has(String(out.response_class)) && out.execution_result !== "failure") invalid();
    if (new Set(["not_produced", "unavailable"]).has(String(out.response_class)) && out.response_handoff === "known_complete") invalid();
  }
  return out;
}
export function parseHostedMeasurements(p: Record<string, unknown>, outcome: string): Props {
  if (![...HOSTED_MEASUREMENT_KEYS].some((key) => Object.hasOwn(p, key))) return {};
  const out: Props = {};
  timings(p, out); result(p, out);
  out.output_delivery = vocabulary(p.output_delivery, DELIVERY);
  if ((out.output_delivery === "failed") !== (outcome === "failure" && p.hosted_failure_stage === "output")) invalid();
  return out;
}
export function parseProductRuntime(value: unknown, surface: string, operation: string, outcome: string): Props {
  const p = requireRecord(value, "invalid_properties");
  if (operation === "sift_summary") {
    if (surface !== (p.sift_entry === "mcp" ? "mcp" : "cli") || outcome !== "success") invalid();
    return summary(p);
  }
  if (operation === "server_summary" || operation === "sharing_summary") {
    if (surface !== (operation === "server_summary" ? "server" : "daemon") || outcome !== "success") invalid();
    return parseWindow(p,operation);
  }
  if (operation !== "product_runtime") invalid();
  rejectUnknownKeys(p, new Set([...SHARED_PROPERTY_KEYS, ...RUNTIME_KEYS]), "unknown_product_property");
  const out = validateSharedProperties(p);
  out.runtime_kind = vocabulary(p.runtime_kind, new Set(["server", "graph_serve", "graph_watch"]));
  out.runtime_phase = vocabulary(p.runtime_phase, new Set(["ready", "liveness", "stopped", "failed", "shutting_down", "recovered"]));
  const expected = out.runtime_kind === "server" ? "server" : out.runtime_kind === "graph_serve" ? "mcp" : "cli";
  if (surface !== expected || (outcome === "failure") !== (out.runtime_phase === "failed")) invalid();
  out.uptime_bucket = vocabulary(p.uptime_bucket, new Set(["lt_1h", "1h-1d", "1d-7d", "7d+"]));
  failure(p, out, outcome === "failure");
  optionalCounts(p, out, ["active_requests_bucket", "pending_work_bucket"]);
  return out;
}
const COUNT_RANGES: Record<string, [number, number]> = {
  "0": [0,0], "1": [1,1], "2-5": [2,5], "6-20": [6,20], "21-100": [21,100],
  "101-1k": [101,1000], "1k-10k": [1001,10000], "10k-100k": [10001,100000],
  "100k-1m": [100001,1000000], "1m+": [1000001,Infinity],
};
function range(value: TelemetryScalar | undefined): [number, number] { return COUNT_RANGES[String(value)]!; }
export function noGreater(a: TelemetryScalar | undefined, b: TelemetryScalar | undefined): void {
  if (range(a)[0] > range(b)[1]) invalid();
}
export function partition(parts: (TelemetryScalar | undefined)[], total: TelemetryScalar | undefined): void {
  const lower = parts.reduce<number>((sum, value) => sum + range(value)[0], 0);
  const upper = parts.reduce<number>((sum, value) => sum + range(value)[1], 0);
  if (lower > range(total)[1] || upper < range(total)[0]) invalid();
}
function summary(p: Record<string, unknown>): Props {
  rejectUnknownKeys(p, new Set([...SHARED_PROPERTY_KEYS, ...summaryKeys()]), "unknown_product_property");
  const out = validateSharedProperties(p);
  out.sift_operation = vocabulary(p.sift_operation, SIFT_OPERATIONS);
  out.sift_host = vocabulary(p.sift_host, new Set(["unknown", "standalone", "claude", "codex", "cursor", "copilot", "gemini", "hermes", "vscode", "droid", "vibe", "pi", "omp", "open_code", "kilo"]));
  out.sift_execution_outcome = vocabulary(p.sift_execution_outcome, new Set(["success", "failure", "mixed"]));
  out.sift_mode = vocabulary(p.sift_mode, new Set(["lossless", "presentation", "raw", "streaming", "semantic", "restore", "not_applicable", "automatic", "capture", "explicit_view", "rewrite", "control"]));
  out.collection_scope = vocabulary(p.collection_scope, new Set(["best_effort_observed"]));
  out.collection_limited = requireBoolean(p.collection_limited, "invalid_product_value");
  out.observation_window_bucket = vocabulary(p.observation_window_bucket, DURATION_BUCKETS);
  for (const key of SUMMARY_COUNTS) out[key] = vocabulary(p[key], PRODUCT_COUNTS);
  if (out.observed_count_bucket === "0") invalid();
  if ((out.sift_execution_outcome === "success") !== (out.execution_failed_count_bucket === "0")) invalid();
  if (out.sift_execution_outcome === "failure" && out.execution_failed_count_bucket !== out.observed_count_bucket) invalid();
  partition([out.complete_measurement_count_bucket, out.partial_measurement_count_bucket, out.unmeasured_count_bucket], out.observed_count_bucket);
  for (const key of SUMMARY_COUNTS.slice(1)) noGreater(out[key], out.observed_count_bucket);
  for (const unit of ["bytes", "tokens"]) measuredPair(p, out, unit);
  Object.assign(out, parseSiftCohort(p), parseSiftSemantic(p,out.observed_count_bucket));
  const hasTokens = Object.hasOwn(out, "sift_tokens_input_bucket");
  if (Object.hasOwn(p, "sift_token_basis") !== hasTokens) invalid();
  if (hasTokens) out.sift_token_basis = vocabulary(p.sift_token_basis, new Set(["o200k_base_v1"]));
  const latency = ["latency_measured_count_bucket", ...LATENCY_COUNTS];
  if (latency.some((key) => Object.hasOwn(p, key))) {
    for (const key of latency) out[key] = vocabulary(p[key], PRODUCT_COUNTS);
    partition(LATENCY_COUNTS.map((key) => out[key]), out.latency_measured_count_bucket);
    noGreater(out.latency_measured_count_bucket, out.observed_count_bucket);
  }
  return out;
}
function measuredPair(p: Record<string, unknown>, out: Props, unit: string): void {
  const keys = PAIR_KEYS(unit);
  if (!keys.some((key) => Object.hasOwn(p, key))) return;
  const base = `sift_${unit}_`;
  out[`${base}measured_count_bucket`] = vocabulary(p[`${base}measured_count_bucket`], PRODUCT_COUNTS);
  if (out[`${base}measured_count_bucket`] === "0") invalid();
  noGreater(out[`${base}measured_count_bucket`], out.complete_measurement_count_bucket);
  for (const key of ["input", "output", "delta"]) out[`${base}${key}_bucket`] = vocabulary(p[`${base}${key}_bucket`], unit === "bytes" ? PRODUCT_BYTES : PRODUCT_COUNTS);
  const change = vocabulary(p[`${base}change`], new Set(["increased", "reduced", "unchanged"]));
  out[`${base}change`] = change;
  const inputZero = out[`${base}input_bucket`] === "0";
  const outputZero = out[`${base}output_bucket`] === "0";
  if ((change === "unchanged") !== (out[`${base}delta_bucket`] === "0")) invalid();
  if (change === "unchanged" && out[`${base}input_bucket`] !== out[`${base}output_bucket`]) invalid();
  if ((inputZero && change === "reduced") || (outputZero && change === "increased")) invalid();
  // Ordered buckets can prove some contradictions without inventing exact values.
  const buckets = [...(unit === "bytes" ? PRODUCT_BYTES : PRODUCT_COUNTS)];
  const inputIndex = buckets.indexOf(String(out[`${base}input_bucket`]));
  const outputIndex = buckets.indexOf(String(out[`${base}output_bucket`]));
  const deltaIndex = buckets.indexOf(String(out[`${base}delta_bucket`]));
  if ((change === "reduced" && outputIndex > inputIndex)
    || (change === "increased" && outputIndex < inputIndex)
    || deltaIndex > Math.max(inputIndex, outputIndex)) invalid();
  if (unit === "tokens" && inputIndex === outputIndex && inputIndex <= 1 && change !== "unchanged") invalid();
  const ratio = `${base}savings_fraction_bucket`;
  if (Object.hasOwn(p, ratio) === inputZero) invalid();
  if (!inputZero) {
    out[ratio] = vocabulary(p[ratio], FRACTIONS);
    if (change === "reduced" ? new Set(["increased", "unchanged"]).has(String(out[ratio])) : out[ratio] !== change) invalid();
    if ((out[ratio] === "100pct") !== outputZero) invalid();
  }
}
