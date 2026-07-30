use std::collections::BTreeMap;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind,
};

use crate::provider::source_backed::hydration_failure;

use super::*;

pub(super) fn hydrate_codebuddy_group(
    root: &Path,
    request: &BatchHydrationRequest,
) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
    let expected_source = request
        .events()
        .first()
        .map(|event| event.locator().source().clone())
        .ok_or_else(|| invalid_locator("CodeBuddy hydration group is empty"))?;
    if request.events().iter().any(|event| {
        event.locator().validate_contract().is_err()
            || !event
                .locator()
                .source()
                .exact_descriptor_eq(&expected_source)
    }) {
        return Err(invalid_locator(
            "CodeBuddy hydration group has invalid or mixed-source locators",
        ));
    }

    let inventory = discover_codebuddy_tree(root).map_err(temporarily_unavailable)?;
    if inventory.status == CodeBuddyInventoryStatus::Unavailable {
        return Err(temporarily_unavailable(
            "CodeBuddy selected route is temporarily unavailable",
        ));
    }
    let tree = inventory
        .into_complete_tree()
        .ok_or_else(|| temporarily_unavailable("complete CodeBuddy inventory lost its tree"))?;
    let mut matches = tree.leaves.iter().filter(|leaf| {
        leaf.provider_leaf
            .source
            .exact_descriptor_eq(&expected_source)
    });
    let leaf = matches
        .next()
        .ok_or_else(|| missing_record("the exact CodeBuddy source is absent"))?;
    if matches.next().is_some() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "more than one CodeBuddy leaf owns the exact source",
        ));
    }
    let source =
        open_codebuddy_source(&tree.authority, &leaf.provider_leaf).map_err(stale_evidence)?;
    let state = initial_state(
        &source,
        &ProviderAdapterContext {
            machine_id: "source-backed-codebuddy-hydration".to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: Some(root.to_path_buf()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
    )
    .map_err(stale_evidence)?;
    let source_key = codebuddy_source_key(&source, &state.session).map_err(invalid_locator)?;
    if !source_key.exact_descriptor_eq(&expected_source) {
        return Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "CodeBuddy source identity changed while opening its group",
        ));
    }

    let records = match source.shape {
        CodeBuddySourceShape::Cli => hydrate_cli_group(&source, &state.session, request)?,
        CodeBuddySourceShape::Extension => {
            hydrate_extension_group(&source, &state.session, request)?
        }
    };
    let terminal = revalidate_codebuddy_tree(&tree).map_err(temporarily_unavailable)?;
    if terminal != tree.tree_fingerprint {
        return Err(temporarily_unavailable(
            "CodeBuddy tree changed during grouped hydration",
        ));
    }
    let result = BatchHydrationResult::new(records).map_err(invalid_locator)?;
    result.validate_for_request(request)?;
    Ok(result)
}

fn hydrate_cli_group(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
    request: &BatchHydrationRequest,
) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
    let primary = source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
        .ok_or_else(|| stale_evidence("CodeBuddy CLI source lost its opened file"))?;
    note_body_read();
    let mut file = primary
        .file()
        .try_clone()
        .map_err(temporarily_unavailable)?;
    let source_len = primary.metadata().len();
    let mut records = Vec::with_capacity(request.events().len());
    for event in request.events() {
        let locator = event.locator();
        if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
            || locator.certified_source_revision_digest().is_some()
        {
            return Err(invalid_locator("CLI locator has the wrong revision policy"));
        }
        let NativeRecordCoordinate::Jsonl {
            byte_offset,
            byte_length,
            physical_ordinal,
            native_session_key,
            native_event_key,
        } = locator.coordinate()
        else {
            return Err(invalid_locator("CLI locator is not a JSONL byte range"));
        };
        if *byte_length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64
            || byte_offset
                .checked_add(*byte_length)
                .is_none_or(|end| end > source_len)
        {
            return Err(stale_evidence("CLI locator range is no longer valid"));
        }
        file.seek(SeekFrom::Start(*byte_offset))
            .map_err(temporarily_unavailable)?;
        let mut bytes = vec![
            0_u8;
            usize::try_from(*byte_length).map_err(|_| invalid_locator(
                "CLI locator range exceeds platform limits"
            ))?
        ];
        file.read_exact(&mut bytes)
            .map_err(temporarily_unavailable)?;
        let payload = jsonl_payload(&bytes);
        if Sha256::digest(payload).as_slice() != locator.record_digest() {
            return Err(stale_evidence(
                "CLI locator digest no longer matches provider bytes",
            ));
        }
        let value: Value = serde_json::from_slice(payload).map_err(stale_evidence)?;
        let physical_line = usize::try_from(*physical_ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| invalid_locator("CLI physical line exceeds platform limits"))?;
        let (text, native_message_id) =
            codebuddy_cli_complete_content_record(&value, physical_line)
                .ok_or_else(|| missing_record("CLI locator no longer resolves to a message"))?;
        let expected_session = session_key_utf8(native_session_key.as_ref())
            .ok_or_else(|| invalid_locator("CLI locator has an invalid native session key"))?;
        let observed_session = value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| format!("{}/{}", session.project_hash, value))
            .unwrap_or_else(|| session.provider_session_id());
        if expected_session != observed_session
            || !tagged_event_key_matches(
                native_event_key.as_ref(),
                CODEBUDDY_CLI_LOCATOR_TAG,
                &native_message_id,
            )
        {
            return Err(stale_evidence("CLI locator native identity changed"));
        }
        records.push(HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes: text.into_bytes(),
        });
    }
    primary.revalidate().map_err(stale_evidence)?;
    Ok(records)
}

