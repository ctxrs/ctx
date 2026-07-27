//! Bounded complete-message recovery for provider SQLite sources.
//!
//! The resolver never opens a provider database read-write. Databases without
//! sidecars are opened through SQLite's immutable URI mode. Databases with a
//! WAL, SHM, or rollback journal are copied to a private temporary snapshot by
//! the shared provider SQLite opener before SQLite sees them. Every supported
//! request addresses one allowlisted provider row by its captured native key;
//! capture ordinals are never used as SQL offsets.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType};
use rusqlite::{limits::Limit as SqliteLimit, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    compute_payload_hash,
    native_source::{NativeLocator, NativeSqliteValue},
    provider::{
        providers::{
            crush, firebender, forgecode, goose, hermes, kiro, nanoclaw, opencode, shelley, warp,
            zed,
        },
        sqlite::{ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists},
    },
    CaptureError,
};

use super::{
    attach_verified_content_locator, verified_content_profile, verified_content_route_supported,
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteMessage, CompleteMessageRequest, ResolvedResultContent, ResultContentRequest,
    ResultContentResolver, SourceVerification, VerifiedContentLocatorV1, VerifiedContentRole,
    COMPLETE_CONTENT_MAX_BODY_BYTES,
};
#[cfg(test)]
use crate::{
    FIREBENDER_SQLITE_SOURCE_FORMAT, KIRO_SQLITE_SOURCE_FORMAT, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

const FIREBENDER_LOCATOR_KIND: &str = "firebender-chat-session-row-v1";
const KIRO_LOCATOR_KIND: &str = "kiro-conversation-row-v1";
const ZED_LOCATOR_KIND: &str = "zed-thread-row-v1";
const HERMES_LOCATOR_KIND: &str = "hermes-sqlite-row-v1";
const FORGECODE_LOCATOR_KIND: &str = "forgecode-conversation-row-v1";
const OPENCODE_LOCATOR_KIND: &str = "opencode-sqlite-logical-row-v1";
const CRUSH_LOCATOR_KIND: &str = "crush-sqlite-row-v1";
const GOOSE_LOCATOR_KIND: &str = "goose-logical-row-v3";
const WARP_LOCATOR_KIND: &str = "warp-task-message-v1";
const SHELLEY_LOCATOR_KIND: &str = "shelley-compound-message-row-v1";

const MAX_SQLITE_COMPLETE_REQUESTS: usize = 256;
const SQLITE_PROGRESS_INSTRUCTIONS: i32 = 1_000;
const SQLITE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const SQLITE_MAX_SCHEMA_OBJECTS: usize = 1_024;
const SQLITE_MAX_ROW_VALUES: usize = 64;

mod no_tool_messages;
#[cfg(test)]
use no_tool_messages::LINGMA_LOCATOR_KIND;
mod deepagents;
mod warp_result;

mod errors;
mod locators;
mod messages;
mod query;
mod results;

use errors::*;
use locators::*;
use messages::*;
use query::*;
use results::*;

pub(crate) use errors::map_bounded_sqlite_error_for_event;
pub(crate) use locators::attach_sqlite_complete_content_locator;
pub(crate) use query::{
    configure_complete_content_sqlite_connection, CompleteContentSqliteBoundError,
    CompleteContentSqliteQueryBudget,
};
pub(crate) use results::SqliteResultRecord;

#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteCompleteContentResolver;

impl SqliteCompleteContentResolver {
    pub fn new() -> Self {
        Self
    }
}

impl CompleteContentResolver for SqliteCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Sqlite
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        verified_content_route_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::MessageBody,
        )
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        validate_request_batch(requests)?;
        if !CompleteContentResolver::supports(self, first.provider, &first.source_format) {
            return Err(error(first, CompleteContentErrorKind::HydrationUnsupported));
        }
        for request in requests {
            match request.provider {
                CaptureProvider::Firebender => {
                    decode_raw_rowid(request, FIREBENDER_LOCATOR_KIND)?;
                }
                CaptureProvider::KiroCli => {
                    decode_kiro_rowid(request)?;
                }
                CaptureProvider::AstrBot | CaptureProvider::Lingma | CaptureProvider::Trae => {
                    no_tool_messages::validate_locator(request)?;
                }
                CaptureProvider::Zed => {
                    decode_raw_rowid(request, ZED_LOCATOR_KIND)?;
                }
                CaptureProvider::ForgeCode => {
                    decode_raw_rowid(request, FORGECODE_LOCATOR_KIND)?;
                }
                CaptureProvider::Crush => {
                    decode_phased_ordered_rowid(request, CRUSH_LOCATOR_KIND)?;
                }
                CaptureProvider::Goose => {
                    decode_phased_ordered_rowid(request, GOOSE_LOCATOR_KIND)?;
                }
                CaptureProvider::Hermes => {
                    decode_phased_raw_rowid(request, HERMES_LOCATOR_KIND)?;
                }
                CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
                    decode_opencode_locator(request)?;
                }
                CaptureProvider::DeepAgents => {
                    deepagents::validate_message_request(request)?;
                }
                CaptureProvider::Warp => {
                    decode_warp_message_coordinate(request)?;
                }
                CaptureProvider::Shelley => {
                    decode_shelley_locator(request)?;
                }
                CaptureProvider::NanoClaw => {
                    decode_nanoclaw_locator(request)?;
                }
                _ => {
                    return Err(error(
                        request,
                        CompleteContentErrorKind::HydrationUnsupported,
                    ));
                }
            }
        }
        if first.provider == CaptureProvider::NanoClaw {
            return resolve_nanoclaw_project(requests);
        }
        let conn = first.source_access.open_sqlite_snapshot(first.event_id)?;
        configure_connection(&conn, first)?;
        validate_schema(&conn, first)?;

        let deadline = Instant::now() + SQLITE_RESOLVE_TIMEOUT;
        conn.progress_handler(
            SQLITE_PROGRESS_INSTRUCTIONS,
            Some(move || Instant::now() >= deadline),
        );
        let resolved = requests
            .iter()
            .map(|request| resolve_one(&conn, request))
            .collect::<Result<Vec<_>, _>>();
        conn.progress_handler(0, None::<fn() -> bool>);
        resolved
    }
}

