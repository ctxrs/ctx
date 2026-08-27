use ctx_history_core::{
    derive_event_id, derive_native_session_id, CoreContentPolicyStatus, CoreRecord,
    EventIdentityInput, EventRole, EventType, NativeItemKey, SourceKey, SubrecordSelector,
    TypedKey, MAX_CORE_CONTENT_BYTES,
};
use serde_json::Value;

use crate::{
    CanonicalState, FxProviderError, FxProviderResult, HistoryTurn, HistoryTurnKind, LogicalTurn,
};

pub const FX_PARSER_REVISION: &str = "fx-event-log-v3/v0.0.6-79666393+main-385f74e0";
const FX_SESSION_KIND: &str = "fx_session";
const FX_SESSION_NAMESPACE: &str = "fx.session_id";
const FX_TURN_ITEM_KIND: &str = "fx_history_turn";
const FX_TURN_NAMESPACE: &str = "fx.absolute_history_ordinal";
const FX_TURN_PART_NAMESPACE: &str = "fx.history_turn_part";
const FX_SUMMARY_ITEM_KIND: &str = "fx_compacted_summary";
const FX_SUMMARY_NAMESPACE: &str = "fx.compacted_summary_range";
const OVERSIZED_CONTENT_OMISSION_REASON: &str =
    "fx searchable content exceeds the Core content limit";

#[derive(Debug, Clone, Copy)]
pub struct ProjectionBinding<'a> {
    pub source: &'a SourceKey,
    pub native_session_id: &'a str,
}

pub fn project_canonical_state(
    binding: ProjectionBinding<'_>,
    state: &CanonicalState,
) -> FxProviderResult<Vec<CoreRecord>> {
    validate_binding(binding, &state.id)?;
    let mut records = Vec::with_capacity(state.history.len().saturating_mul(2));
    if let Some(summary_turn) = state
        .history
        .first()
        .filter(|turn| turn.kind() == HistoryTurnKind::CompactedSummary)
    {
        let summary = summary_turn
            .compacted_summary()
            .ok_or(FxProviderError::InvalidState("summary shape is missing"))?;
        records.push(project_summary(
            binding,
            summary.removed_turn_count,
            summary_turn,
        )?);
    }
    let logical = state.logical_turns()?;
    records.extend(project_logical_turns(binding, &logical)?);
    Ok(records)
}

pub fn project_logical_turns(
    binding: ProjectionBinding<'_>,
    turns: &[LogicalTurn],
) -> FxProviderResult<Vec<CoreRecord>> {
    validate_binding(binding, binding.native_session_id)?;
    let mut records = Vec::with_capacity(turns.len().saturating_mul(2));
    for turn in turns {
        if turn.turn.kind() == HistoryTurnKind::CompactedSummary {
            return Err(FxProviderError::InvalidState(
                "a logical turn cannot be a compacted summary",
            ));
        }
        records.extend(project_turn(binding, turn)?);
    }
    Ok(records)
}

pub(crate) fn project_turn(
    binding: ProjectionBinding<'_>,
    logical: &LogicalTurn,
) -> FxProviderResult<Vec<CoreRecord>> {
    project_history_turn(binding, logical.absolute_ordinal, &logical.turn)
}

pub(crate) fn project_history_turn(
    binding: ProjectionBinding<'_>,
    absolute_ordinal: u64,
    turn: &HistoryTurn,
) -> FxProviderResult<Vec<CoreRecord>> {
    let session_id = core_session_id(binding)?;
    let native_item_key = NativeItemKey::composite(
        FX_TURN_NAMESPACE,
        vec![
            TypedKey::utf8(binding.native_session_id)?,
            TypedKey::U64(absolute_ordinal),
        ],
    )?;
    let structured = turn.structured_value()?;
    let user = required_field(&structured, "user")?;
    let user_body = searchable_part_body(user_turn_label(turn)?, required_field(user, "text")?)?;
    let user_structured = match assistant_value(turn, &structured)? {
        Some(_) => user.clone(),
        None => structured.clone(),
    };
    let mut records = Vec::with_capacity(2);
    records.push(project_message_part(
        binding,
        session_id,
        &native_item_key,
        absolute_ordinal,
        EventRole::User,
        user_body,
        user_structured,
    )?);
    if let Some(assistant) = assistant_value(turn, &structured)? {
        let body = searchable_part_body("assistant turn", assistant)?;
        records.push(project_message_part(
            binding,
            session_id,
            &native_item_key,
            absolute_ordinal,
            EventRole::Assistant,
            body,
            structured,
        )?);
    }
    Ok(records)
}

