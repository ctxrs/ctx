use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceRecordLocator, SourceResolverContractError, SubrecordSelector,
    TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::scanner::{
    absolute_trae_path, acquire_source, packed_native_index, TraeCoreRecord, TraeFrontier,
    TraeScanner, TraeSourceAuthority,
};
use crate::{
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceEvidence},
    CaptureError,
};

use super::super::{TRAE_CHAT_KEYS, TRAE_STATE_VSCDB_SOURCE_FORMAT};

mod hydration;
mod replacement;

#[cfg(test)]
pub(crate) use hydration::hydrate_trae_source_backed_locator_v0;
pub(crate) use hydration::TraeLocatorResolverV0;
pub(crate) use replacement::TraeReplacementTree;

const TRAE_SOURCE_ANCHOR_NAMESPACE: &str = "trae.workspace-storage";
const TRAE_SOURCE_SCHEMA_VARIANT: &str = "trae-itemtable-json-v1";
const TRAE_SOURCE_BACKED_PARSER_REVISION: &str = "trae-itemtable-source-backed-v1";
const TRAE_NATIVE_SESSION_NAMESPACE: &str = "trae.itemtable-session-v1";
const TRAE_SESSION_POSITION_KIND: &str = "trae.itemtable-session-position-v1";
const TRAE_NATIVE_ITEM_NAMESPACE: &str = "trae.itemtable-key-v1";
const TRAE_NATIVE_MESSAGE_NAMESPACE: &str = "trae.itemtable-message-v1";
const TRAE_MESSAGE_POSITION_KIND: &str = "trae.itemtable-message-position-v1";
const TRAE_LOGICAL_SESSION_KIND: &str = "trae-session";
const TRAE_LOGICAL_EVENT_KIND: &str = "trae-message";
const TRAE_LOCATOR_RELATION: &str = "ItemTable";
const TRAE_SOURCE_BACKED_PAGE_ROWS: usize = 64;

#[derive(Debug, Error)]
pub(crate) enum TraeSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error("Trae source-backed adapter requires an explicit state.vscdb leaf")]
    ExplicitLeafRequired,
    #[error("Trae source-backed scan counters overflowed or did not reconcile")]
    CountMismatch,
    #[error("locator is not a Trae ItemTable nested-message locator")]
    InvalidLocator,
    #[error("Trae locator is bound to a different explicit source")]
    LocatorSourceMismatch,
    #[error("Trae source revision no longer matches the exact locator")]
    SourceRevisionMismatch,
    #[error("Trae locator ItemTable value is unavailable")]
    LocatorValueMissing,
    #[error("Trae locator ItemTable value digest no longer matches")]
    LocatorValueDigestMismatch,
    #[error("Trae locator nested message is unavailable")]
    LocatorMessageMissing,
}

