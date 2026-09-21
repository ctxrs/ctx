// Public main briefly retained the 0.25.0 package version after the v0.25 tag
// while import outcomes were made explicit, before typed-v1 telemetry replaced
// cli_invocation. Keep that exact bounded transition compatible without
// admitting arbitrary post-release legacy properties.
export const V025_TRANSITION_IMPORT_KEYS = [
  "rejected_records_bucket", "import_outcome", "import_failure_scope", "import_failure_type",
] as const;

export const V025_TRANSITION_PROPERTY_ENUMS = new Map<string, ReadonlySet<string>>([
  ["import_outcome", new Set([
    "success", "failure", "completed_with_rejections", "completed_with_source_failures",
    "completed_with_rejections_and_source_failures",
  ])],
  ["import_failure_scope", new Set([
    "none", "record", "source", "record_and_source", "system",
  ])],
  ["import_failure_type", new Set([
    "none", "record_rejection", "source_failure", "record_rejection_and_source_failure",
    "unsupported_schema", "not_found", "permission", "source_database", "malformed_source",
    "store", "worker_panic", "system_io", "system", "other",
  ])],
]);
