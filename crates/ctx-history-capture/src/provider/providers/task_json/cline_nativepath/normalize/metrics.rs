use super::*;

pub(in super::super) fn page_identity(
    source: &ClineFileSourceIdentity,
    revision: &ClineCertifiedRevision,
    expected: &ClinePageFrontier,
    next: &ClinePageFrontier,
    terminal: bool,
    core_fingerprint: &[u8; 32],
) -> ClineNativePageIdentity {
    let mut hasher = Sha256::new();
    hasher.update(PAGE_IDENTITY_DOMAIN);
    hash_field(&mut hasher, source.provider.as_bytes());
    hash_field(&mut hasher, source.stable_id.as_bytes());
    hash_field(&mut hasher, &revision.revision_sha256);
    hash_frontier(&mut hasher, expected);
    hash_frontier(&mut hasher, next);
    hash_field(&mut hasher, core_fingerprint);
    hasher.update([u8::from(terminal)]);
    ClineNativePageIdentity(hasher.finalize().into())
}

pub(in super::super) fn core_payload_fingerprint(
    component: ClineComponent,
    transition: ClineComponentTransition,
    session: Option<&ClineSessionRow>,
    items: &[ClineItemCheckpoint],
    rejections: &[ClineItemRejection],
    terminal: bool,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-cline-nativepath-core-payload-v1\0");
    hasher.update([component as u8, transition_tag(transition)]);
    if let Some(session) = session {
        hasher.update(session.metadata_hash);
    }
    for item in items {
        hasher.update(item.semantic_hash);
    }
    for rejection in rejections {
        hasher.update([rejection.kind as u8]);
        hasher.update(rejection.native_index.to_le_bytes());
    }
    hasher.update([u8::from(terminal)]);
    hasher.finalize().into()
}

pub(in super::super) fn estimated_event_bytes(row: &ClineEventRow) -> usize {
    // This is the exact size of the provider-owned length-prefixed page
    // encoding. Strings and byte arrays carry an eight-byte length. Optional
    // values carry a one-byte presence tag.
    encoded_str(row.identity.task.as_str())
        .saturating_add(1)
        .saturating_add(estimated_native_key_bytes(&row.identity.item))
        .saturating_add(4)
        .saturating_add(1 + 8 + 4)
        .saturating_add(1 + 1)
        .saturating_add(encoded_option_i64(row.occurred_at_millis))
        .saturating_add(encoded_option_str(row.body.as_deref()))
        .saturating_add(32)
        .saturating_add(1 + usize::from(row.source_record.is_some()) * (8 + 8 + 8 + 32))
        .saturating_add(encoded_option_str(row.preview.as_deref()))
        .saturating_add(row.tool_call.as_ref().map_or(1, |call| {
            1_usize
                .saturating_add(encoded_option_str(call.call_id.as_deref()))
                .saturating_add(encoded_option_str(call.name.as_deref()))
        }))
        .saturating_add(row.sparse_output.as_ref().map_or(1, |output| {
            1_usize
                .saturating_add(1)
                .saturating_add(encoded_option_i32(output.exit_code))
                .saturating_add(encoded_option_u64(output.duration_ms))
                .saturating_add(8)
                .saturating_add(encoded_option_str(output.preview.as_deref()))
                .saturating_add(encoded_option_str(output.call_id.as_deref()))
        }))
        .saturating_add(8)
        .saturating_add(row.file_touches.iter().fold(0_usize, |bytes, touch| {
            bytes
                .saturating_add(encoded_str(&touch.path))
                .saturating_add(encoded_option_str(touch.old_path.as_deref()))
                .saturating_add(1)
                .saturating_add(1)
                .saturating_add(encoded_str(
                    &serde_json::to_string(&touch.metadata)
                        .expect("file-touch metadata should serialize"),
                ))
        }))
}