#[allow(clippy::too_many_arguments)]
fn project_message_part(
    binding: ProjectionBinding<'_>,
    session_id: ctx_history_core::StableEntityId,
    native_item_key: &NativeItemKey,
    absolute_ordinal: u64,
    role: EventRole,
    body: Option<String>,
    structured: Value,
) -> FxProviderResult<CoreRecord> {
    let selector =
        SubrecordSelector::native_id(FX_TURN_PART_NAMESPACE, TypedKey::utf8(role.as_str())?)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: binding.source,
        session_id,
        logical_item_kind: FX_TURN_ITEM_KIND,
        native_item_key,
        subrecord_selector: Some(&selector),
    })?;
    let part_ordinal = match role {
        EventRole::User => 0,
        EventRole::Assistant => 1,
        _ => {
            return Err(FxProviderError::InvalidState(
                "fx turn projection received an unsupported message role",
            ));
        }
    };
    let event_sequence = absolute_ordinal
        .checked_mul(2)
        .and_then(|sequence| sequence.checked_add(part_ordinal))
        .ok_or(FxProviderError::InvalidState(
            "fx projected event sequence overflowed",
        ))?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        binding.source.clone(),
        event_sequence,
        EventType::Message.as_str(),
        FX_PARSER_REVISION,
        body.as_deref().unwrap_or("oversized fx message"),
    )?;
    record.provider_session_id = Some(binding.native_session_id.to_owned());
    record.role = Some(role.as_str().to_owned());
    fit_content(&mut record, body, structured)?;
    record.validate_contract()?;
    Ok(record)
}

pub(crate) fn project_summary(
    binding: ProjectionBinding<'_>,
    removed_turn_count: u64,
    turn: &HistoryTurn,
) -> FxProviderResult<CoreRecord> {
    let session_id = core_session_id(binding)?;
    let native_item_key = NativeItemKey::composite(
        FX_SUMMARY_NAMESPACE,
        vec![
            TypedKey::utf8(binding.native_session_id)?,
            TypedKey::U64(0),
            TypedKey::U64(removed_turn_count),
        ],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: binding.source,
        session_id,
        logical_item_kind: FX_SUMMARY_ITEM_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let structured = turn.structured_value()?;
    let body = searchable_body(turn, &structured)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        binding.source.clone(),
        removed_turn_count
            .checked_mul(2)
            .and_then(|sequence| sequence.checked_sub(1))
            .ok_or(FxProviderError::InvalidState(
                "fx summary event sequence overflowed",
            ))?,
        EventType::Summary.as_str(),
        FX_PARSER_REVISION,
        body.as_deref().unwrap_or("oversized fx summary"),
    )?;
    record.provider_session_id = Some(binding.native_session_id.to_owned());
    fit_content(&mut record, body, structured)?;
    record.validate_contract()?;
    Ok(record)
}

fn core_session_id(
    binding: ProjectionBinding<'_>,
) -> FxProviderResult<ctx_history_core::StableEntityId> {
    Ok(derive_native_session_id(
        binding.source,
        FX_SESSION_KIND,
        FX_SESSION_NAMESPACE,
        TypedKey::utf8(binding.native_session_id)?,
    )?)
}

fn validate_binding(binding: ProjectionBinding<'_>, state_id: &str) -> FxProviderResult<()> {
    if binding.source.provider() != "fx" {
        return Err(FxProviderError::InvalidState(
            "projection source provider must be fx",
        ));
    }
    if binding.native_session_id != state_id {
        return Err(FxProviderError::InvalidState(
            "projection session binding does not match canonical state",
        ));
    }
    Ok(())
}

fn fit_content(
    record: &mut CoreRecord,
    body: Option<String>,
    structured: Value,
) -> FxProviderResult<()> {
    let Some(body) = body else {
        record.content.policy_status = CoreContentPolicyStatus::Omitted {
            reason: OVERSIZED_CONTENT_OMISSION_REASON.to_owned(),
        };
        record.content.normalized_body = None;
        record.content.structured_content = None;
        return Ok(());
    };
    record.content.normalized_body = Some(body);
    record.content.structured_content = Some(structured);
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    Ok(())
}

