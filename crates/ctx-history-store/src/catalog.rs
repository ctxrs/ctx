use ctx_history_core::{
    canonical_provider_material_source_format, AgentType, CaptureProvider,
    PROVIDER_MATERIAL_SOURCE_FORMATS,
};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::{fmt::Write as _, str::FromStr};

use crate::connection::{
    capped_i64, collect_rows, nonnegative_i64_to_u32, nonnegative_i64_to_u64, parse_json,
    parse_text_enum, with_immediate_transaction,
};
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSession {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_path: String,
    pub external_session_id: Option<String>,
    pub parent_external_session_id: Option<String>,
    pub agent_type: AgentType,
    pub role_hint: Option<String>,
    pub external_agent_id: Option<String>,
    pub cwd: Option<String>,
    pub session_started_at_ms: Option<i64>,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub import_revision: u32,
    pub cataloged_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSourceIndexUpdate<'a> {
    pub source_root: &'a str,
    pub source_path: &'a str,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub import_revision: u32,
    pub inventory_generation: u64,
    pub file_sha256: Option<&'a str>,
    pub event_count: Option<u64>,
    pub indexed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSourceIndexState {
    pub last_imported_file_size_bytes: Option<u64>,
    pub last_imported_file_modified_at_ms: Option<i64>,
    pub last_imported_event_count: Option<u64>,
    pub last_imported_at_ms: Option<i64>,
    pub last_imported_file_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceImportFile {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_path: String,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub import_revision: u32,
    pub observed_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceImportFileIndexUpdate<'a> {
    pub source_root: &'a str,
    pub source_path: &'a str,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub import_revision: u32,
    pub inventory_generation: u64,
    pub metadata: &'a Value,
    pub indexed_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPendingReason {
    FreshNew,
    FreshChanged,
    FreshAppend,
    RecoveryRetry,
    RecoveryReplacement,
    ParserRevision,
    MissingMaterial,
    AbandonedPublication,
    Legacy,
    ExplicitRescan,
}

impl ImportPendingReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FreshNew => "fresh_new",
            Self::FreshChanged => "fresh_changed",
            Self::FreshAppend => "fresh_append",
            Self::RecoveryRetry => "recovery_retry",
            Self::RecoveryReplacement => "recovery_replacement",
            Self::ParserRevision => "parser_revision",
            Self::MissingMaterial => "missing_material",
            Self::AbandonedPublication => "abandoned_publication",
            Self::Legacy => "legacy",
            Self::ExplicitRescan => "explicit_rescan",
        }
    }

    pub fn class(self) -> ImportWorkClass {
        match self {
            Self::FreshNew | Self::FreshChanged | Self::FreshAppend => ImportWorkClass::Fresh,
            Self::RecoveryRetry
            | Self::RecoveryReplacement
            | Self::ParserRevision
            | Self::MissingMaterial
            | Self::AbandonedPublication
            | Self::Legacy
            | Self::ExplicitRescan => ImportWorkClass::Recovery,
        }
    }

    pub fn requires_replacement(self) -> bool {
        !matches!(self, Self::FreshAppend | Self::RecoveryRetry)
    }

    fn retry_after_failure(prior: Option<Self>) -> Self {
        match prior {
            Some(Self::FreshAppend | Self::RecoveryRetry) => Self::RecoveryRetry,
            _ => Self::RecoveryReplacement,
        }
    }
}