pub(in super::super) fn estimated_session_bytes(session: &ClineSessionRow) -> usize {
    encoded_str(session.identity.as_str())
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(
            session
                .identity_aliases
                .iter()
                .map(|alias| encoded_str(alias.as_str()))
                .sum::<usize>(),
        )
        .saturating_add(encoded_option_str(session.title.as_deref()))
        .saturating_add(encoded_option_str(session.workspace_directory.as_deref()))
        .saturating_add(encoded_option_str(session.created_at.as_deref()))
        .saturating_add(encoded_option_str(session.last_modified.as_deref()))
        .saturating_add(encoded_option_str(session.model_id.as_deref()))
        .saturating_add(encoded_option_str(session.model_provider.as_deref()))
        .saturating_add(encoded_option_u64(session.tokens_input))
        .saturating_add(encoded_option_u64(session.tokens_output))
        .saturating_add(32)
}

pub(in super::super) fn estimated_metadata_checkpoint_bytes(
    checkpoint: &ClineMetadataCheckpoint,
) -> usize {
    estimated_observation_bytes(&checkpoint.observation)
        .saturating_add(1 + usize::from(checkpoint.content_sha256.is_some()) * 32)
        .saturating_add(estimated_session_bytes(&checkpoint.session))
}

pub(in super::super) fn estimated_rejection_bytes(rejection: &ClineItemRejection) -> usize {
    1_usize
        .saturating_add(8)
        .saturating_add(encoded_option_str(rejection.native_id.as_deref()))
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(encoded_str(&rejection.detail))
}

pub(in super::super) fn estimated_output_bytes(output: &ProOutputObservation) -> usize {
    let coordinate = encoded_str(&output.coordinate.unit_key)
        .saturating_add(8)
        .saturating_add(encoded_option_str(
            output.coordinate.native_record_id.as_deref(),
        ))
        .saturating_add(encoded_option_u64(output.coordinate.source_record_ordinal))
        .saturating_add(encoded_option_u32(
            output.coordinate.source_record_subrecord_index,
        ))
        .saturating_add(encoded_option_u64(output.coordinate.byte_start))
        .saturating_add(encoded_option_u64(output.coordinate.byte_end_exclusive));
    let associations = encoded_str(&output.associations.direct_session_id)
        .saturating_add(encoded_str(&output.associations.root_session_id))
        .saturating_add(encoded_option_str(
            output.associations.parent_session_id.as_deref(),
        ))
        .saturating_add(encoded_option_str(
            output.associations.provider_session_id.as_deref(),
        ))
        .saturating_add(encoded_option_str(output.associations.agent_id.as_deref()))
        .saturating_add(
            output
                .associations
                .repository
                .as_ref()
                .map_or(1, |repository| {
                    1_usize
                        .saturating_add(encoded_str(&repository.repository_id))
                        .saturating_add(encoded_option_str(repository.checkout_id.as_deref()))
                        .saturating_add(encoded_option_str(repository.worktree_id.as_deref()))
                        .saturating_add(encoded_option_str(repository.object_format.as_deref()))
                }),
        );
    let command = output.command.as_ref().map_or(1, |command| {
        1_usize
            .saturating_add(encoded_str(&command.tool_name))
            .saturating_add(encoded_str(&command.command))
            .saturating_add(encoded_option_str(command.working_directory.as_deref()))
    });
    1_usize
        .saturating_add(coordinate)
        .saturating_add(encoded_option_i64(output.occurred_at_unix_ms))
        .saturating_add(associations)
        .saturating_add(encoded_option_str(output.call_id.as_deref()))
        .saturating_add(command)
        .saturating_add(1)
        .saturating_add(encoded_option_i32(output.outcome.exit_code))
        .saturating_add(encoded_option_u64(output.outcome.duration_ms))
        .saturating_add(4)
        .saturating_add(encoded_str(&output.locator.kind))
        .saturating_add(encoded_bytes(&output.locator.payload))
        .saturating_add(encoded_bytes(&output.content))
}

