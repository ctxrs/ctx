use super::*;
use crate::provider::source_backed::{
    family::jsonl::JsonlAppendOccurrenceState, BaseEventLookup as _,
};
use std::sync::Arc;

const CODEX_NATIVE_EVENT_NAMESPACE: &str = "codex.event.v1";
const CODEX_PROVIDER_EVENT_KEY_VERSION: &str = "provider-native-v1";
const CODEX_FALLBACK_EVENT_KEY_VERSION: &str = "fallback-v1";
const CODEX_PROVIDER_EVENT_OCCURRENCE_DOMAIN: &[u8] =
    b"ctx/codex-nativepath/provider-event-occurrence/v1\0";
const CODEX_FALLBACK_EVENT_DIGEST_DOMAIN: &[u8] = b"ctx/codex-nativepath/fallback-event/v1\0";

#[derive(Clone)]
struct CodexEventLookup(
    Arc<dyn Fn(uuid::Uuid) -> std::result::Result<bool, CaptureError> + Send + Sync>,
);

impl crate::provider::source_backed::BaseEventLookup for CodexEventLookup {
    type Error = CaptureError;

    fn contains(&self, event_id: uuid::Uuid) -> std::result::Result<bool, CaptureError> {
        (self.0)(event_id)
    }
}

#[derive(Default)]
pub(in crate::codex::nativepath) struct CodexEventIdentityStateV0 {
    occurrences: JsonlAppendOccurrenceState<[u8; 32], CodexEventLookup>,
}

impl CodexEventIdentityStateV0 {
    pub(in crate::codex::nativepath) fn for_append(
        base_lookup: impl crate::provider::source_backed::BaseEventLookup<Error = impl std::error::Error>
            + 'static,
    ) -> Self {
        let lookup = CodexEventLookup(Arc::new(move |event_id| {
            base_lookup
                .contains(event_id)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
        }));
        Self {
            occurrences: JsonlAppendOccurrenceState::for_append(lookup),
        }
    }

    fn next_identity(
        &mut self,
        source: &SourceKey,
        session_id: StableEntityId,
        row: &CodexCoreRecordDraft,
    ) -> CodexSourceBackedResultV0<(StableEntityId, TypedKey, u64)> {
        let (occurrence_key, parts) = match row.provider_event_identity.as_ref() {
            Some(provider_identity) => provider_event_key(row, provider_identity)?,
            None => fallback_event_key(row)?,
        };
        let occurrence = self.occurrences.next(
            occurrence_key,
            || CodexSourceBackedErrorV0::CountOverflow,
            |base_lookup, occurrence| {
                base_occurrence_exists(base_lookup, source, session_id, &parts, occurrence)
            },
        )?;
        let (event_id, native_event_id) =
            event_identity_for_occurrence(source, session_id, &parts, occurrence)?;
        Ok((event_id, native_event_id, occurrence))
    }
}

fn base_occurrence_exists(
    base_lookup: &CodexEventLookup,
    source: &SourceKey,
    session_id: StableEntityId,
    parts: &[TypedKey],
    occurrence: u64,
) -> CodexSourceBackedResultV0<bool> {
    let (event_id, _) = event_identity_for_occurrence(source, session_id, parts, occurrence)?;
    Ok(base_lookup
        .contains(event_id.as_uuid())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?)
}

fn event_identity_for_occurrence(
    source: &SourceKey,
    session_id: StableEntityId,
    parts: &[TypedKey],
    occurrence: u64,
) -> CodexSourceBackedResultV0<(StableEntityId, TypedKey)> {
    let mut native_parts = parts.to_vec();
    native_parts.push(TypedKey::U64(occurrence));
    let native_event_id = TypedKey::composite(native_parts.clone())?;
    let native_item_key = NativeItemKey::composite(CODEX_NATIVE_EVENT_NAMESPACE, native_parts)?;
    let event_id = codex_event_identity(source, session_id, &native_item_key)?;
    Ok((event_id, native_event_id))
}

fn provider_event_key(
    row: &CodexCoreRecordDraft,
    provider_identity: &CodexProviderEventIdentityV0,
) -> CodexSourceBackedResultV0<([u8; 32], Vec<TypedKey>)> {
    provider_event_key_parts(
        row.event_type.as_str(),
        row.role.map(|role| role.as_str()),
        provider_identity,
    )
}

fn provider_event_key_parts(
    event_type: &str,
    role: Option<&str>,
    provider_identity: &CodexProviderEventIdentityV0,
) -> CodexSourceBackedResultV0<([u8; 32], Vec<TypedKey>)> {
    let mut hasher = Sha256::new();
    hasher.update(CODEX_PROVIDER_EVENT_OCCURRENCE_DOMAIN);
    hash_identity_text(&mut hasher, provider_identity.kind.as_str());
    hash_identity_text(&mut hasher, &provider_identity.value);
    hash_identity_text(&mut hasher, event_type);
    hash_identity_optional_text(&mut hasher, role);
    let occurrence_key = hasher.finalize().into();
    let parts = vec![
        TypedKey::utf8(CODEX_PROVIDER_EVENT_KEY_VERSION)?,
        TypedKey::utf8(provider_identity.kind.as_str())?,
        TypedKey::utf8(&provider_identity.value)?,
        TypedKey::utf8(event_type)?,
        role.map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
    ];
    Ok((occurrence_key, parts))
}