impl FromStr for ImportPendingReason {
    type Err = StoreError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "fresh_new" => Ok(Self::FreshNew),
            "fresh_changed" => Ok(Self::FreshChanged),
            "fresh_append" => Ok(Self::FreshAppend),
            "recovery_retry" => Ok(Self::RecoveryRetry),
            "recovery_replacement" => Ok(Self::RecoveryReplacement),
            "parser_revision" => Ok(Self::ParserRevision),
            "missing_material" => Ok(Self::MissingMaterial),
            "abandoned_publication" => Ok(Self::AbandonedPublication),
            "legacy" => Ok(Self::Legacy),
            "explicit_rescan" => Ok(Self::ExplicitRescan),
            other => Err(StoreError::InvalidImportPendingReason(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportWorkClass {
    Fresh,
    Recovery,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogImportWork {
    pub session: CatalogSession,
    pub reason: ImportPendingReason,
    pub estimated_bytes: u64,
    pub last_attempt_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceImportFileWork {
    pub file: SourceImportFile,
    pub reason: ImportPendingReason,
    pub estimated_bytes: u64,
    pub last_attempt_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportPendingReasonRepairProgress {
    pub processed_rows: usize,
    pub classified_rows: usize,
    pub completed_families: usize,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy)]
enum ImportPendingReasonRepairFamily {
    CatalogSessions,
    SourceImportFiles,
}

impl ImportPendingReasonRepairFamily {
    const ALL: [Self; 2] = [Self::CatalogSessions, Self::SourceImportFiles];

    fn as_str(self) -> &'static str {
        match self {
            Self::CatalogSessions => "catalog_sessions",
            Self::SourceImportFiles => "source_import_files",
        }
    }
}

#[derive(Debug)]
struct ImportPendingReasonRepairRow {
    provider: String,
    source_root: String,
    source_path: String,
    requires_work: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatalogCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
    pub completed_with_rejections: usize,
    pub rejected: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceImportFileCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
    pub completed_with_rejections: usize,
    pub rejected: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexedHistoryCounts {
    pub sessions: usize,
    pub events: usize,
}

impl IndexedHistoryCounts {
    pub fn items(self) -> usize {
        self.sessions.saturating_add(self.events)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogIndexedStatus {
    Pending,
    Indexed,
    CompletedWithRejections,
    Rejected,
    Failed,
}

impl CatalogIndexedStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Indexed => "indexed",
            Self::CompletedWithRejections => "completed_with_rejections",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    fn preserves_native_resume_checkpoint(self) -> bool {
        self == Self::Indexed
    }
}

#[derive(Debug)]
struct CatalogPendingState {
    provider: CaptureProvider,
    source_format: String,
    source_root: String,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
    import_revision: u32,
    is_stale: bool,
    indexed_file_size_bytes: Option<u64>,
    indexed_file_modified_at_ms: Option<i64>,
    indexed_status: CatalogIndexedStatus,
    indexed_import_revision: Option<u32>,
    pending_reason: Option<ImportPendingReason>,
}

#[derive(Debug)]
struct SourceImportPendingState {
    source_format: String,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
    import_revision: u32,
    is_stale: bool,
    indexed_file_size_bytes: Option<u64>,
    indexed_file_modified_at_ms: Option<i64>,
    indexed_status: CatalogIndexedStatus,
    indexed_import_revision: Option<u32>,
    pending_reason: Option<ImportPendingReason>,
    metadata_json: String,
}

impl FromStr for CatalogIndexedStatus {
    type Err = StoreError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "indexed" => Ok(Self::Indexed),
            "completed_with_rejections" => Ok(Self::CompletedWithRejections),
            "rejected" => Ok(Self::Rejected),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::InvalidImportPendingReason(format!(
                "invalid catalog indexed status {other}"
            ))),
        }
    }
}

include!("catalog/inventory.rs");
include!("catalog/pending_work.rs");
include!("catalog/source_pending_work.rs");
include!("catalog/source_imports.rs");
include!("catalog/counts.rs");

fn catalog_session_select_sql(tail: &str) -> String {
    format!(
        "SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, import_revision, cataloged_at_ms, metadata_json FROM catalog_sessions {tail}"
    )
}

fn source_import_file_select_sql(tail: &str) -> String {
    format!(
        "SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, import_revision, observed_at_ms, metadata_json FROM source_import_files {tail}"
    )
}

fn source_import_file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceImportFile> {
    Ok(SourceImportFile {
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(0)?)?,
        source_format: row.get(1)?,
        source_root: row.get(2)?,
        source_path: row.get(3)?,
        file_size_bytes: nonnegative_i64_to_u64(row.get(4)?)?,
        file_modified_at_ms: row.get(5)?,
        import_revision: nonnegative_i64_to_u32(row.get(6)?)?,
        observed_at_ms: row.get(7)?,
        metadata: parse_json(row.get::<_, String>(8)?)?,
    })
}

fn catalog_session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogSession> {
    Ok(CatalogSession {
        source_path: row.get(0)?,
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(1)?)?,
        source_format: row.get(2)?,
        source_root: row.get(3)?,
        external_session_id: row.get(4)?,
        parent_external_session_id: row.get(5)?,
        agent_type: parse_text_enum::<AgentType>(row.get::<_, String>(6)?)?,
        role_hint: row.get(7)?,
        external_agent_id: row.get(8)?,
        cwd: row.get(9)?,
        session_started_at_ms: row.get(10)?,
        file_size_bytes: nonnegative_i64_to_u64(row.get(11)?)?,
        file_modified_at_ms: row.get(12)?,
        import_revision: nonnegative_i64_to_u32(row.get(13)?)?,
        cataloged_at_ms: row.get(14)?,
        metadata: parse_json(row.get::<_, String>(15)?)?,
    })
}

fn catalog_pending_import_condition_sql(alias: &str) -> String {
    let material_exists = catalog_material_exists_sql(alias);
    format!(
        r#"
        (
            {alias}.pending_reason IS NOT NULL
            OR {alias}.indexed_status IN ('pending', 'failed')
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
            OR {alias}.indexed_import_revision IS NULL
            OR {alias}.indexed_import_revision != {alias}.import_revision
            OR (
              {alias}.indexed_status IN ('indexed', 'completed_with_rejections')
              AND NOT ({material_exists})
            )
        )
        "#
    )
}

fn source_import_file_pending_condition_sql(alias: &str) -> String {
    let material_exists = source_import_material_exists_sql(alias);
    format!(
        r#"
        (
            {alias}.pending_reason IS NOT NULL
            OR {alias}.indexed_status IN ('pending', 'failed')
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
            OR {alias}.indexed_import_revision IS NULL
            OR {alias}.indexed_import_revision != {alias}.import_revision
            OR (
              {alias}.indexed_status IN ('indexed', 'completed_with_rejections')
              AND NOT ({material_exists})
            )
        )
        "#
    )
}

fn import_work_class_predicate(alias: &str, class: ImportWorkClass) -> String {
    let reasons = match class {
        ImportWorkClass::Fresh => "'fresh_new', 'fresh_changed', 'fresh_append'",
        ImportWorkClass::Recovery => {
            "'recovery_retry', 'recovery_replacement', 'parser_revision', 'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'"
        }
    };
    format!("{alias}.pending_reason IN ({reasons})")
}

fn import_work_order(alias: &str, _class: ImportWorkClass) -> String {
    format!("{alias}.indexed_at_ms, {alias}.source_path")
}

fn expected_material_source_format(
    provider: CaptureProvider,
    inventory_source_format: &str,
) -> &str {
    canonical_provider_material_source_format(provider, inventory_source_format)
        .unwrap_or(inventory_source_format)
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn material_source_format_sql_case(alias: &str) -> String {
    let mut expression = String::from("CASE");
    for mapping in PROVIDER_MATERIAL_SOURCE_FORMATS {
        write!(
            expression,
            " WHEN {alias}.provider = {} AND {alias}.source_format = {} THEN {}",
            sql_string_literal(mapping.provider.as_str()),
            sql_string_literal(mapping.inventory_source_format),
            sql_string_literal(mapping.material_source_format),
        )
        .expect("writing to a String cannot fail");
    }
    write!(expression, " ELSE {alias}.source_format END").expect("writing to a String cannot fail");
    expression
}

fn catalog_material_exists_sql(alias: &str) -> String {
    let material_source_format = material_source_format_sql_case(alias);
    let owner = crate::provider_files::material_owner_predicate(
        "source",
        &format!("{alias}.provider"),
        &material_source_format,
        &format!("{alias}.source_root"),
        &format!("{alias}.source_path"),
    );
    format!(
        r#"
        EXISTS (
          SELECT 1
          FROM sessions AS material_session
          JOIN capture_sources AS source
            ON source.id = material_session.capture_source_id
          WHERE material_session.provider = {alias}.provider
            AND {alias}.external_session_id IS NOT NULL
            AND material_session.external_session_id = {alias}.external_session_id
            AND ({owner})
            AND source.external_session_id = {alias}.external_session_id
          LIMIT 1
        )
        "#
    )
}

fn source_import_material_exists_sql(alias: &str) -> String {
    let material_source_format = material_source_format_sql_case(alias);
    format!(
        r#"
        EXISTS (
          SELECT 1
            FROM capture_sources AS source
          WHERE source.provider = {alias}.provider
            AND source.source_format = {material_source_format}
            AND (
              (
                json_extract({alias}.metadata_json, '$.inventory_unit') = 'source_root'
                AND source.source_root = {alias}.source_root
              )
              OR (
                json_extract({alias}.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                AND source.raw_source_path = {alias}.source_path
                AND (
                  source.source_root = {alias}.source_root
                  OR source.source_root = source.raw_source_path
                  OR source.source_root IS NULL
                )
              )
            )
          LIMIT 1
        )
        "#
    )
}

fn catalog_indexed_count_sql() -> String {
    let visible = crate::provider_files::catalog_material_visible_predicate("catalog");
    let material_exists = catalog_material_exists_sql("catalog");
    format!(
        r#"
    SELECT COUNT(*)
    FROM catalog_sessions AS catalog
    WHERE catalog.is_stale = 0
      AND {visible}
      AND catalog.indexed_status IN ('indexed', 'completed_with_rejections')
      AND catalog.indexed_file_size_bytes = catalog.file_size_bytes
      AND catalog.indexed_file_modified_at_ms = catalog.file_modified_at_ms
      AND catalog.indexed_import_revision = catalog.import_revision
      AND {material_exists}
    "#
    )
}

#[cfg(test)]
mod tests;