fn searchable_body(turn: &HistoryTurn, structured: &Value) -> FxProviderResult<Option<String>> {
    let kind = match turn.kind() {
        HistoryTurnKind::CompactedSummary => "compacted summary",
        HistoryTurnKind::Assistant => "assistant turn",
        HistoryTurnKind::BackgroundCommand => "background command turn",
        HistoryTurnKind::Interrupted => "interrupted turn",
    };
    let mut builder = SearchBody::default();
    builder.push(kind)?;
    match turn.kind() {
        HistoryTurnKind::CompactedSummary => {
            push_durable_value(required_field(structured, "summary")?, &mut builder)?;
            if let Some(messages) = structured
                .get("root_user_messages")
                .filter(|value| !value.is_null())
            {
                for message in messages.as_array().ok_or(FxProviderError::InvalidState(
                    "root user messages are not an array",
                ))? {
                    push_durable_value(message, &mut builder)?;
                }
            }
        }
        HistoryTurnKind::Assistant => {
            push_user_text(structured, &mut builder)?;
            push_durable_value(required_field(structured, "assistant")?, &mut builder)?;
        }
        HistoryTurnKind::BackgroundCommand | HistoryTurnKind::Interrupted => {
            push_user_text(structured, &mut builder)?;
            if let Some(assistant) = structured.get("assistant").filter(|value| !value.is_null()) {
                push_durable_value(assistant, &mut builder)?;
            }
        }
    }
    Ok((!builder.oversized).then_some(builder.body))
}

fn assistant_value<'a>(
    turn: &HistoryTurn,
    structured: &'a Value,
) -> FxProviderResult<Option<&'a Value>> {
    match turn.kind() {
        HistoryTurnKind::Assistant => Ok(Some(required_field(structured, "assistant")?)),
        HistoryTurnKind::BackgroundCommand | HistoryTurnKind::Interrupted => {
            Ok(structured.get("assistant").filter(|value| !value.is_null()))
        }
        HistoryTurnKind::CompactedSummary => Err(FxProviderError::InvalidState(
            "compacted summary cannot project as a message turn",
        )),
    }
}

fn user_turn_label(turn: &HistoryTurn) -> FxProviderResult<&'static str> {
    match turn.kind() {
        HistoryTurnKind::Assistant => Ok("user turn"),
        HistoryTurnKind::BackgroundCommand => Ok("background command turn"),
        HistoryTurnKind::Interrupted => Ok("interrupted turn"),
        HistoryTurnKind::CompactedSummary => Err(FxProviderError::InvalidState(
            "compacted summary cannot project as a user message",
        )),
    }
}

fn searchable_part_body(label: &str, value: &Value) -> FxProviderResult<Option<String>> {
    let mut builder = SearchBody::default();
    builder.push(label)?;
    push_durable_value(value, &mut builder)?;
    Ok((!builder.oversized).then_some(builder.body))
}

fn required_field<'a>(value: &'a Value, name: &str) -> FxProviderResult<&'a Value> {
    value.get(name).ok_or(FxProviderError::InvalidState(
        "searchable turn field is missing",
    ))
}

fn push_user_text(value: &Value, builder: &mut SearchBody) -> FxProviderResult<()> {
    let user = required_field(value, "user")?;
    push_durable_value(required_field(user, "text")?, builder)
}

fn push_durable_value(value: &Value, builder: &mut SearchBody) -> FxProviderResult<()> {
    match value {
        Value::String(text) => builder.push(text),
        Value::Object(object)
            if object.len() == 2
                && object.get("encoding").and_then(Value::as_str) == Some("base64") =>
        {
            let data =
                object
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or(FxProviderError::InvalidState(
                        "durable base64 wrapper has no data",
                    ))?;
            builder.push_prefixed("base64:", data)
        }
        _ => Err(FxProviderError::InvalidState(
            "searchable turn field is not durable text",
        )),
    }
}

#[derive(Default)]
struct SearchBody {
    body: String,
    oversized: bool,
}

impl SearchBody {
    fn push(&mut self, text: &str) -> FxProviderResult<()> {
        self.push_prefixed("", text)
    }

    fn push_prefixed(&mut self, prefix: &str, text: &str) -> FxProviderResult<()> {
        if self.oversized {
            return Ok(());
        }
        let separator = usize::from(!self.body.is_empty());
        let Some(next) = self
            .body
            .len()
            .checked_add(separator)
            .and_then(|size| size.checked_add(prefix.len()))
            .and_then(|size| size.checked_add(text.len()))
        else {
            self.body.clear();
            self.oversized = true;
            return Ok(());
        };
        if next > MAX_CORE_CONTENT_BYTES {
            self.body.clear();
            self.oversized = true;
            return Ok(());
        }
        if separator != 0 {
            self.body.push('\n');
        }
        self.body.push_str(prefix);
        self.body.push_str(text);
        Ok(())
    }
}
