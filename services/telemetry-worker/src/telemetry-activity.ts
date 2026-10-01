import type { TelemetryScalar } from "./telemetry-contract";
import type { TelemetryActivityClass } from "./telemetry-ingest";

export function classifyActivity(
  eventName: string,
  surface: string,
  operation: string,
  outcome: string,
  properties: Record<string, TelemetryScalar>,
): TelemetryActivityClass {
  if (eventName === "analytics_delivery_observation") return "operational";
  if (eventName === "provider_refresh_completed") {
    if (properties.trigger === "setup") return "setup";
    if (
      properties.refresh_result === "failure" ||
      (typeof properties.failure_scope === "string" && properties.failure_scope !== "none") ||
      (typeof properties.failure_type === "string" && properties.failure_type !== "none")
    ) return "operational";
    return "automatic";
  }
  if (eventName === "runtime_observation") {
    return operation === "liveness" || properties.runtime_phase === "liveness" ? "liveness" : "operational";
  }
  if (operation === "graph") {
    if (new Set(["install", "uninstall", "hook", "provider", "switch"]).has(String(properties.graph_operation))) return "setup";
    if (new Set(["stats", "check_update", "diagnose", "benchmark"]).has(String(properties.graph_operation))) return "status";
    if (properties.graph_operation === "watch") return "automatic";
    return outcome === "success" && properties.output_delivery === "known_complete" && properties.result_empty === false ? "product_value" : "product_activity";
  }
  if (operation === "remote") {
    if (properties.remote_operation === "status") return "status";
    return outcome === "success" && properties.output_delivery === "known_complete" && properties.result_empty === false ? "product_value" : "product_activity";
  }
  if (surface === "server") return "operational";
  if (new Set(["remote_status", "archive_verify", "server_status", "server_publications", "server_user_list", "server_user_credentials"]).has(operation)) return "status";
  if (new Set(["remote_pause", "remote_resume", "remote_remove", "server_collection_create", "server_user_create", "server_user_credential"]).has(operation)) return "setup";
  if (surface === "daemon") {
    if (operation === "status") return "status";
    if (operation === "enable" || operation === "disable") return "setup";
    return "automatic";
  }
  if ((surface === "cli" || surface === "mcp") && operation === "blame"
    && Object.hasOwn(properties, "blame_target_kind")) {
    return outcome === "success" && properties.blame_output_served === true
      ? "product_value" : "product_activity";
  }
  if (surface === "mcp") {
    if (operation === "missing" || operation === "unknown") return "operational";
    if (operation === "status") return "status";
    if (operation === "pro_status") return "operational";
    if (
      outcome === "success" &&
      ((operation === "search" && properties.zero_result === false) ||
        // `sql` classifies already-emitted historical MCP events only.
        new Set([
          "sql", "show_session", "show_event", "blame",
        ]).has(operation))
    ) return "product_value";
    return "product_activity";
  }
  if (surface === "pro_host") {
    if (operation === "lifecycle") return "operational";
    if (operation === "materialize") return "automatic";
    if (operation === "status") return "operational";
    if (operation === "query") {
      return outcome === "success" && properties.query_empty === false
        ? "product_value"
        : "product_activity";
    }
    if (operation === "blame" && outcome === "success") {
      if (properties.blame_schema_version === 1) {
        return properties.blame_output_served === true
          && new Set(["proven", "possible", "conflicting", "none"])
            .has(String(properties.blame_result_state))
          ? "product_value"
          : "product_activity";
      }
      if (
        properties.blame_schema_version === 2
        && new Set(["proven", "possible", "conflicting", "none"])
          .has(String(properties.blame_result_state))
      ) return "product_value";
      if (properties.blame_result_count_bucket !== "0") return "product_value";
    }
    return "product_activity";
  }
  if (operation === "integration") {
    return properties.integration_action === "status" ? "status" : "setup";
  }
  if (operation === "daemon") {
    return properties.daemon_command === "run" ? "automatic" : "setup";
  }
  if (operation === "index" && properties.index_operation === "status") return "status";
  if (operation === "upgrade") {
    if (properties.upgrade_mode === "auto") return "automatic";
    return new Set(["check", "status"]).has(String(properties.upgrade_operation))
      ? "status"
      : "product_activity";
  }
  if (
    new Set([
      "setup", "enable", "disable", "semantic_enable", "semantic_disable",
    ]).has(operation)
  ) return "setup";
  if (new Set(["status", "doctor", "sources", "semantic_status"]).has(operation)) {
    return "status";
  }
  if (operation === "run_once") return "automatic";
  // `sql` remains here solely to classify already-emitted historical rows.
  if (outcome === "success" && new Set(["show", "locate", "sql"]).has(operation)) {
    return "product_value";
  }
  if (
    outcome === "success" &&
    operation === "search" && properties.zero_result === false
  ) return "product_value";
  return "product_activity";
}
