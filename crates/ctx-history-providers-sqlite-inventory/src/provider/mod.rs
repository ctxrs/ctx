pub(crate) mod sqlite {
    use std::collections::BTreeSet;

    #[cfg(test)]
    use std::path::Path;

    use rusqlite::Connection;

    #[cfg(test)]
    use crate::CaptureError;
    use crate::Result;

    pub(crate) use ctx_history_source_sqlite::{
        optional_text_column_expr, optional_timestamp_millis_expr, SqliteLengthPreflightGuard,
    };

    pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
        ctx_history_source_sqlite::sqlite_table_exists(conn, table).map_err(Into::into)
    }

    pub(crate) fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
        ctx_history_source_sqlite::sqlite_table_columns(conn, table).map_err(Into::into)
    }

    pub(crate) fn ensure_sqlite_table_columns(
        columns: &BTreeSet<String>,
        label: &str,
        required: &[&str],
    ) -> Result<()> {
        ctx_history_source_sqlite::ensure_sqlite_table_columns(columns, label, required)
            .map_err(Into::into)
    }

    pub(crate) fn sqlite_schema_fingerprint(conn: &Connection) -> Result<String> {
        ctx_history_source_sqlite::sqlite_schema_fingerprint(conn).map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) type ReadOnlySqliteConnection =
        ctx_history_source_sqlite::MappedReadOnlySqliteConnection<CaptureError>;

    #[cfg(test)]
    pub(crate) fn open_provider_sqlite_readonly(
        data_root: &Path,
        path: &Path,
    ) -> Result<ReadOnlySqliteConnection> {
        ReadOnlySqliteConnection::open(data_root, path)
    }
}

pub(crate) mod source_backed {
    pub(crate) use crate::lifecycle::*;

    pub(crate) fn record_sqlite_rejection(
        rejections: &mut SourceBackedRecordRejectionDrafts,
        source: &ctx_history_core::SourceKey,
        provider: ctx_history_core::CaptureProvider,
        source_path: &std::path::Path,
        physical_record: u64,
        class: SourceBackedRecordRejectionClass,
        detail: impl Into<String>,
    ) {
        rejections.record(sqlite_rejection_draft(
            source,
            provider,
            source_path,
            physical_record,
            class,
            detail,
        ));
    }

    pub(crate) fn sqlite_rejection_draft(
        source: &ctx_history_core::SourceKey,
        provider: ctx_history_core::CaptureProvider,
        source_path: &std::path::Path,
        physical_record: u64,
        class: SourceBackedRecordRejectionClass,
        detail: impl Into<String>,
    ) -> SourceBackedRecordRejectionDraft {
        SourceBackedRecordRejectionDraft {
            source: source.clone(),
            provider,
            source_selector: source_path.to_string_lossy().into_owned(),
            line_number: physical_record,
            payload_type: Some("sqlite_row".to_owned()),
            class,
            detail: detail.into(),
        }
    }

    pub(crate) fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
        SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
    }

    pub(crate) fn combine_primary_and_cleanup_route_errors(
        primary: SourceBackedRouteError,
        cleanup: SourceBackedRouteError,
    ) -> SourceBackedRouteError {
        let kind = if route_error_severity(primary.kind) >= route_error_severity(cleanup.kind) {
            primary.kind
        } else {
            cleanup.kind
        };
        SourceBackedRouteError::new(
            kind,
            format!(
                "{}; explicit SQLite snapshot cleanup also failed: {}",
                primary.detail, cleanup.detail
            ),
        )
    }

    const fn route_error_severity(kind: SourceBackedRouteErrorKind) -> u8 {
        match kind {
            SourceBackedRouteErrorKind::Internal => 6,
            SourceBackedRouteErrorKind::ResourceUnavailable => 5,
            SourceBackedRouteErrorKind::SourceChanged => 4,
            SourceBackedRouteErrorKind::InvalidSource => 3,
            SourceBackedRouteErrorKind::Unsupported => 2,
            SourceBackedRouteErrorKind::Unavailable => 1,
        }
    }

    pub(crate) mod family {
        pub(crate) mod document {
            pub(crate) use crate::lifecycle::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
                DocumentLeafFingerprint, DocumentRecordSpool, DocumentSourceTerminal,
                ObservedDocumentLeaf, ReplacementDocumentTree,
            };
        }
    }
}

pub mod providers;