pub(in super::super) fn estimated_observation_bytes(
    observation: &ClineComponentObservation,
) -> usize {
    1_usize
        .saturating_add(encoded_bytes(
            observation.path.as_os_str().as_encoded_bytes(),
        ))
        .saturating_add(1)
        .saturating_add(match &observation.state {
            super::super::source::ClineObservedFileState::Missing => 0,
            super::super::source::ClineObservedFileState::Present(stamp) => {
                8_usize.saturating_add(encoded_str(&stamp.token()))
            }
            super::super::source::ClineObservedFileState::Unavailable(message) => {
                encoded_str(message)
            }
        })
}

pub(in super::super) fn estimated_source_bytes(source: &ClineFileSourceIdentity) -> usize {
    encoded_str(source.provider)
        .saturating_add(encoded_str(source.task.as_str()))
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(
            source
                .task_aliases
                .iter()
                .map(|alias| encoded_str(alias.as_str()))
                .sum::<usize>(),
        )
        .saturating_add(1)
        .saturating_add(encoded_bytes(
            source.canonical_path.as_os_str().as_encoded_bytes(),
        ))
        .saturating_add(encoded_str(&source.stable_id))
        .saturating_add(8)
}

pub(in super::super) fn estimated_revision_bytes(revision: &ClineCertifiedRevision) -> usize {
    32_usize.saturating_add(encoded_str(&revision.observed_stamp_token))
}

pub(in super::super) fn estimated_frontier_bytes(_frontier: &ClinePageFrontier) -> usize {
    4 + 8 + 32
}

fn estimated_native_key_bytes(key: &ClineNativeItemKey) -> usize {
    match key {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence: _,
        } => 1_usize
            .saturating_add(encoded_str(native_id))
            .saturating_add(8),
        ClineNativeItemKey::ComponentOrdinal(_) => 1 + 8,
    }
}

fn encoded_str(value: &str) -> usize {
    encoded_bytes(value.as_bytes())
}

fn encoded_bytes(value: &[u8]) -> usize {
    8_usize.saturating_add(value.len())
}

fn encoded_option_str(value: Option<&str>) -> usize {
    1_usize.saturating_add(value.map_or(0, encoded_str))
}

fn encoded_option_i32(value: Option<i32>) -> usize {
    1 + usize::from(value.is_some()) * 4
}

fn encoded_option_u32(value: Option<u32>) -> usize {
    1 + usize::from(value.is_some()) * 4
}

fn encoded_option_i64(value: Option<i64>) -> usize {
    1 + usize::from(value.is_some()) * 8
}

fn encoded_option_u64(value: Option<u64>) -> usize {
    1 + usize::from(value.is_some()) * 8
}

fn transition_tag(transition: ClineComponentTransition) -> u8 {
    match transition {
        ClineComponentTransition::Cold => 0,
        ClineComponentTransition::Unchanged => 1,
        ClineComponentTransition::Append { .. } => 2,
        ClineComponentTransition::Rewrite => 3,
        ClineComponentTransition::ControlOnlyRewrite => 5,
        ClineComponentTransition::LogicalEmpty => 6,
        ClineComponentTransition::MissingPhysical => 7,
    }
}

pub(super) fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_frontier(hasher: &mut Sha256, frontier: &ClinePageFrontier) {
    hasher.update(frontier.version.to_le_bytes());
    hasher.update(frontier.next_native_index.to_le_bytes());
    hasher.update(frontier.prefix_semantic_sha256);
}

pub(super) fn hash_native_key(hasher: &mut Sha256, key: &ClineNativeItemKey) {
    match key {
        ClineNativeItemKey::NativeId {
            native_id,
            occurrence,
        } => {
            hasher.update(b"id\0");
            hasher.update(native_id.as_bytes());
            hasher.update(occurrence.to_le_bytes());
        }
        ClineNativeItemKey::ComponentOrdinal(ordinal) => {
            hasher.update(b"ordinal\0");
            hasher.update(ordinal.to_le_bytes());
        }
    }
    hasher.update(b"\0");
}