pub(crate) type TraeSourceBackedResultV0<T> = std::result::Result<T, TraeSourceBackedErrorV0>;

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedPageV0 {
    pub(crate) documents: Vec<LexicalDocument>,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedScanV0 {
    pub(crate) source: CertifiedSource,
    pub(crate) terminal_fence: TraeSourceTerminalFence,
    pub(crate) row_decode_passes: u64,
    pub(crate) decoded_rows: u64,
    pub(crate) emitted_pages: u64,
    pub(crate) peak_buffered_documents: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceTerminalFence {
    evidence: SqliteSourceEvidence,
}

/// Scans exactly one explicitly supplied `state.vscdb` leaf.
///
/// The callback receives bounded pages from the existing ItemTable scanner.
/// This leaf owns no automatic inventory, lifecycle, or publication behavior.
#[cfg(test)]
pub(crate) fn scan_trae_source_backed_explicit_v0(
    path: &Path,
    emit: &mut dyn FnMut(TraeSourceBackedPageV0) -> TraeSourceBackedResultV0<()>,
) -> TraeSourceBackedResultV0<TraeSourceBackedScanV0> {
    let canonical_path = explicit_trae_leaf(path)?;
    let authority = acquire_source(
        crate::test_provider_sqlite_data_root(),
        &canonical_path,
        DateTime::<Utc>::UNIX_EPOCH,
    )?;
    scan_trae_authority(&canonical_path, &authority, emit)
}

pub(super) fn scan_trae_authority(
    canonical_path: &Path,
    authority: &TraeSourceAuthority,
    emit: &mut dyn FnMut(TraeSourceBackedPageV0) -> TraeSourceBackedResultV0<()>,
) -> TraeSourceBackedResultV0<TraeSourceBackedScanV0> {
    let source = source_key(&authority)?;
    let mut scanner = TraeScanner::new(&authority, TraeFrontier::default());
    let mut counts = ScannedSourceCounts::default();
    let mut emitted_pages = 0_u64;
    let mut peak_buffered_documents = 0_u64;
    authority.database.read_provider(canonical_path, |conn| {
        while let Some(page) = scanner.next_page(conn)? {
            let complete_records = u64::try_from(page.logical_units)
                .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
            let rejected_records = u64::try_from(page.rejections.len())
                .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
            let mut documents = Vec::with_capacity(page.core.len());
            for record in page.core {
                if let Some(document) = lexical_document(&source, authority, record)? {
                    documents.push(document);
                }
            }
            let retained_records = u64::try_from(documents.len())
                .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
            let ignored_records = complete_records
                .checked_sub(
                    retained_records
                        .checked_add(rejected_records)
                        .ok_or(TraeSourceBackedErrorV0::CountMismatch)?,
                )
                .ok_or(TraeSourceBackedErrorV0::CountMismatch)?;

            counts.complete_records = checked_add(counts.complete_records, complete_records)?;
            counts.retained_records = checked_add(counts.retained_records, retained_records)?;
            counts.rejected_records = checked_add(counts.rejected_records, rejected_records)?;
            counts.ignored_records = checked_add(counts.ignored_records, ignored_records)?;
            counts.indexed_documents = checked_add(counts.indexed_documents, retained_records)?;
            peak_buffered_documents = peak_buffered_documents.max(retained_records);
            if !documents.is_empty() {
                if documents.len() > TRAE_SOURCE_BACKED_PAGE_ROWS {
                    return Err(TraeSourceBackedErrorV0::CountMismatch);
                }
                emitted_pages = checked_add(emitted_pages, 1)?;
                emit(TraeSourceBackedPageV0 { documents })?;
            }
        }
        Ok(())
    })?;

    authority.database.revalidate()?;
    counts.certified_bytes = scanner.certified_source_bytes();
    let decoded_rows = scanner.decoded_rows();
    let source = SqliteLogicalSnapshot::new(
        TRAE_SOURCE_BACKED_PARSER_REVISION,
        &authority.schema_evidence,
        scanner.source_content_digest(),
        counts,
    )
    .certify(source)?;
    Ok(TraeSourceBackedScanV0 {
        source,
        terminal_fence: TraeSourceTerminalFence {
            evidence: authority.database.evidence().clone(),
        },
        row_decode_passes: 1,
        decoded_rows,
        emitted_pages,
        peak_buffered_documents,
    })
}

fn lexical_document(
    source: &SourceKey,
    authority: &TraeSourceAuthority,
    record: TraeCoreRecord,
) -> TraeSourceBackedResultV0<Option<LexicalDocument>> {
    let body = record.lexical_text;
    if body.is_empty() {
        return Ok(None);
    }
    let revision_scope = TypedKey::bytes(record.value_digest.to_vec())?;
    let session_key = if record.native_session_id_from_provider {
        NativeSessionKey::composite(
            TRAE_NATIVE_SESSION_NAMESPACE,
            vec![
                TypedKey::utf8(record.chat_key)?,
                TypedKey::utf8(&record.native_session_id)?,
            ],
        )?
    } else {
        NativeSessionKey::revision_scoped_position(
            TRAE_SESSION_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::from(record.key_index)),
                TypedKey::U64(u64::from(record.raw_session_index)),
            ])?,
            revision_scope.clone(),
        )?
    };
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: TRAE_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    let item_key =
        NativeItemKey::native_id(TRAE_NATIVE_ITEM_NAMESPACE, TypedKey::utf8(record.chat_key)?)?;
    let subrecord = if record.native_message_id_from_provider {
        SubrecordSelector::composite(
            TRAE_NATIVE_MESSAGE_NAMESPACE,
            vec![
                TypedKey::utf8(&record.native_session_id)?,
                TypedKey::utf8(&record.native_message_id)?,
            ],
        )?
    } else {
        SubrecordSelector::revision_scoped_position(
            TRAE_MESSAGE_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::from(record.raw_session_index)),
                TypedKey::U64(u64::from(record.message_index)),
            ])?,
            revision_scope,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: TRAE_LOGICAL_EVENT_KIND,
        native_item_key: &item_key,
        subrecord_selector: Some(&subrecord),
    })?;
    let coordinate = NativeRecordCoordinate::ProviderSqlite {
        logical_relation: TRAE_LOCATOR_RELATION.to_owned(),
        primary_key: TypedKey::composite(vec![
            TypedKey::utf8(record.chat_key)?,
            TypedKey::U64(u64::from(record.raw_session_index)),
            TypedKey::U64(u64::from(record.message_index)),
            TypedKey::utf8(&record.provider_session_id)?,
        ])?,
        row_version: Some(TypedKey::bytes(record.value_digest.to_vec())?),
    };
    let locator = SourceRecordLocator::new(
        source.clone(),
        coordinate,
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record.value_digest,
    )?;
    let event_sequence = packed_native_index(
        record.key_index,
        record.raw_session_index,
        record.message_index,
    )?;
    Ok(Some(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(record.provider_session_id),
        branch: None,
        source_path: Some(authority.raw_source_path.clone()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence,
        occurred_at_unix_ms: Some(record.occurred_at.timestamp_millis()),
        event_type: record.event_type.as_str().to_owned(),
        role: record.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: authority
            .workspace_folder
            .clone()
            .or_else(|| Some(authority.workspace_id.clone())),
        cwd: authority.workspace_folder.clone(),
        touched_files: Vec::new(),
    }))
}

