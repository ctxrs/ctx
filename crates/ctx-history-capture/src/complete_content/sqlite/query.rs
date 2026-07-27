//! Request validation, query budgets, connection bounds, and schema allowlists.

use super::*;

pub(super) fn validate_request_batch(
    requests: &[CompleteMessageRequest],
) -> Result<(), CompleteContentError> {
    let first = &requests[0];
    if requests.len() > MAX_SQLITE_COMPLETE_REQUESTS {
        return Err(error(first, CompleteContentErrorKind::ContentTooLarge));
    }
    let mut previous = None;
    for request in requests {
        if request.provider != first.provider
            || request.source_format != first.source_format
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Sqlite
            || request.source_family != Some(CompleteContentSourceFamily::Sqlite)
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let coordinate = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if previous.is_some_and(|previous| previous >= coordinate) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        if request.source_locator.is_none()
            || request.expected_native_record_id.is_none()
            || request.expected_record_digest.is_none()
            || request.expected_content_ref.is_none()
        {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        previous = Some(coordinate);
    }
    Ok(())
}

pub(super) fn configure_connection(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let value_limit = i32::try_from(COMPLETE_CONTENT_MAX_BODY_BYTES)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, value_limit);
    conn.set_limit(
        SqliteLimit::SQLITE_LIMIT_COLUMN,
        SQLITE_MAX_ROW_VALUES as i32,
    );
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|cause| map_sqlite_error(request, cause))?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|cause| map_sqlite_error(request, cause))?;
    let schema_objects: i64 = conn
        .query_row("select count(*) from sqlite_schema", [], |row| row.get(0))
        .map_err(|cause| map_sqlite_error(request, cause))?;
    if schema_objects < 0 || schema_objects as usize > SQLITE_MAX_SCHEMA_OBJECTS {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|cause| map_sqlite_error(request, cause))?;
    if user_version < 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum CompleteContentSqliteBoundError {
    Capture(CaptureError),
    ContentTooLarge,
}

impl From<CaptureError> for CompleteContentSqliteBoundError {
    fn from(error: CaptureError) -> Self {
        Self::Capture(error)
    }
}

impl From<rusqlite::Error> for CompleteContentSqliteBoundError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Capture(CaptureError::Sqlite(error))
    }
}

