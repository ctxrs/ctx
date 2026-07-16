use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, EntityTimestamps, Event, EventRole, EventType, Fidelity, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncMetadata, SyncState, Visibility,
};
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use std::{
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use super::*;
use crate::{CatalogSession, SourceImportFile};

const FORMAT: &str = "claude_projects_jsonl_tree";
const MATERIAL_FORMAT: &str = "claude_projects_jsonl";
const ROOT: &str = "/history/claude/projects";
const PATH_A: &str = "/history/claude/projects/a.jsonl";
const PATH_B: &str = "/history/claude/projects/b.jsonl";
const WRONG_CATALOG_FORMAT: &str = "claude_other_catalog";
const WRONG_SOURCE_IMPORT_FORMAT: &str = "claude_other_source_import";

#[derive(Debug, Clone, Copy)]
enum RetirementInventoryFamily {
    Catalog,
    SourceImport,
}

impl RetirementInventoryFamily {
    fn inventory_source_format(self) -> &'static str {
        match self {
            Self::Catalog => MATERIAL_FORMAT,
            Self::SourceImport => FORMAT,
        }
    }

    fn wrong_source_format(self) -> &'static str {
        match self {
            Self::Catalog => WRONG_CATALOG_FORMAT,
            Self::SourceImport => WRONG_SOURCE_IMPORT_FORMAT,
        }
    }

    fn inventory_table(self) -> &'static str {
        match self {
            Self::Catalog => "catalog_sessions",
            Self::SourceImport => "source_import_files",
        }
    }

    fn opposite_inventory_table(self) -> &'static str {
        match self {
            Self::Catalog => "source_import_files",
            Self::SourceImport => "catalog_sessions",
        }
    }
}

fn source_file(size: u64, modified_at_ms: i64) -> SourceImportFile {
    SourceImportFile {
        provider: CaptureProvider::Claude,
        source_format: FORMAT.to_owned(),
        source_root: ROOT.to_owned(),
        source_path: PATH_A.to_owned(),
        file_size_bytes: size,
        file_modified_at_ms: modified_at_ms,
        import_revision: 7,
        observed_at_ms: modified_at_ms,
        metadata: json!({"inventory_unit": "logical_import_unit"}),
    }
}

fn catalog_file(size: u64, modified_at_ms: i64) -> CatalogSession {
    CatalogSession {
        provider: CaptureProvider::Claude,
        source_format: MATERIAL_FORMAT.to_owned(),
        source_root: ROOT.to_owned(),
        source_path: PATH_A.to_owned(),
        external_session_id: Some("cross-family".to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        external_agent_id: None,
        cwd: None,
        session_started_at_ms: Some(1),
        file_size_bytes: size,
        file_modified_at_ms: modified_at_ms,
        import_revision: 7,
        cataloged_at_ms: modified_at_ms + 1,
        metadata: json!({"file_observation_token_v1": "test-catalog-token"}),
    }
}

fn checkpoint(size: u64, lines: u64, identity: &str, updated_at_ms: i64) -> ProviderFileCheckpoint {
    ProviderFileCheckpoint {
        provider: CaptureProvider::Claude,
        source_format: FORMAT.to_owned(),
        source_root: ROOT.to_owned(),
        source_path: PATH_A.to_owned(),
        import_revision: 7,
        checkpoint_version: 1,
        stable_file_identity: identity.to_owned(),
        committed_byte_offset: size,
        committed_complete_line_count: lines,
        head_sha256: "a".repeat(64),
        boundary_sha256: if size == 10 {
            "b".repeat(64)
        } else {
            "c".repeat(64)
        },
        resume_state: None,
        updated_at_ms,
    }
}

fn source_outcome<'a>(
    file: &'a SourceImportFile,
    inventory_generation: u64,
    indexed_at_ms: i64,
) -> ProviderFileImportOutcome<'a> {
    ProviderFileImportOutcome {
        provider: file.provider,
        observation: ProviderFileInventoryObservation::SourceImport {
            source_format: &file.source_format,
            update: SourceImportFileIndexUpdate {
                source_root: &file.source_root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation,
                metadata: &file.metadata,
                indexed_at_ms,
            },
        },
        status: CatalogIndexedStatus::Indexed,
        error: None,
    }
}

fn catalog_observation(
    catalog: &CatalogSession,
    inventory_generation: u64,
    indexed_at_ms: i64,
) -> ProviderFileInventoryObservation<'_> {
    ProviderFileInventoryObservation::ObservedCatalog {
        source_format: &catalog.source_format,
        update: CatalogSourceIndexUpdate {
            source_root: &catalog.source_root,
            source_path: &catalog.source_path,
            file_size_bytes: catalog.file_size_bytes,
            file_modified_at_ms: catalog.file_modified_at_ms,
            import_revision: catalog.import_revision,
            inventory_generation,
            file_sha256: None,
            event_count: None,
            indexed_at_ms,
        },
        metadata: &catalog.metadata,
    }
}

fn checkpoint_for_catalog(
    catalog: &CatalogSession,
    size: u64,
    lines: u64,
    updated_at_ms: i64,
) -> ProviderFileCheckpoint {
    ProviderFileCheckpoint {
        provider: catalog.provider,
        source_format: catalog.source_format.clone(),
        source_root: catalog.source_root.clone(),
        source_path: catalog.source_path.clone(),
        import_revision: catalog.import_revision,
        checkpoint_version: 1,
        stable_file_identity: "unix:2049:catalog-retain".to_owned(),
        committed_byte_offset: size,
        committed_complete_line_count: lines,
        head_sha256: "d".repeat(64),
        boundary_sha256: "e".repeat(64),
        resume_state: Some(b"opaque-resume-state".to_vec()),
        updated_at_ms,
    }
}

type CatalogLegacyCursor = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

fn catalog_legacy_cursor(store: &Store, source_path: &str) -> CatalogLegacyCursor {
    store
        .conn
        .query_row(
            "SELECT last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count FROM catalog_sessions WHERE source_path = ?1",
            params![source_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap()
}

include!("tests/checkpoints_and_append.rs");
include!("tests/fresh_new.rs");
include!("tests/lifecycle_and_adoption.rs");
include!("tests/retirement_observations.rs");
include!("tests/retirement_reconciliation.rs");
include!("tests/locks_and_security.rs");
include!("tests/visibility_fencing.rs");
include!("tests/archive_visibility.rs");
include!("tests/recovery_and_faults.rs");
include!("tests/batching_and_scaling.rs");
include!("tests/semantics_and_ownership.rs");
include!("tests/support_and_subprocess.rs");
