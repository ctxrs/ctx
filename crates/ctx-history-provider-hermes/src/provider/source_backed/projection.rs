use super::*;

pub(super) fn hermes_schema_evidence(
    sqlite_user_version: i64,
    schema_fingerprint: &str,
) -> Vec<u8> {
    format!(
        "hermes-logical-schema-v1:capture={HERMES_CAPTURE_REVISION};\
         policy={HERMES_POLICY_REVISION};user_version={sqlite_user_version};\
         schema={schema_fingerprint}",
    )
    .into_bytes()
}

pub(super) fn hermes_logical_progress(
    stage: SourceBackedCurrentSourceProgressStage,
    rows_scanned: u64,
    certified_bytes: u64,
) -> SourceBackedCurrentSourceProgress {
    let mut progress = SourceBackedCurrentSourceProgress::new(stage);
    progress.logical_rows_scanned = Some(rows_scanned);
    progress.logical_certified_bytes = Some(certified_bytes);
    progress
}

pub(super) fn hermes_tree_fingerprint<L: CaptureLifecycleSink>(
    profile_source: &SourceKey,
    schema_evidence: &[u8],
    leaves: &[ObservedDocumentLeaf<HermesSessionLeaf<L>>],
) -> [u8; 32]
where
    L::PinnedAppendBase: Clone,
{
    let mut digest = Sha256::new();
    digest.update(HERMES_TREE_FINGERPRINT_DOMAIN);
    digest.update(profile_source.exact_descriptor_digest());
    hash_bytes(&mut digest, schema_evidence);
    digest.update((leaves.len() as u64).to_be_bytes());
    for leaf in leaves {
        digest.update(leaf.fingerprint.as_bytes());
    }
    digest.finalize().into()
}

