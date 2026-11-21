//! ATIF v1.0-v1.8 parsing, CoreRecord projection, and source-backed import adapter.

mod projection;
mod source_backed;

pub use projection::{
    project_trajectory_to_core_records, AtifLineageLink, AtifProjectionInput,
    StoredAtifEmptyMessageStep, StoredAtifEnvelope, StoredAtifRecord, StoredAtifRecordKind,
    StoredAtifTrajectoryHeader, ATIF_STORED_METADATA_VERSION,
};
pub(crate) use source_backed::{path_to_posix_key, resolve_local_relative_path};
pub use source_backed::{
    AtifDocumentLeaf, AtifDocumentTreeAdapter, AtifTreeAuthority, ATIF_MAX_DIRECTORY_DEPTH,
    ATIF_MAX_DISCOVERED_FILES, ATIF_MAX_FILE_BYTES, ATIF_PARSER_REVISION,
    ATIF_SOURCE_SCHEMA_VARIANT,
};

use thiserror::Error;

use crate::schema::{AtifValidationError, Trajectory};

/// Errors produced when parsing or validating ATIF trajectory inputs.
#[derive(Debug, Error)]
pub enum AtifImportError {
    #[error("invalid ATIF JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] AtifValidationError),
    #[error(
        "unsupported ATIF schema_version `{0}`; supported versions are ATIF-v1.0 through ATIF-v1.8"
    )]
    UnsupportedSchemaVersion(String),
    #[error("ATIF trajectory document must be a JSON object with a string `schema_version`")]
    NotAtifObject,
}

/// Inspects parsed JSON to decide whether a file in a directory scan claims to
/// be an ATIF trajectory (any object carrying a `schema_version` field).
pub fn has_schema_version_field(bytes: &[u8]) -> Result<bool, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    Ok(value
        .as_object()
        .is_some_and(|object| object.contains_key("schema_version")))
}

/// Parses and strictly validates one ATIF v1.0-v1.8 trajectory JSON payload.
pub fn parse_atif_bytes(bytes: &[u8]) -> Result<Trajectory, AtifImportError> {
    let raw: serde_json::Value = serde_json::from_slice(bytes)?;
    let Some(object) = raw.as_object() else {
        return Err(AtifImportError::NotAtifObject);
    };
    if let Some(version) = object.get("schema_version").and_then(|v| v.as_str()) {
        if !matches!(
            version,
            "ATIF-v1.0"
                | "ATIF-v1.1"
                | "ATIF-v1.2"
                | "ATIF-v1.3"
                | "ATIF-v1.4"
                | "ATIF-v1.5"
                | "ATIF-v1.6"
                | "ATIF-v1.7"
                | "ATIF-v1.8"
        ) {
            return Err(AtifImportError::UnsupportedSchemaVersion(
                version.to_owned(),
            ));
        }
    }
    let trajectory: Trajectory = serde_json::from_value(raw)?;
    trajectory.validate()?;
    Ok(trajectory)
}

/// Parses and strictly validates one ATIF v1.0-v1.8 trajectory JSON string.
///
/// ```
/// use ctx_history_atif::{parse_atif_str, SchemaVersion};
///
/// let json = r#"{
///   "schema_version": "ATIF-v1.8",
///   "session_id": "run-1",
///   "agent": { "name": "demo-agent", "version": "1.0.0" },
///   "steps": [
///     { "step_id": 1, "source": "user", "message": "Hello" },
///     { "step_id": 2, "source": "agent", "message": "Hi there!" }
///   ]
/// }"#;
/// let trajectory = parse_atif_str(json).expect("valid ATIF v1.8");
/// assert_eq!(trajectory.schema_version, SchemaVersion::V1_8);
/// assert_eq!(trajectory.steps.len(), 2);
/// ```
pub fn parse_atif_str(input: &str) -> Result<Trajectory, AtifImportError> {
    parse_atif_bytes(input.as_bytes())
}
