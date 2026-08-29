use std::path::Path;

use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
};
use ctx_history_core::{CoreRecordError, SourceKey};

use super::super::{json::OpenCodeJsonProjection, model::OpenCodeNativeRejectionKind};
use crate::provider::providers::opencode::OpenCodeSqliteDialect;

pub(super) fn record_local_core_projection_failure(error: &CoreRecordError) -> bool {
    matches!(
        error,
        CoreRecordError::FieldTooLarge {
            field: "normalized_body" | "structured_content" | "selected_content",
            ..
        }
    )
}

pub(super) fn projection_rejection_draft(
    source: &SourceKey,
    dialect: &OpenCodeSqliteDialect,
    path: &Path,
    source_rowid: i64,
    projection: &OpenCodeJsonProjection,
) -> Option<SourceBackedRecordRejectionDraft> {
    let kind = match projection {
        OpenCodeJsonProjection::Rejected(kind)
        | OpenCodeJsonProjection::RejectedWithReason(kind, _) => *kind,
        OpenCodeJsonProjection::Retained(_) | OpenCodeJsonProjection::Output(_) => return None,
    };
    let detail = format!(
        "{} SQLite row {}",
        dialect.display_name,
        rejection_detail(kind)
    );
    Some(sqlite_record_rejection_draft(
        source,
        dialect,
        path,
        source_rowid,
        rejection_class(kind),
        detail,
    ))
}

pub(super) fn core_projection_rejection_draft(
    source: &SourceKey,
    dialect: &OpenCodeSqliteDialect,
    path: &Path,
    source_rowid: i64,
    error: &CoreRecordError,
) -> SourceBackedRecordRejectionDraft {
    sqlite_record_rejection_draft(
        source,
        dialect,
        path,
        source_rowid,
        SourceBackedRecordRejectionClass::UnsupportedRecord,
        format!(
            "{} SQLite row exceeds Core projection limits: {error}",
            dialect.display_name
        ),
    )
}

fn sqlite_record_rejection_draft(
    source: &SourceKey,
    dialect: &OpenCodeSqliteDialect,
    path: &Path,
    source_rowid: i64,
    class: SourceBackedRecordRejectionClass,
    detail: String,
) -> SourceBackedRecordRejectionDraft {
    SourceBackedRecordRejectionDraft {
        source: source.clone(),
        provider: dialect.provider,
        source_selector: path.to_string_lossy().into_owned(),
        line_number: u64::try_from(source_rowid).unwrap_or(0),
        payload_type: Some("sqlite_row".to_owned()),
        class,
        detail,
    }
}

const fn rejection_class(kind: OpenCodeNativeRejectionKind) -> SourceBackedRecordRejectionClass {
    match kind {
        OpenCodeNativeRejectionKind::MalformedJson
        | OpenCodeNativeRejectionKind::MalformedResultJson
        | OpenCodeNativeRejectionKind::MissingSession
        | OpenCodeNativeRejectionKind::MissingMessage
        | OpenCodeNativeRejectionKind::SessionRelationshipMismatch
        | OpenCodeNativeRejectionKind::InvalidTimestamp => {
            SourceBackedRecordRejectionClass::MalformedRecord
        }
        OpenCodeNativeRejectionKind::UnsupportedStorageClass
        | OpenCodeNativeRejectionKind::OversizedRetainedContent
        | OpenCodeNativeRejectionKind::UnknownRecordType => {
            SourceBackedRecordRejectionClass::UnsupportedRecord
        }
    }
}

const fn rejection_detail(kind: OpenCodeNativeRejectionKind) -> &'static str {
    match kind {
        OpenCodeNativeRejectionKind::MalformedJson => "contains malformed JSON",
        OpenCodeNativeRejectionKind::MalformedResultJson => "contains malformed result JSON",
        OpenCodeNativeRejectionKind::UnsupportedStorageClass => {
            "uses an unsupported SQLite storage class"
        }
        OpenCodeNativeRejectionKind::OversizedRetainedContent => {
            "exceeds the retained-content size limit"
        }
        OpenCodeNativeRejectionKind::MissingSession => "references a missing session",
        OpenCodeNativeRejectionKind::MissingMessage => "references a missing message",
        OpenCodeNativeRejectionKind::SessionRelationshipMismatch => {
            "has inconsistent session relationships"
        }
        OpenCodeNativeRejectionKind::UnknownRecordType => "has an unsupported record type",
        OpenCodeNativeRejectionKind::InvalidTimestamp => "has an invalid timestamp",
    }
}
