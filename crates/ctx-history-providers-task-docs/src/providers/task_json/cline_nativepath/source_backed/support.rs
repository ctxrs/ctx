use super::*;
use crate::{ProviderSourceKind, ProviderSourceStatus};

use super::super::{discover_cline_root, discover_roo_root};

pub(super) struct SelectedRoots {
    pub(super) roots: Vec<PathBuf>,
    pub(super) detected_but_unsupported: Vec<ProviderSource>,
    pub(super) unavailable: Vec<ProviderSource>,
}

pub(super) fn select_authoritative_roots(
    dialect: TaskJsonNativeDialect,
    selected: &[ProviderSource],
) -> SelectedRoots {
    let mut roots = Vec::new();
    let mut detected_but_unsupported = Vec::new();
    let mut unavailable = Vec::new();
    for source in selected
        .iter()
        .filter(|source| source.provider == dialect.provider)
    {
        let exact_format = source.source_format == dialect.source_format;
        let supported = exact_format
            && source.source_kind == ProviderSourceKind::NativeHistory
            && source.import_support.is_importable();
        if !supported {
            detected_but_unsupported.push(source.clone());
        } else if matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        ) && source.exists
        {
            roots.push(source.path.clone());
        } else {
            unavailable.push(source.clone());
        }
    }
    SelectedRoots {
        roots,
        detected_but_unsupported,
        unavailable,
    }
}

pub(super) fn discover_root(
    dialect: TaskJsonNativeDialect,
    root: &Path,
) -> Result<ClineDiscovery, ClineNativePathError> {
    match dialect.provider {
        CaptureProvider::Cline => discover_cline_root(root),
        CaptureProvider::RooCode => discover_roo_root(root),
        _ => unreachable!("task-JSON source-backed adapter has only Cline and Roo dialects"),
    }
}

pub(super) fn task_source_key_scoped(
    dialect: TaskJsonNativeDialect,
    task: &ClineLiveTaskObservation,
    source_anchor_scope: SourceAnchorScope,
) -> TaskJsonSourceBackedResult<SourceKey> {
    task_source_key_for_id_scoped(
        dialect,
        task.directory_task_id.as_ref(),
        source_anchor_scope,
    )
}

pub(super) fn task_source_key_for_id_scoped(
    dialect: TaskJsonNativeDialect,
    task_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> TaskJsonSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_provider_native_scoped(
        dialect.provider.as_str(),
        dialect.source_format,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(task_id)?,
        source_anchor_scope,
    )?)
}

pub(super) fn task_observation(
    source: &SourceKey,
    task: &ClineLiveTaskObservation,
) -> TaskJsonSourceBackedResult<SourceObservation> {
    let mut revision = Sha256::new();
    revision.update(b"ctx-task-json-source-revision-v1\0");
    revision.update(source.identity().digest());
    for component in [
        ClineComponent::ApiHistory,
        ClineComponent::UiMessages,
        ClineComponent::FallbackHistory,
        ClineComponent::TaskMetadata,
        ClineComponent::HistoryItem,
        ClineComponent::TaskIndex,
    ] {
        revision.update([component as u8]);
        match &task.component(component).state {
            ClineObservedFileState::Missing => revision.update([0]),
            ClineObservedFileState::Present(stamp) => {
                revision.update([1]);
                revision.update(stamp.len().to_le_bytes());
                let token = stamp.token();
                revision.update(token.len().to_le_bytes());
                revision.update(token.as_bytes());
            }
            ClineObservedFileState::Unavailable(message) => {
                revision.update([2]);
                revision.update(message.len().to_le_bytes());
                revision.update(message.as_bytes());
            }
        }
    }
    SourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        revision.finalize().to_vec(),
    )
    .map_err(Into::into)
}

pub(super) fn digest_revision(observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-source-revision-evidence-v1\0");
    digest.update(observation.revision_kind().as_bytes());
    digest.update(observation.revision());
    digest.finalize().into()
}

pub(super) fn derive_task_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> TaskJsonSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

pub(super) fn native_item_key(
    event: &ClineEventRow,
    revision_digest: [u8; 32],
) -> TaskJsonSourceBackedResult<NativeItemKey> {
    Ok(match &event.identity.item {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => NativeItemKey::composite(
            NATIVE_ITEM_NAMESPACE,
            vec![
                TypedKey::U64(event.identity.component as u64),
                TypedKey::utf8(native_id.as_ref())?,
                TypedKey::U64(*occurrence),
            ],
        )?,
        ClineNativeItemKey::ComponentOrdinal(ordinal) => NativeItemKey::revision_scoped_position(
            NATIVE_ITEM_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(event.identity.component as u64),
                TypedKey::U64(*ordinal),
            ])?,
            TypedKey::bytes(revision_digest.to_vec())?,
        )?,
    })
}

