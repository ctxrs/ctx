//! ATIF v1.8 (Agent Trajectory Interchange Format, Harbor RFC 0001) schema,
//! strict validator, source-backed importer, and exporter for ctx.

pub mod export;
pub mod import;
pub mod schema;

#[cfg(test)]
mod tests;

/// Canonical source format identifier for explicit ATIF imports.
pub const ATIF_SOURCE_FORMAT: &str = "atif_v1";

pub use export::{
    export_session_to_trajectory, export_trajectory_json_pretty, safe_trajectory_filename,
    AtifExportError, AtifExportOptions, AtifExportSession,
};
pub use import::{
    has_schema_version_field, parse_atif_bytes, parse_atif_str, project_trajectory_to_core_records,
    AtifDocumentLeaf, AtifDocumentTreeAdapter, AtifImportError, AtifLineageLink,
    AtifProjectionInput, AtifTreeAuthority, StoredAtifEmptyMessageStep, StoredAtifEnvelope,
    StoredAtifRecord, StoredAtifRecordKind, StoredAtifTrajectoryHeader, ATIF_MAX_DIRECTORY_DEPTH,
    ATIF_MAX_DISCOVERED_FILES, ATIF_MAX_FILE_BYTES, ATIF_PARSER_REVISION,
    ATIF_SOURCE_SCHEMA_VARIANT, ATIF_STORED_METADATA_VERSION,
};
pub use schema::{
    parse_iso8601_timestamp, Agent, AtifValidationError, AudioMediaType, AudioSource, ContentPart,
    FinalMetrics, ImageMediaType, ImageSource, MessageContent, Metrics, Observation,
    ObservationResult, ReasoningEffort, SchemaVersion, Step, StepSource, SubagentTrajectoryRef,
    ToolCall, Trajectory, MAX_SUBAGENT_NESTING_DEPTH,
};