impl ResultContentResolver for SqliteCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Sqlite
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        sqlite_result_profile(provider, source_format).is_some()
    }

    fn resolve_results(
        &self,
        requests: &[ResultContentRequest],
    ) -> Vec<Result<ResolvedResultContent, CompleteContentError>> {
        self.resolve_result_group(requests).unwrap_or_else(|error| {
            requests
                .iter()
                .map(|request| Err(CompleteContentError::new(error.kind, request.event_id)))
                .collect()
        })
    }
}

impl SqliteCompleteContentResolver {
    fn resolve_result_group(
        &self,
        requests: &[ResultContentRequest],
    ) -> Result<Vec<Result<ResolvedResultContent, CompleteContentError>>, CompleteContentError>
    {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if requests.len() > MAX_SQLITE_COMPLETE_REQUESTS {
            return Err(result_error(
                first,
                CompleteContentErrorKind::ContentTooLarge,
            ));
        }
        let mut previous = None;
        for request in requests {
            let coordinate = (
                request.source_record_ordinal,
                request.source_record_subrecord_index,
            );
            if request.provider != first.provider
                || request.source_format != first.source_format
                || request.source_access != first.source_access
                || request.source_access.family() != CompleteContentSourceFamily::Sqlite
                || request.source_family != CompleteContentSourceFamily::Sqlite
                || !super::verified_content_route_matches(
                    &request.content_profile,
                    request.provider,
                    &request.source_format,
                    request.source_family,
                    VerifiedContentRole::ResultBody,
                    request.source_locator.kind(),
                )
                || previous.is_some_and(|previous| previous >= coordinate)
                || decode_result_locator(request).is_err()
            {
                return Err(result_error(
                    request,
                    CompleteContentErrorKind::ContentVerificationFailed,
                ));
            }
            previous = Some(coordinate);
        }
        if sqlite_result_profile(first.provider, &first.source_format).is_none() {
            return Err(result_error(
                first,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }

        let shim = result_request_shim(first);
        let conn = first.source_access.open_sqlite_snapshot(first.event_id)?;
        configure_connection(&conn, &shim)?;
        let deadline = Instant::now() + SQLITE_RESOLVE_TIMEOUT;
        conn.progress_handler(
            SQLITE_PROGRESS_INSTRUCTIONS,
            Some(move || Instant::now() >= deadline),
        );
        let resolved = requests
            .iter()
            .map(|request| resolve_one_result(&conn, request))
            .collect::<Vec<_>>();
        conn.progress_handler(0, None::<fn() -> bool>);
        Ok(resolved)
    }
}

#[cfg(test)]
#[path = "sqlite/tests/mod.rs"]
mod tests;
