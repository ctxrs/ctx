use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::captured_batch::SourceObservation;
use crate::provider::sqlite::ProviderSqliteSourceSnapshot;
use crate::{CaptureError, Result, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT};

use super::{GOOSE_CAPTURE_REVISION, GOOSE_POLICY_REVISION};

pub(super) fn goose_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Goose SQLite source must be a regular non-symlink file",
        "Goose SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn goose_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_version: Option<i64>,
    schema_fingerprint: &str,
) -> String {
    format!(
        "goose-sqlite-snapshot-v1:capture={GOOSE_CAPTURE_REVISION};policy={GOOSE_POLICY_REVISION};user_version={user_version};schema_version={};schema={schema_fingerprint};{}",
        schema_version.map_or_else(|| "none".to_owned(), |value| value.to_string()),
        snapshot.revision_component(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn goose_source_observation(
    snapshot: &ProviderSqliteSourceSnapshot,
    cursor_path: &str,
    cursor_stream: String,
    user_version: i64,
    schema_version: Option<i64>,
    schema_fingerprint: &str,
    inventory_observation_token: Option<&str>,
) -> Result<SourceObservation> {
    SourceObservation::new(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        format!("goose-sqlite:{cursor_path}"),
        goose_source_revision(snapshot, user_version, schema_version, schema_fingerprint),
        cursor_stream,
        GOOSE_CAPTURE_REVISION,
        GOOSE_POLICY_REVISION,
        inventory_observation_token,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}