#[derive(Debug)]
struct CachedMessage {
    bytes: Vec<u8>,
    text: String,
}

fn hydrate_extension_group(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
    request: &BatchHydrationRequest,
) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
    let expected_revision = source_revision_digest(source);
    let mut cache = BTreeMap::<String, CachedMessage>::new();
    let mut records = Vec::with_capacity(request.events().len());
    for event in request.events() {
        let locator = event.locator();
        if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
            || locator.certified_source_revision_digest() != Some(&expected_revision)
        {
            return Err(stale_evidence(
                "structured locator source revision is stale",
            ));
        }
        let (relative_path, _ordinal, native_record_id) =
            structured_coordinate(locator.coordinate()).map_err(invalid_locator)?;
        let message_id = relative_path
            .strip_prefix("messages/")
            .and_then(|value| value.strip_suffix(".json"))
            .filter(|value| provider_safe_path_segment(value))
            .ok_or_else(|| invalid_locator("structured locator message path is invalid"))?;
        if native_record_id != format!("{}:{message_id}", session.provider_session_id()) {
            return Err(stale_evidence("structured locator native identity changed"));
        }
        if !cache.contains_key(message_id) {
            let capability = source
                .capability
                .as_ref()
                .ok_or_else(|| stale_evidence("extension source lost its authority"))?;
            let expected = capability
                .extension
                .as_ref()
                .and_then(|extension| extension.messages.get(message_id))
                .ok_or_else(|| missing_record("structured message is absent"))?;
            let bytes = read_observed_file(
                &capability.authority,
                expected,
                CODEBUDDY_NATIVE_RECORD_MAX_BYTES,
            )
            .map_err(stale_evidence)?;
            let raw: Value = serde_json::from_slice(&bytes).map_err(stale_evidence)?;
            let decoded = codebuddy_decoded_message(&raw);
            let text = codebuddy_message_text(&decoded, &raw);
            if text.trim().is_empty() {
                return Err(missing_record(
                    "structured locator no longer resolves to displayable text",
                ));
            }
            cache.insert(message_id.to_owned(), CachedMessage { bytes, text });
        }
        let cached = cache
            .get(message_id)
            .ok_or_else(|| temporarily_unavailable("CodeBuddy message cache lost a record"))?;
        if Sha256::digest(&cached.bytes).as_slice() != locator.record_digest() {
            return Err(stale_evidence(
                "structured locator digest no longer matches provider bytes",
            ));
        }
        records.push(HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes: cached.text.as_bytes().to_vec(),
        });
    }
    Ok(records)
}

pub(super) fn structured_coordinate(
    coordinate: &NativeRecordCoordinate,
) -> Result<(String, u64, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(invalid_source_backed(
            "structured locator is not a tree record",
        ));
    };
    let TypedKey::Utf8(relative_path) = relative_file_key else {
        return Err(invalid_source_backed(
            "structured locator relative path is not UTF-8",
        ));
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(invalid_source_backed(
            "structured locator coordinate is not tagged",
        ));
    };
    match parts.as_slice() {
        [TypedKey::Utf8(tag), TypedKey::U64(ordinal), TypedKey::Utf8(native_id)]
            if tag == CODEBUDDY_EXTENSION_LOCATOR_TAG =>
        {
            Ok((relative_path.clone(), *ordinal, native_id.clone()))
        }
        _ => Err(invalid_source_backed(
            "structured locator coordinate has the wrong format tag",
        )),
    }
}

pub(super) fn tagged_event_key_matches(key: Option<&TypedKey>, tag: &str, native_id: &str) -> bool {
    matches!(
        key,
        Some(TypedKey::Composite(parts))
            if matches!(
                parts.as_slice(),
                [TypedKey::Utf8(actual_tag), TypedKey::Utf8(actual_id)]
                    if actual_tag == tag && actual_id == native_id
            )
    )
}

fn session_key_utf8(key: Option<&TypedKey>) -> Option<&str> {
    match key {
        Some(TypedKey::Utf8(value)) => Some(value),
        _ => None,
    }
}

fn jsonl_payload(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn invalid_locator(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, detail)
}

fn missing_record(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::MissingRecord, detail)
}

fn stale_evidence(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleRecordEvidence, detail)
}

fn temporarily_unavailable(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, detail)
}
