use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::*;

const TRAE_SOURCE_ANCHOR_NAMESPACE: &str = "trae.workspace-storage";
const TRAE_SOURCE_SCHEMA_VARIANT: &str = "trae-itemtable-json-v1";
const TRAE_SOURCE_REVISION_KIND: &str = "trae-sqlite-snapshot-v1";
const TRAE_SOURCE_BACKED_PARSER_REVISION: &str = "trae-itemtable-source-backed-v1";
const TRAE_NATIVE_SESSION_NAMESPACE: &str = "trae.itemtable-session-v1";
const TRAE_SESSION_POSITION_KIND: &str = "trae.itemtable-session-position-v1";
const TRAE_NATIVE_ITEM_NAMESPACE: &str = "trae.itemtable-key-v1";
const TRAE_NATIVE_MESSAGE_NAMESPACE: &str = "trae.itemtable-message-v1";
const TRAE_MESSAGE_POSITION_KIND: &str = "trae.itemtable-message-position-v1";
const TRAE_LOGICAL_SESSION_KIND: &str = "trae-session";
const TRAE_LOGICAL_EVENT_KIND: &str = "trae-message";
const TRAE_LOCATOR_NAMESPACE: &str = "trae.itemtable-json-message-v1";
const TRAE_LOCATOR_RELATION: &str = "ItemTable";

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
    pub(crate) rejections: Vec<ProviderImportFailure>,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedScanV0 {
    pub(crate) source: CertifiedSource,
    pub(crate) emitted_pages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraeHydratedRecordV0 {
    pub(crate) exact_text: String,
}

/// Scans exactly one explicitly supplied `state.vscdb` leaf.
///
/// The callback receives bounded pages from the existing ItemTable scanner.
/// This leaf owns no automatic inventory, lifecycle, or publication behavior.
pub(crate) fn scan_trae_source_backed_explicit_v0(
    path: &Path,
    emit: &mut dyn FnMut(TraeSourceBackedPageV0) -> TraeSourceBackedResultV0<()>,
) -> TraeSourceBackedResultV0<TraeSourceBackedScanV0> {
    let canonical_path = explicit_trae_leaf(path)?;
    let source_root = canonical_path.parent().unwrap_or(canonical_path.as_path());
    let authority = acquire_source(&canonical_path, source_root, DateTime::<Utc>::UNIX_EPOCH)?;
    let source = source_key(&authority)?;
    let opening = source_observation(&source, &authority.source_revision)?;
    let revision_digest = source_revision_digest(&authority.source_revision);
    let mut scanner = TraeScanner::new(&authority, TraeFrontier::default());
    let mut counts = ScannedSourceCounts::default();
    let mut emitted_pages = 0_u64;

    while let Some(page) = authority
        .database
        .read(&canonical_path, |conn| scanner.next_page(conn, true, false))?
    {
        let complete_records = u64::try_from(page.logical_units)
            .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let rejected_records = u64::try_from(page.rejections.len())
            .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let mut documents = Vec::with_capacity(page.core.len());
        for record in page.core {
            if let Some(document) = lexical_document(&source, &authority, revision_digest, record)?
            {
                documents.push(document);
            }
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
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
        emitted_pages = checked_add(emitted_pages, 1)?;

        emit(TraeSourceBackedPageV0 {
            documents,
            rejections: page.rejections,
            complete_records,
            retained_records,
            ignored_records,
            terminal: page.terminal,
        })?;
    }

    authority.database.revalidate()?;
    counts.certified_bytes = scanner.certified_source_bytes();
    let closing = source_observation(&source, &authority.source_revision)?;
    let source = CertifiedSource::certify(
        opening,
        closing,
        TRAE_SOURCE_BACKED_PARSER_REVISION,
        scanner.source_content_digest(),
        counts,
    )?;
    Ok(TraeSourceBackedScanV0 {
        source,
        emitted_pages,
    })
}

pub(crate) fn hydrate_trae_source_backed_locator_v0(
    path: &Path,
    locator: &SourceRecordLocator,
) -> TraeSourceBackedResultV0<TraeHydratedRecordV0> {
    locator.validate_contract()?;
    let canonical_path = explicit_trae_leaf(path)?;
    let source_root = canonical_path.parent().unwrap_or(canonical_path.as_path());
    let authority = acquire_source(&canonical_path, source_root, DateTime::<Utc>::UNIX_EPOCH)?;
    let source = source_key(&authority)?;
    if !source.exact_descriptor_eq(locator.source()) {
        return Err(TraeSourceBackedErrorV0::LocatorSourceMismatch);
    }
    let current_revision_digest = source_revision_digest(&authority.source_revision);
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || locator.certified_source_revision_digest() != Some(&current_revision_digest)
    {
        return Err(TraeSourceBackedErrorV0::SourceRevisionMismatch);
    }
    let coordinate = decode_locator(locator)?;
    let key_index = TRAE_CHAT_KEYS
        .iter()
        .position(|candidate| *candidate == coordinate.chat_key)
        .and_then(|index| u16::try_from(index).ok())
        .ok_or(TraeSourceBackedErrorV0::InvalidLocator)?;
    let value = authority
        .database
        .read(&canonical_path, |conn| {
            super::super::trae_complete_value(conn, key_index)
        })?
        .ok_or(TraeSourceBackedErrorV0::LocatorValueMissing)?;
    let actual_digest: [u8; 32] = Sha256::digest(&value).into();
    if actual_digest != coordinate.value_digest || &actual_digest != locator.record_digest() {
        return Err(TraeSourceBackedErrorV0::LocatorValueDigestMismatch);
    }
    let (_, exact_text) = super::super::trae_complete_message(
        &value,
        key_index,
        coordinate.session_index,
        coordinate.message_index,
        &coordinate.provider_session_id,
    )?
    .ok_or(TraeSourceBackedErrorV0::LocatorMessageMissing)?;
    Ok(TraeHydratedRecordV0 { exact_text })
}

fn lexical_document(
    source: &SourceKey,
    authority: &TraeSourceAuthority,
    revision_digest: [u8; 32],
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
    let coordinate = NativeRecordCoordinate::ProviderNative {
        namespace: TRAE_LOCATOR_NAMESPACE.to_owned(),
        coordinate: TypedKey::composite(vec![
            TypedKey::utf8(TRAE_LOCATOR_RELATION)?,
            TypedKey::utf8(record.chat_key)?,
            TypedKey::bytes(record.value_digest.to_vec())?,
            TypedKey::U64(u64::from(record.raw_session_index)),
            TypedKey::U64(u64::from(record.message_index)),
            TypedKey::utf8(&record.provider_session_id)?,
        ])?,
    };
    let locator = SourceRecordLocator::new(
        source.clone(),
        coordinate,
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
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
        occurred_at_unix_ms: Some(record.event.occurred_at.timestamp_millis()),
        event_type: record.event.event_type.as_str().to_owned(),
        role: record.event.role.map(|role| role.as_str().to_owned()),
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
    let anchor = SourceAnchor::provider_native(
        TRAE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(&authority.workspace_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Trae.as_str(),
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        TRAE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn source_observation(
    source: &SourceKey,
    source_revision: &str,
) -> TraeSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        TRAE_SOURCE_REVISION_KIND,
        source_revision.as_bytes().to_vec(),
    )?)
}

fn source_revision_digest(source_revision: &str) -> [u8; 32] {
    Sha256::digest(source_revision.as_bytes()).into()
}

fn explicit_trae_leaf(path: &Path) -> TraeSourceBackedResultV0<PathBuf> {
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

struct DecodedLocator {
    chat_key: String,
    value_digest: [u8; 32],
    session_index: u32,
    message_index: u32,
    provider_session_id: String,
}

fn decode_locator(locator: &SourceRecordLocator) -> TraeSourceBackedResultV0<DecodedLocator> {
    if locator.source().provider() != CaptureProvider::Trae.as_str()
        || locator.source().source_format() != TRAE_STATE_VSCDB_SOURCE_FORMAT
        || locator.source().schema_variant() != TRAE_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
    {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate,
    } = locator.coordinate()
    else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    let TypedKey::Composite(parts) = coordinate else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::Utf8(relation), TypedKey::Utf8(chat_key), TypedKey::Bytes(value_digest), TypedKey::U64(session_index), TypedKey::U64(message_index), TypedKey::Utf8(provider_session_id)] =
        parts.as_slice()
    else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    if namespace != TRAE_LOCATOR_NAMESPACE
        || relation != TRAE_LOCATOR_RELATION
        || value_digest.len() != 32
        || !TRAE_CHAT_KEYS.contains(&chat_key.as_str())
    {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    }
    let mut value_digest_bytes = [0_u8; 32];
    value_digest_bytes.copy_from_slice(value_digest);
    Ok(DecodedLocator {
        chat_key: chat_key.clone(),
        value_digest: value_digest_bytes,
        session_index: u32::try_from(*session_index)
            .map_err(|_| TraeSourceBackedErrorV0::InvalidLocator)?,
        message_index: u32::try_from(*message_index)
            .map_err(|_| TraeSourceBackedErrorV0::InvalidLocator)?,
        provider_session_id: provider_session_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::{params, Connection};
    use serde_json::json;

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
        assert!(scan.emitted_pages > 0);
        assert!(pages.last().is_some_and(|page| page.terminal));
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

        let NativeRecordCoordinate::ProviderNative {
            namespace,
            coordinate,
        } = documents[0].locator.coordinate()
        else {
            panic!("expected provider-native locator");
        };
        assert_eq!(namespace, TRAE_LOCATOR_NAMESPACE);
        let TypedKey::Composite(parts) = coordinate else {
            panic!("expected composite locator");
        };
        assert_eq!(parts.len(), 6);
        assert_eq!(parts[0], TypedKey::Utf8("ItemTable".to_owned()));
        assert_eq!(parts[1], TypedKey::Utf8(TRAE_CHAT_KEYS[0].to_owned()));
        assert_eq!(parts[3], TypedKey::U64(0));
        assert_eq!(parts[4], TypedKey::U64(0));

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

    fn collect_scan(source: &Path) -> (TraeSourceBackedScanV0, Vec<LexicalDocument>) {
        let mut documents = Vec::new();
        let scan = scan_trae_source_backed_explicit_v0(source, &mut |page| {
            documents.extend(page.documents);
            Ok(())
        })
        .expect("scan");
        (scan, documents)
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

    fn write_value(path: &Path, value: &Value) {
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
            params![TRAE_CHAT_KEYS[0], value.to_string()],
        )
        .expect("write value");
    }
}