pub(super) fn typed_native_item_key(
    item: &ClineNativeItemKey,
) -> TaskJsonSourceBackedResult<TypedKey> {
    Ok(match item {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => TypedKey::composite(vec![
            TypedKey::U64(0),
            TypedKey::utf8(native_id.as_ref())?,
            TypedKey::U64(*occurrence),
        ])?,
        ClineNativeItemKey::ComponentOrdinal(ordinal) => {
            TypedKey::composite(vec![TypedKey::U64(1), TypedKey::U64(*ordinal)])?
        }
    })
}

pub(super) fn event_sequence(
    dialect: TaskJsonNativeDialect,
    event: &ClineEventRow,
) -> TaskJsonSourceBackedResult<u64> {
    const SUBRECORD_BITS: u32 = 20;
    const ITEM_BITS: u32 = 41;
    if event.native_order.item_index >= (1_u64 << ITEM_BITS)
        || u64::from(event.native_order.sub_index) >= (1_u64 << SUBRECORD_BITS)
    {
        return Err(TaskJsonSourceBackedError::EventSequenceBound {
            provider: dialect.display_name,
        });
    }
    Ok(
        ((event.native_order.component as u64) << (ITEM_BITS + SUBRECORD_BITS))
            | (event.native_order.item_index << SUBRECORD_BITS)
            | u64::from(event.native_order.sub_index),
    )
}

pub(super) fn lexical_event_body(event: &ClineEventRow) -> String {
    let candidate = event
        .body
        .as_deref()
        .or_else(|| {
            event
                .tool_call
                .as_ref()
                .and_then(|call| call.name.as_deref().or(call.call_id.as_deref()))
        })
        .unwrap_or_else(|| event_kind(event.kind));
    candidate.to_owned()
}

pub(super) fn event_kind(kind: ClineEventKind) -> &'static str {
    match kind {
        ClineEventKind::Message => "message",
        ClineEventKind::Summary => "summary",
        ClineEventKind::Notice => "notice",
        ClineEventKind::ToolCall => "tool_call",
        ClineEventKind::ToolOutput => "tool_output",
        ClineEventKind::CommandOutput => "command_output",
    }
}

pub(super) fn event_role(role: ClineEventRole) -> &'static str {
    match role {
        ClineEventRole::User => "user",
        ClineEventRole::Assistant => "assistant",
        ClineEventRole::System => "system",
        ClineEventRole::Unknown => "unknown",
    }
}

pub(super) fn hash_record_evidence(
    digest: &mut Sha256,
    component: ClineComponent,
    evidence: ClineSourceRecordEvidence,
) {
    digest.update(b"record\0");
    digest.update([component as u8]);
    digest.update(evidence.native_index.to_le_bytes());
    digest.update(evidence.byte_start.to_le_bytes());
    digest.update(evidence.byte_length.to_le_bytes());
    digest.update(evidence.record_digest);
}

pub(super) fn hash_metadata_checkpoint(digest: &mut Sha256, checkpoint: &ClineTaskCheckpoint) {
    digest.update(b"metadata\0");
    digest.update([checkpoint.task_metadata.observation.component as u8]);
    match checkpoint.task_metadata.content_sha256 {
        Some(content) => {
            digest.update([1]);
            digest.update(content);
        }
        None => digest.update([0]),
    }
    digest.update(checkpoint.task_metadata.session.metadata_hash);
}

pub(super) fn hash_array_checkpoint(digest: &mut Sha256, checkpoint: &ClineArrayCheckpoint) {
    digest.update(b"array\0");
    digest.update([checkpoint.component as u8]);
    digest.update(checkpoint.complete_bytes.to_le_bytes());
    digest.update(checkpoint.observed_items.to_le_bytes());
    digest.update(checkpoint.retained_rows.to_le_bytes());
    digest.update(checkpoint.certified_revision_sha256);
    digest.update(checkpoint.final_frontier.prefix_semantic_sha256);
}

pub(super) fn checked_add(
    dialect: TaskJsonNativeDialect,
    left: u64,
    right: u64,
) -> TaskJsonSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| count_overflow(dialect))
}

pub(super) fn count_overflow(dialect: TaskJsonNativeDialect) -> TaskJsonSourceBackedError {
    TaskJsonSourceBackedError::CountOverflow {
        provider: dialect.display_name,
    }
}