#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    static HERMES_LOGICAL_ROW_TRAVERSALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static HERMES_INVENTORY_ROWS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static HERMES_SESSION_SCAN_RECEIPTS: std::cell::RefCell<BTreeMap<String, (u64, u64)>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn reset_logical_row_traversals() {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(|count| count.set(0));
    HERMES_INVENTORY_ROWS.with(|count| count.set(0));
    HERMES_SESSION_SCAN_RECEIPTS.with(|receipts| receipts.borrow_mut().clear());
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn logical_row_traversals() -> u64 {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn inventory_observation_rows() -> u64 {
    HERMES_INVENTORY_ROWS.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn session_scan_receipts() -> BTreeMap<String, (u64, u64)> {
    HERMES_SESSION_SCAN_RECEIPTS.with(|receipts| receipts.borrow().clone())
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_logical_row_traversal() {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(|count| {
        count.set(count.get().saturating_add(1));
    });
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_inventory_rows(rows: u64) {
    HERMES_INVENTORY_ROWS.with(|count| count.set(count.get().saturating_add(rows)));
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_inventory_rows(_rows: u64) {}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn record_session_scan_receipt(
    provider_session_id: &str,
    decoded_rows: u64,
    hydration_queries: u64,
) {
    HERMES_SESSION_SCAN_RECEIPTS.with(|receipts| {
        receipts.borrow_mut().insert(
            provider_session_id.to_owned(),
            (decoded_rows, hydration_queries),
        );
    });
}

#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn record_session_scan_receipt(
    _provider_session_id: &str,
    _decoded_rows: u64,
    _hydration_queries: u64,
) {
}

pub(super) fn project_native_row(
    source: &SourceKey,
    source_path: &str,
    native: HermesNativeRow,
    session_context: Option<&HermesSessionContext>,
    context_rejection: Option<&str>,
) -> HermesSourceBackedResult<HermesSourceBackedRecord> {
    let ordinal = native.ordinal;
    match native.record {
        HermesNativeRecord::Session(row) => {
            if let Some(reason) = context_rejection {
                return Ok(rejected(reason.to_owned()));
            }
            let Some(context) = session_context else {
                return Ok(rejected(format!(
                    "Hermes session {} disappeared during projection",
                    row.id
                )));
            };
            match project_session(source_path, row, context) {
                Ok(session) => Ok(HermesSourceBackedRecord::Session(session)),
                Err(error) => Ok(rejected(error.to_string())),
            }
        }
        HermesNativeRecord::Message { row, values: _ } => {
            if let Some(reason) = context_rejection {
                return Ok(rejected(reason.to_owned()));
            }
            let Some(context) = session_context else {
                return Ok(rejected(format!(
                    "Hermes message {} depends on missing session {}",
                    row.id, row.session_id
                )));
            };
            match project_message(source, ordinal, row, context) {
                Ok(document) => Ok(HermesSourceBackedRecord::Event(document)),
                Err(error) => Ok(rejected(error.to_string())),
            }
        }
        HermesNativeRecord::Rejected(reason) => Ok(rejected(reason)),
    }
}

pub(super) fn rejected(reason: String) -> HermesSourceBackedRecord {
    HermesSourceBackedRecord::Rejected(HermesSourceBackedRejection { reason })
}

pub(super) fn project_session(
    source_path: &str,
    row: HermesSessionRow,
    context: &HermesSessionContext,
) -> HermesSourceBackedResult<HermesSourceBackedSession> {
    Ok(HermesSourceBackedSession {
        provider_session_id: row.id,
        provider_parent_session_id: row.parent_session_id,
        branch: context.branch.clone(),
        source_path: source_path.to_owned(),
        workspace: context.workspace.clone(),
        cwd: context.cwd.clone(),
    })
}

pub(super) fn project_message(
    source: &SourceKey,
    ordinal: u64,
    row: HermesMessageRow,
    session: &HermesSessionContext,
) -> HermesSourceBackedResult<CoreRecord> {
    let native = hermes_native_event(&row, ordinal)?;
    let _provider_owned_evidence = (&native.cursor, &native.payload, &native.metadata);
    let activity = hermes_activity(&row, &native);
    let body = native.complete_text;
    let native_item_key = NativeItemKey::composite(
        HERMES_MESSAGE_NAMESPACE,
        vec![TypedKey::utf8(&row.session_id)?, TypedKey::I64(row.id)],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: HERMES_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(&row.session_id)?,
        TypedKey::I64(row.id),
    ])?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        source.clone(),
        native.provider_event_index,
        native.event_type.as_str(),
        HERMES_SOURCE_PARSER_REVISION,
        body,
    )?;
    record.agent_scope = Some(session.agent_scope);
    if let Some(parent_session_id) = session.parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    }
    record.provider_session_id = Some(row.session_id);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(native.occurred_at.timestamp_millis());
    record.role = native.role.map(|role| role.as_str().to_owned());
    record.content.structured_content = native.payload.get("body").cloned();
    record.content.activity = merge_hermes_facts(activity, session);
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn hermes_activity(
    row: &HermesMessageRow,
    native: &HermesNativeEvent,
) -> Option<CoreActivity> {
    let provider_call_id = admit_optional_provider_call_id(row.tool_call_id.clone());
    let invocation = if native.event_type == ctx_history_core::EventType::ToolCall
        && provider_call_id.is_some()
    {
        admit_optional_metadata_text(row.tool_name.clone()).map(|tool| ActivityInvocation {
            protocol: None,
            server: None,
            tool,
            arguments: row
                .tool_calls
                .as_deref()
                .map(ctx_history_capture_model::normalization::provider_json_text)
                .map(|value| ActivityJsonCapture::Present { value })
                .unwrap_or(ActivityJsonCapture::Unavailable),
            started_at_unix_ms: Some(native.occurred_at.timestamp_millis()),
        })
    } else {
        None
    };
    let result = (native.event_type == ctx_history_core::EventType::ToolOutput
        && provider_call_id.is_some())
    .then(|| ActivityResult {
        status: admit_optional_metadata_text(row.finish_reason.clone()),
        completed_at_unix_ms: Some(native.occurred_at.timestamp_millis()),
        duration_ns: None,
        text: ActivityTextCapture::NormalizedBody,
        structured_content: ActivityJsonCapture::Present {
            value: super::super::hermes_decode_content(row.content.as_deref()),
        },
    });
    if invocation.is_none() && result.is_none() {
        return None;
    }
    Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts: Vec::new(),
    })
}

pub(super) fn merge_hermes_facts(
    activity: Option<CoreActivity>,
    session: &HermesSessionContext,
) -> Option<CoreActivity> {
    let mut facts = Vec::new();
    for (kind, value) in [
        (LiteralFactKind::Branch, session.branch.clone()),
        (LiteralFactKind::Workspace, session.workspace.clone()),
        (LiteralFactKind::SessionCwd, session.cwd.clone()),
    ] {
        if let Some(fact) =
            value.and_then(|value| admit_provider_declared_fact(kind, value, facts.len()))
        {
            facts.push(fact);
        }
    }
    match (activity, facts.is_empty()) {
        (Some(mut activity), _) => {
            activity.facts = facts;
            Some(activity)
        }
        (None, false) => Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts,
        }),
        (None, true) => None,
    }
}