fn fallback_event_key(
    row: &CodexCoreRecordDraft,
) -> CodexSourceBackedResultV0<([u8; 32], Vec<TypedKey>)> {
    let mut hasher = Sha256::new();
    hasher.update(CODEX_FALLBACK_EVENT_DIGEST_DOMAIN);
    hasher.update(row.occurred_at.timestamp().to_le_bytes());
    hasher.update(row.occurred_at.timestamp_subsec_nanos().to_le_bytes());
    hash_identity_text(&mut hasher, row.event_type.as_str());
    hash_identity_optional_text(&mut hasher, row.role.map(|role| role.as_str()));
    hash_identity_text(&mut hasher, &row.lexical_body);
    let digest: [u8; 32] = hasher.finalize().into();
    Ok((
        digest,
        vec![
            TypedKey::utf8(CODEX_FALLBACK_EVENT_KEY_VERSION)?,
            TypedKey::bytes(digest.to_vec())?,
        ],
    ))
}

fn hash_identity_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_identity_text(hasher, value);
    }
}

fn hash_identity_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

pub(in crate::codex::nativepath) fn codex_source_key_in_root(
    source_root_lineage: Option<[u8; 32]>,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<SourceKey> {
    let scope =
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage);
    Ok(SourceKey::derive_provider_native_scoped(
        CaptureProvider::Codex.as_str(),
        CODEX_SESSION_SOURCE_FORMAT,
        CODEX_SOURCE_SCHEMA_VARIANT,
        1,
        CODEX_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
        scope,
    )?)
}

pub(in crate::codex::nativepath) fn codex_session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<StableEntityId> {
    Ok(derive_native_session_id(
        source,
        CODEX_LOGICAL_SESSION_KIND,
        CODEX_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?)
}

pub(super) fn codex_event_identity(
    source: &SourceKey,
    session_id: StableEntityId,
    native_item_key: &NativeItemKey,
) -> CodexSourceBackedResultV0<StableEntityId> {
    Ok(derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CODEX_LOGICAL_EVENT_KIND,
        native_item_key,
        subrecord_selector: None,
    })?)
}