fn source_key(authority: &TraeSourceAuthority) -> TraeSourceBackedResultV0<SourceKey> {
    source_key_for_workspace(&authority.workspace_id)
}

fn source_key_for_workspace(workspace_id: &str) -> TraeSourceBackedResultV0<SourceKey> {
    let anchor =
        SourceAnchor::provider_native(TRAE_SOURCE_ANCHOR_NAMESPACE, TypedKey::utf8(workspace_id)?)?;
    Ok(SourceKey::derive(
        CaptureProvider::Trae.as_str(),
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        TRAE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(super) fn explicit_trae_leaf(path: &Path) -> TraeSourceBackedResultV0<PathBuf> {
    crate::common::io::ensure_provider_path_parents_are_not_symlinks(path)?;
    let metadata = fs::symlink_metadata(path).map_err(CaptureError::from)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || path.file_name().and_then(|name| name.to_str()) != Some("state.vscdb")
    {
        return Err(TraeSourceBackedErrorV0::ExplicitLeafRequired);
    }
    Ok(absolute_trae_path(path)?)
}

fn checked_add(left: u64, right: u64) -> TraeSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(TraeSourceBackedErrorV0::CountMismatch)
}

#[cfg(test)]
mod tests {
    use rusqlite::{params, Connection};
    use serde_json::{json, Value};

    use super::*;

    #[test]
    fn explicit_cold_scan_certifies_and_hydrates_nested_itemtable_message() {
        let temp = crate::test_support_paths::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace-explicit");
        fs::create_dir_all(&workspace).expect("workspace");
        let source = workspace.join("state.vscdb");
        let full_assistant_body = format!("{}trae-tail", "bounded ".repeat(400));
        write_value(
            &source,
            &chat_value("cold exact nested sentinel", &full_assistant_body),
        );

        let mut pages = Vec::new();
        let scan = scan_trae_source_backed_explicit_v0(&source, &mut |page| {
            pages.push(page);
            Ok(())
        })
        .expect("cold scan");
        assert_eq!(scan.source.counts().complete_records, 2);
        assert_eq!(scan.source.counts().retained_records, 2);
        assert_eq!(scan.source.counts().indexed_documents, 2);
        assert!(scan.source.counts().certified_bytes > 0);
        let documents = pages
            .iter()
            .flat_map(|page| page.documents.iter())
            .collect::<Vec<_>>();
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].body, "cold exact nested sentinel");
        assert_eq!(documents[1].body, full_assistant_body);
        assert!(documents[1].body.ends_with("trae-tail"));
        assert_eq!(documents[0].parent_session_id, None);
        assert_eq!(documents[0].root_session_id, documents[0].session_id);
        assert_eq!(
            documents[0].provider_session_id.as_deref(),
            Some("workspace-explicit/native-session-stable")
        );
        assert_eq!(documents[0].branch, None);
        assert_eq!(
            documents[0].source_path.as_deref(),
            Some(source.to_string_lossy().as_ref())
        );
        assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
        assert!(documents[0].is_primary);

        let NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key,
            row_version,
        } = documents[0].locator.coordinate()
        else {
            panic!("expected provider SQLite locator");
        };
        assert_eq!(logical_relation, TRAE_LOCATOR_RELATION);
        assert!(matches!(
            row_version,
            Some(TypedKey::Bytes(digest)) if digest.len() == 32
        ));
        let TypedKey::Composite(parts) = primary_key else {
            panic!("expected composite locator");
        };
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], TypedKey::Utf8(TRAE_CHAT_KEYS[0].to_owned()));
        assert_eq!(parts[1], TypedKey::U64(0));
        assert_eq!(parts[2], TypedKey::U64(0));

        let hydrated = hydrate_trae_source_backed_locator_v0(&source, &documents[0].locator)
            .expect("exact hydration");
        assert_eq!(hydrated.exact_text, "cold exact nested sentinel");
        let hydrated_assistant =
            hydrate_trae_source_backed_locator_v0(&source, &documents[1].locator)
                .expect("exact assistant hydration");
        assert_eq!(hydrated_assistant.exact_text, documents[1].body);

        let mut replay_ids = Vec::new();
        scan_trae_source_backed_explicit_v0(&source, &mut |page| {
            replay_ids.extend(page.documents.into_iter().map(|document| document.event_id));
            Ok(())
        })
        .expect("repeat scan");
        assert_eq!(
            replay_ids,
            documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );

        let error = scan_trae_source_backed_explicit_v0(&workspace, &mut |_| Ok(())).unwrap_err();
        assert!(matches!(
            error,
            TraeSourceBackedErrorV0::ExplicitLeafRequired
        ));
    }

    #[test]
    fn nested_value_replacement_keeps_native_ids_and_rejects_stale_locator() {
        let temp = crate::test_support_paths::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace-replacement");
        fs::create_dir_all(&workspace).expect("workspace");
        let source = workspace.join("state.vscdb");
        write_value(
            &source,
            &chat_value("before replacement", "assistant before"),
        );

        let (before_scan, before_documents) = collect_scan(&source);
        let stale_locator = before_documents[0].locator.clone();
        write_value(&source, &chat_value("after replacement", "assistant after"));
        let (after_scan, after_documents) = collect_scan(&source);

        assert_ne!(
            before_scan.source.content_digest(),
            after_scan.source.content_digest()
        );
        assert_eq!(
            before_documents
                .iter()
                .map(|document| document.session_id)
                .collect::<Vec<_>>(),
            after_documents
                .iter()
                .map(|document| document.session_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            before_documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>(),
            after_documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );
        let error = hydrate_trae_source_backed_locator_v0(&source, &stale_locator).unwrap_err();
        assert!(matches!(
            error,
            TraeSourceBackedErrorV0::SourceRevisionMismatch
                | TraeSourceBackedErrorV0::LocatorValueDigestMismatch
        ));
        assert_eq!(
            hydrate_trae_source_backed_locator_v0(&source, &after_documents[0].locator)
                .expect("replacement hydration")
                .exact_text,
            "after replacement"
        );
    }

    #[test]
    fn source_backed_scans_chatstore_and_cn_stream_shapes() {
        let temp = crate::test_support_paths::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace-stream-shapes");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(
            workspace.join("workspace.json"),
            r#"{"folder":"file:///workspace/trae-stream-shapes"}"#,
        )
        .expect("workspace metadata");
        let source = workspace.join("state.vscdb");
        write_key_value(
            &source,
            "ChatStore",
            &json!({
                "entries": {
                    "drift-session": {
                        "id": "drift-session",
                        "messages": [
                            {
                                "id": "drift-user",
                                "role": "user",
                                "content": [{"type": "text", "text": "chatstore prompt"}],
                                "createdAt": "2026-07-28T12:00:00Z"
                            },
                            {
                                "id": "drift-assistant",
                                "role": "assistant",
                                "content": {"summary": "chatstore answer"},
                                "createdAt": "2026-07-28T12:00:01Z"
                            }
                        ]
                    }
                }
            }),
        );
        write_key_value(
            &source,
            super::super::super::TRAE_CN_INPUT_HISTORY_KEY,
            &json!([
                {
                    "id": "cn-input-1",
                    "inputText": "cn prompt alpha",
                    "createdAt": "2026-07-28T12:01:00Z"
                },
                {
                    "id": "cn-input-2",
                    "text": "cn prompt beta",
                    "createdAt": "2026-07-28T12:01:01Z"
                }
            ]),
        );

        let (_, documents) = collect_scan(&source);
        assert_eq!(documents.len(), 4);
        assert!(
            documents
                .iter()
                .all(|document| document.workspace.as_deref()
                    == Some("/workspace/trae-stream-shapes"))
        );
        for expected in [
            ("chatstore prompt", "user"),
            ("chatstore answer", "assistant"),
            ("cn prompt alpha", "user"),
            ("cn prompt beta", "user"),
        ] {
            let document = documents
                .iter()
                .find(|document| document.body == expected.0)
                .expect("stream-shape document");
            assert_eq!(document.role.as_deref(), Some(expected.1));
            assert_eq!(
                hydrate_trae_source_backed_locator_v0(&source, &document.locator)
                    .expect("stream-shape hydration")
                    .exact_text,
                expected.0
            );
        }
        let (_, replayed) = collect_scan(&source);
        assert_eq!(document_ids(&documents), document_ids(&replayed));
    }

    #[test]
    fn append_rewrite_truncate_delete_unavailable_and_stale_are_source_exact() {
        let temp = crate::test_support_paths::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace-lifecycle");
        fs::create_dir_all(&workspace).expect("workspace");
        let source = workspace.join("state.vscdb");

        write_value(
            &source,
            &chat_value_from_messages(&[
                ("native-message-1", "user", "cold body"),
                ("native-message-2", "assistant", "second body"),
            ]),
        );
        let (cold_scan, cold_documents) = collect_scan(&source);
        let cold_ids = document_ids(&cold_documents);
        let cold_locator = cold_documents[0].locator.clone();
        assert_eq!(cold_documents.len(), 2);

        write_value(
            &source,
            &chat_value_from_messages(&[
                ("native-message-1", "user", "cold body"),
                ("native-message-2", "assistant", "second body"),
                ("native-message-3", "assistant", "appended body"),
            ]),
        );
        let (append_scan, appended) = collect_scan(&source);
        assert_eq!(&document_ids(&appended)[..2], cold_ids.as_slice());
        assert_eq!(appended[2].body, "appended body");
        assert_ne!(
            cold_scan.source.content_digest(),
            append_scan.source.content_digest()
        );
        assert!(hydrate_trae_source_backed_locator_v0(&source, &cold_locator).is_err());
        let appended_locator = appended[2].locator.clone();

        write_value(
            &source,
            &chat_value_from_messages(&[
                ("native-message-1", "user", "rewritten body"),
                ("native-message-2", "assistant", "second body"),
                ("native-message-3", "assistant", "appended body"),
            ]),
        );
        let (rewrite_scan, rewritten) = collect_scan(&source);
        assert_eq!(document_ids(&rewritten), document_ids(&appended));
        assert_eq!(rewritten[0].body, "rewritten body");
        assert_eq!(
            hydrate_trae_source_backed_locator_v0(&source, &rewritten[0].locator)
                .expect("rewritten hydration")
                .exact_text,
            "rewritten body"
        );
        assert_ne!(
            append_scan.source.content_digest(),
            rewrite_scan.source.content_digest()
        );

        write_value(
            &source,
            &chat_value_from_messages(&[("native-message-1", "user", "rewritten body")]),
        );
        let (truncate_scan, truncated) = collect_scan(&source);
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].event_id, cold_ids[0]);
        assert!(hydrate_trae_source_backed_locator_v0(&source, &appended_locator).is_err());
        let truncated_locator = truncated[0].locator.clone();
        assert_ne!(
            rewrite_scan.source.content_digest(),
            truncate_scan.source.content_digest()
        );

        delete_value(&source);
        let (delete_scan, deleted) = collect_scan(&source);
        assert!(deleted.is_empty());
        assert_eq!(delete_scan.source.counts().complete_records, 0);
        assert_eq!(delete_scan.source.counts().retained_records, 0);
        assert_eq!(delete_scan.source.counts().indexed_documents, 0);
        assert_eq!(
            delete_scan.source.observation().source(),
            truncate_scan.source.observation().source()
        );
        assert_ne!(
            delete_scan.source.content_digest(),
            truncate_scan.source.content_digest()
        );
        assert!(hydrate_trae_source_backed_locator_v0(&source, &truncated_locator).is_err());

        let unavailable = workspace.join("state.vscdb.unavailable");
        fs::rename(&source, &unavailable).expect("make source unavailable");
        let mut emitted = false;
        let error = scan_trae_source_backed_explicit_v0(&source, &mut |_| {
            emitted = true;
            Ok(())
        })
        .unwrap_err();
        assert!(!emitted);
        assert!(matches!(
            error,
            TraeSourceBackedErrorV0::Capture(CaptureError::Io(_))
        ));
    }

    #[test]
    fn source_backed_route_has_no_legacy_store_publication_fallback() {
        let module_source = include_str!("../nativepath.rs");
        let scanner_source = include_str!("scanner.rs");
        let source_backed_source = include_str!("source_backed.rs");
        let source_backed_production = source_backed_source
            .split("#[cfg(test)]")
            .next()
            .expect("source-backed production section");
        let provider_source = include_str!("../../trae.rs");
        for source in [module_source, scanner_source, source_backed_production] {
            for forbidden in [
                "EventSearchBulkGuard",
                "NativePathPublicationGroup",
                "NativePathCursorSetClassification",
                "NativePathCursorTransition",
                "NativePro",
                "process_pro_replay_only",
                "ProviderSourceRouteRetirement",
                "ProOutputSink",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "retained Trae source-backed code contains {forbidden}"
                );
            }
            assert!(!source.contains("ctx_history_store"));
        }
        assert!(!provider_source.contains("ctx_history_store"));
        assert!(!provider_source.contains("legacy Store publication"));
    }

    fn collect_scan(source: &Path) -> (TraeSourceBackedScanV0, Vec<LexicalDocument>) {
        let mut documents = Vec::new();
        let scan = scan_trae_source_backed_explicit_v0(source, &mut |page| {
            documents.extend(page.documents);
            Ok(())
        })
        .expect("scan");
        (scan, documents)
    }

    fn document_ids(documents: &[LexicalDocument]) -> Vec<ctx_history_core::StableEntityId> {
        documents.iter().map(|document| document.event_id).collect()
    }

    fn chat_value(user: &str, assistant: &str) -> Value {
        json!({
            "list": [{
                "id": "native-session-stable",
                "title": "Source-backed Trae",
                "messages": [
                    {
                        "id": "native-message-user",
                        "role": "user",
                        "content": user,
                        "createdAt": "2026-07-28T12:00:00Z"
                    },
                    {
                        "id": "native-message-assistant",
                        "role": "assistant",
                        "content": assistant,
                        "createdAt": "2026-07-28T12:00:01Z"
                    }
                ]
            }]
        })
    }

    fn chat_value_from_messages(messages: &[(&str, &str, &str)]) -> Value {
        json!({
            "list": [{
                "id": "native-session-stable",
                "title": "Source-backed Trae",
                "messages": messages
                    .iter()
                    .enumerate()
                    .map(|(index, (id, role, body))| {
                        json!({
                            "id": id,
                            "role": role,
                            "content": body,
                            "createdAt": format!("2026-07-28T12:00:{index:02}Z"),
                        })
                    })
                    .collect::<Vec<_>>()
            }]
        })
    }

    fn write_value(path: &Path, value: &Value) {
        write_key_value(path, TRAE_CHAT_KEYS[0], value);
    }

    fn write_key_value(path: &Path, key: &str, value: &Value) {
        if !path.exists() {
            let conn = Connection::open(path).expect("open fixture");
            conn.execute(
                "create table ItemTable ([key] text primary key, value text)",
                [],
            )
            .expect("schema");
        }
        let conn = Connection::open(path).expect("reopen fixture");
        conn.execute(
            "insert into ItemTable ([key], value) values (?1, ?2)
             on conflict([key]) do update set value = excluded.value",
            params![key, value.to_string()],
        )
        .expect("write value");
    }

    fn delete_value(path: &Path) {
        Connection::open(path)
            .expect("open fixture for deletion")
            .execute(
                "delete from ItemTable where [key] = ?1",
                params![TRAE_CHAT_KEYS[0]],
            )
            .expect("delete value");
    }
}