impl From<std::io::Error> for CompleteContentSqliteBoundError {
    fn from(error: std::io::Error) -> Self {
        Self::Capture(CaptureError::Io(error))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompleteContentSqliteQueryBudget {
    deadline: Instant,
    progress_instructions: i32,
    force_interrupt: bool,
}

impl CompleteContentSqliteQueryBudget {
    pub(crate) fn new() -> Self {
        Self {
            deadline: Instant::now() + SQLITE_RESOLVE_TIMEOUT,
            progress_instructions: SQLITE_PROGRESS_INSTRUCTIONS,
            force_interrupt: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn interrupted_for_test() -> Self {
        Self {
            deadline: Instant::now() + SQLITE_RESOLVE_TIMEOUT,
            progress_instructions: 1,
            force_interrupt: true,
        }
    }

    fn remaining(self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

pub(crate) fn configure_complete_content_sqlite_connection(
    conn: &Connection,
    budget: CompleteContentSqliteQueryBudget,
) -> Result<(), CompleteContentSqliteBoundError> {
    let remaining = budget.remaining();
    if remaining.is_zero() {
        return Err(CompleteContentSqliteBoundError::ContentTooLarge);
    }
    let value_limit = i32::try_from(COMPLETE_CONTENT_MAX_BODY_BYTES)
        .map_err(|_| CompleteContentSqliteBoundError::ContentTooLarge)?;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, value_limit);
    conn.set_limit(
        SqliteLimit::SQLITE_LIMIT_COLUMN,
        SQLITE_MAX_ROW_VALUES as i32,
    );
    conn.busy_timeout(remaining.min(Duration::from_millis(250)))?;
    conn.pragma_update(None, "query_only", true)?;
    conn.progress_handler(
        budget.progress_instructions,
        Some(move || budget.force_interrupt || Instant::now() >= budget.deadline),
    );
    let schema_objects: i64 =
        conn.query_row("select count(*) from sqlite_schema", [], |row| row.get(0))?;
    if schema_objects < 0 || schema_objects as usize > SQLITE_MAX_SCHEMA_OBJECTS {
        return Err(CompleteContentSqliteBoundError::ContentTooLarge);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if user_version < 0 {
        return Err(CaptureError::InvalidPayload(
            "complete-content SQLite user_version must be nonnegative".to_owned(),
        )
        .into());
    }
    Ok(())
}

pub(super) fn validate_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let required = match request.provider {
        CaptureProvider::Firebender => (
            "chat_sessions",
            &[
                "id",
                "name",
                "created_at",
                "updated_at",
                "messages_json",
                "metadata_json",
            ][..],
        ),
        CaptureProvider::AstrBot => ("conversations", &["content"][..]),
        CaptureProvider::KiroCli => return validate_kiro_schema(conn, request),
        CaptureProvider::Lingma => (
            "chat_record",
            &[
                "session_id",
                "request_id",
                "chat_prompt",
                "summary",
                "error_result",
                "gmt_create",
                "extra",
            ][..],
        ),
        CaptureProvider::Trae => ("ItemTable", &["key", "value"][..]),
        CaptureProvider::Zed => (
            "threads",
            &["id", "summary", "updated_at", "data_type", "data"][..],
        ),
        CaptureProvider::ForgeCode => (
            "conversations",
            &["conversation_id", "workspace_id", "created_at"][..],
        ),
        CaptureProvider::Crush => return validate_crush_schema(conn, request),
        CaptureProvider::Goose => {
            return goose::load_goose_message_values_schema(conn)
                .map_err(|cause| map_capture_error(request, cause))
        }
        CaptureProvider::Hermes => {
            return hermes::load_hermes_message_values_schema(conn)
                .map_err(|cause| map_capture_error(request, cause))
        }
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            return opencode::load_opencode_message_values_schema(
                conn,
                opencode_dialect(request.provider),
            )
            .map_err(|cause| map_capture_error(request, cause));
        }
        CaptureProvider::DeepAgents => return deepagents::validate_schema(conn, request),
        CaptureProvider::Warp => (
            "agent_tasks",
            &["conversation_id", "task_id", "task", "last_modified_at"][..],
        ),
        CaptureProvider::Shelley => return validate_shelley_schema(conn, request),
        _ => {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
    };
    if !sqlite_table_exists(conn, required.0).map_err(|cause| map_capture_error(request, cause))? {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let columns = sqlite_table_columns(conn, required.0)
        .map_err(|cause| map_capture_error(request, cause))?;
    ensure_sqlite_table_columns(&columns, required.0, required.1)
        .map_err(|cause| map_capture_error(request, cause))
}

pub(super) fn validate_kiro_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let has_v2 = sqlite_table_exists(conn, "conversations_v2")
        .map_err(|cause| map_capture_error(request, cause))?;
    let has_legacy = sqlite_table_exists(conn, "conversations")
        .map_err(|cause| map_capture_error(request, cause))?;
    if !has_v2 && !has_legacy {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    if has_v2 {
        let columns = sqlite_table_columns(conn, "conversations_v2")
            .map_err(|cause| map_capture_error(request, cause))?;
        ensure_sqlite_table_columns(
            &columns,
            "conversations_v2",
            &[
                "key",
                "conversation_id",
                "value",
                "created_at",
                "updated_at",
            ],
        )
        .map_err(|cause| map_capture_error(request, cause))?;
    }
    if has_legacy {
        let columns = sqlite_table_columns(conn, "conversations")
            .map_err(|cause| map_capture_error(request, cause))?;
        ensure_sqlite_table_columns(&columns, "conversations", &["key", "value"])
            .map_err(|cause| map_capture_error(request, cause))?;
    }
    Ok(())
}

pub(super) fn validate_shelley_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    shelley::shelley_message_columns(conn)
        .and_then(|_| shelley::shelley_conversation_columns(conn))
        .map(|_| ())
        .map_err(|cause| map_capture_error(request, cause))
}