pub(super) fn bound_projected_record(
    record: HermesSourceBackedRecord,
) -> HermesSourceBackedResult<(HermesSourceBackedRecord, usize)> {
    let owned_bytes = projected_owned_bytes(&record)?;
    if owned_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Ok((record, owned_bytes));
    }
    let record = rejected(format!(
        "Hermes projected row requires {owned_bytes} bytes and exceeds the {}-byte page limit",
        NATIVE_INGESTION_PAGE_MAX_BYTES
    ));
    let owned_bytes = projected_owned_bytes(&record)?;
    Ok((record, owned_bytes))
}

pub(super) fn projected_owned_bytes(
    record: &HermesSourceBackedRecord,
) -> Result<usize, serde_json::Error> {
    let fixed = 1024_usize;
    match record {
        HermesSourceBackedRecord::Session(session) => Ok(fixed
            .saturating_add(session.provider_session_id.len())
            .saturating_add(
                session
                    .provider_parent_session_id
                    .as_deref()
                    .map(str::len)
                    .unwrap_or(0),
            )
            .saturating_add(session.branch.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.source_path.len())
            .saturating_add(session.workspace.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.cwd.as_deref().map(str::len).unwrap_or(0))),
        HermesSourceBackedRecord::Event(event) => {
            Ok(fixed.saturating_add(serde_json::to_vec(event)?.len()))
        }
        HermesSourceBackedRecord::Rejected(rejection) => {
            Ok(fixed.saturating_add(rejection.reason.len()))
        }
    }
}

pub(super) fn native_record_digest(native: &HermesNativeRow) -> HermesSourceBackedResult<[u8; 32]> {
    match &native.record {
        HermesNativeRecord::Session(row) => Ok(session_record_digest(row)),
        HermesNativeRecord::Message { values, .. } => {
            decode_sha256(hermes_layout_record_digest(values).as_str())
        }
        HermesNativeRecord::Rejected(reason) => {
            let mut digest = Sha256::new();
            digest.update(HERMES_REJECTION_DIGEST_DOMAIN);
            digest.update(reason.as_bytes());
            Ok(digest.finalize().into())
        }
    }
}

pub(super) fn session_record_digest(row: &HermesSessionRow) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HERMES_SESSION_DIGEST_DOMAIN);
    hash_text(&mut digest, &row.id);
    hash_text(&mut digest, &row.source);
    hash_optional_text(&mut digest, row.parent_session_id.as_deref());
    hash_optional_text(&mut digest, row.model.as_deref());
    hash_optional_text(&mut digest, row.model_config.as_deref());
    digest.update(row.started_at.to_bits().to_be_bytes());
    hash_optional_f64(&mut digest, row.ended_at);
    hash_optional_text(&mut digest, row.end_reason.as_deref());
    digest.update(row.message_count.to_be_bytes());
    digest.update(row.tool_call_count.to_be_bytes());
    digest.update(row.input_tokens.to_be_bytes());
    digest.update(row.output_tokens.to_be_bytes());
    digest.update(row.cache_read_tokens.to_be_bytes());
    digest.update(row.cache_write_tokens.to_be_bytes());
    digest.update(row.reasoning_tokens.to_be_bytes());
    hash_optional_text(&mut digest, row.cwd.as_deref());
    hash_optional_text(&mut digest, row.git_branch.as_deref());
    hash_optional_text(&mut digest, row.git_repo_root.as_deref());
    hash_optional_text(&mut digest, row.billing_provider.as_deref());
    hash_optional_text(&mut digest, row.billing_base_url.as_deref());
    hash_optional_text(&mut digest, row.billing_mode.as_deref());
    hash_optional_f64(&mut digest, row.estimated_cost_usd);
    hash_optional_f64(&mut digest, row.actual_cost_usd);
    hash_optional_text(&mut digest, row.title.as_deref());
    digest.update(row.archived.to_be_bytes());
    digest.finalize().into()
}

pub(super) fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

pub(super) fn hash_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

pub(super) fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

pub(super) fn hash_optional_f64(digest: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

pub(super) fn decode_sha256(value: &str) -> HermesSourceBackedResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(HermesSourceBackedError::InvalidLogicalDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

pub(super) fn decode_hex_nibble(value: u8) -> HermesSourceBackedResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(HermesSourceBackedError::InvalidLogicalDigest),
    }
}