pub(in crate::codex::nativepath) fn codex_core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    source_root_lineage: Option<[u8; 32]>,
    owner: &CodexSessionRow,
    row: CodexCoreRecordDraft,
    event_identity_state: &mut CodexEventIdentityStateV0,
) -> CodexSourceBackedResultV0<Option<CoreRecord>> {
    let native_session_id = owner.native_session_id.as_str();
    let parent_session_id = owner
        .parent_native_session_id
        .as_deref()
        .map(|native_session_id| {
            codex_session_id_for_native_id(source_root_lineage, native_session_id)
        })
        .transpose()?;
    let root_session_id = owner
        .root_native_session_id
        .as_deref()
        .map(|native_session_id| {
            codex_session_id_for_native_id(source_root_lineage, native_session_id)
        })
        .transpose()?;
    let mut row = row;
    if row.lexical_body.is_empty() {
        return Err(CodexSourceBackedErrorV0::MissingLexicalBody);
    }
    let content_omission = (row.lexical_body.len() > ctx_history_core::MAX_CORE_CONTENT_BYTES)
        .then_some("Codex provider record content exceeds the Core content limit");
    let mut session_facts = Vec::new();
    if let Some(cwd) = row.session_cwd.as_deref().filter(|value| !value.is_empty()) {
        session_facts.push(ctx_history_core::ProviderDeclaredFact {
            kind: ctx_history_core::LiteralFactKind::SessionCwd,
            value: cwd.to_owned(),
        });
    }
    if let Some(git) = owner.git.as_ref() {
        for (kind, value) in [
            (
                ctx_history_core::LiteralFactKind::Commit,
                git.commit_hash.as_deref(),
            ),
            (
                ctx_history_core::LiteralFactKind::Branch,
                git.branch.as_deref(),
            ),
            (
                ctx_history_core::LiteralFactKind::Url,
                git.repository_url.as_deref(),
            ),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                session_facts.push(ctx_history_core::ProviderDeclaredFact {
                    kind,
                    value: value.to_owned(),
                });
            }
        }
    }
    if !session_facts.is_empty() {
        if let Some(activity) = row.activity.as_mut() {
            if activity
                .facts
                .len()
                .checked_add(session_facts.len())
                .is_none_or(|count| count > ctx_history_core::MAX_PROVIDER_DECLARED_FACTS)
            {
                activity.facts.clear();
            }
            activity.facts.splice(0..0, session_facts);
        } else {
            row.activity = Some(ctx_history_core::CoreActivity {
                revision: ctx_history_core::CORE_ACTIVITY_REVISION,
                provider_call_id: None,
                invocation: None,
                result: None,
                facts: session_facts,
            });
        }
    }
    if content_omission.is_none() {
        if !ctx_history_jsonl::selected_content_fits(
            &row.lexical_body,
            row.structured_content.as_ref(),
            row.activity.as_ref(),
            ctx_history_core::MAX_CORE_CONTENT_BYTES,
        ) {
            row.structured_content = None;
        }
        ctx_history_jsonl::fit_jsonl_activity(
            &row.lexical_body,
            row.structured_content.as_ref(),
            &mut row.activity,
            ctx_history_jsonl::JsonlActivityObservedBytes::infer_from_present(),
            ctx_history_core::MAX_CORE_CONTENT_BYTES,
        );
        if !ctx_history_jsonl::selected_content_fits(
            &row.lexical_body,
            row.structured_content.as_ref(),
            row.activity.as_ref(),
            ctx_history_core::MAX_CORE_CONTENT_BYTES,
        ) {
            return Ok(None);
        }
    }
    let (event_id, native_event_id, provider_occurrence) =
        event_identity_state.next_identity(source, session_id, &row)?;
    let CodexCoreRecordDraft {
        raw_ordinal,
        provider_event_identity,
        provider_event_copy,
        occurred_at,
        event_type,
        role,
        session_cwd: _,
        lexical_body,
        structured_content,
        discovery_exclusion,
        activity,
    } = row;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        raw_ordinal,
        event_type.as_str(),
        CODEX_PARSER_REVISION,
        content_omission.map_or(lexical_body, |_| "Codex content omitted".to_owned()),
    )?;
    record.parent_session_id = parent_session_id;
    record.root_session_id = match owner.session_relationship {
        Some(ctx_history_core::ProviderNativeSessionRelationship::Root) => Some(session_id),
        _ => root_session_id,
    };
    record.session_relationship = owner.session_relationship;
    record.event_copy = provider_event_copy
        .as_ref()
        .zip(provider_event_identity.as_ref())
        .map(|(copy, provider_identity)| {
            copied_result_event_copy(
                copy,
                provider_identity,
                event_type.as_str(),
                role.map(|role| role.as_str()),
                provider_occurrence,
                source_root_lineage,
            )
        })
        .transpose()?
        .flatten();
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
    record.role = role.map(|role| role.as_str().to_owned());
    record.agent_scope = owner.session_relationship.map(|relationship| {
        if relationship == ctx_history_core::ProviderNativeSessionRelationship::Root {
            ctx_history_core::AgentScope::Primary
        } else {
            ctx_history_core::AgentScope::Subagent
        }
    });
    if let Some(reason) = content_omission {
        record.content.policy_status = ctx_history_core::CoreContentPolicyStatus::Omitted {
            reason: reason.to_owned(),
        };
        record.content.normalized_body = None;
        record.validate_contract()?;
        return Ok(Some(record));
    }
    record.content.structured_content = structured_content;
    record.content.discovery_exclusion = discovery_exclusion;
    record.content.activity = activity;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(Some(record))
}

fn codex_session_id_for_native_id(
    source_root_lineage: Option<[u8; 32]>,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<StableEntityId> {
    let source = codex_source_key_in_root(source_root_lineage, native_session_id)?;
    codex_session_identity(&source, native_session_id)
}

fn copied_result_event_copy(
    copy: &CodexProviderNativeEventCopyV0,
    provider_identity: &CodexProviderEventIdentityV0,
    event_type: &str,
    role: Option<&str>,
    provider_occurrence: u64,
    source_root_lineage: Option<[u8; 32]>,
) -> CodexSourceBackedResultV0<Option<ctx_history_core::ProviderNativeEventCopy>> {
    if provider_identity.kind != CodexProviderEventIdentityKindV0::CallId
        || provider_identity.value != copy.result_call_id
    {
        return Ok(None);
    }
    let ancestor_source =
        codex_source_key_in_root(source_root_lineage, &copy.ancestor_native_session_id)?;
    let ancestor_session_id =
        codex_session_identity(&ancestor_source, &copy.ancestor_native_session_id)?;
    let (_, parts) = provider_event_key_parts(event_type, role, provider_identity)?;
    let (ancestor_event_id, _) = event_identity_for_occurrence(
        &ancestor_source,
        ancestor_session_id,
        &parts,
        provider_occurrence,
    )?;
    Ok(Some(ctx_history_core::ProviderNativeEventCopy {
        ancestor_session_id,
        ancestor_event_id,
        proof: ctx_history_core::ProviderNativeCopyProof::NativeCallResultIdentity,
    }))
}

#[cfg(test)]
mod tests;
