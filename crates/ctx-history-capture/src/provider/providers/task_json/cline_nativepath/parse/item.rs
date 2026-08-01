use super::*;

#[derive(Clone, Copy)]
pub(super) struct ItemParseContext<'a> {
    pub(super) identity: &'a ClineTaskIdentity,
    pub(super) component: ClineEventComponent,
    pub(super) max_item_units: usize,
}

#[derive(Default)]
pub(super) struct RawEnvelope<'a> {
    pub(super) native_id: Option<String>,
    pub(super) role: Option<String>,
    pub(super) item_type: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) say: Option<String>,
    pub(super) ask: Option<String>,
    pub(super) name: Option<String>,
    pub(super) call_id: Option<String>,
    pub(super) occurred_at_millis: Option<i64>,
    pub(super) content: Option<&'a RawValue>,
    pub(super) text: Option<&'a RawValue>,
    pub(super) message: Option<&'a RawValue>,
    pub(super) output: Option<&'a RawValue>,
    pub(super) result: Option<&'a RawValue>,
    pub(super) response: Option<&'a RawValue>,
    pub(super) timed_out: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) explicit_failure: bool,
    pub(super) explicit_success: bool,
    pub(super) status: Option<String>,
    pub(super) conflicting_discriminator: bool,
    pub(super) oversized_discriminator: bool,
}

impl<'a> RawEnvelope<'a> {
    pub(super) fn unique_result_body(
        &self,
    ) -> Result<Option<&'a RawValue>, (ClineItemRejectionKind, String)> {
        let candidates = [
            self.output,
            self.result,
            self.text,
            self.content,
            self.message,
            self.response,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => Ok(None),
            [selected] => Ok(Some(*selected)),
            _ => Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline result exposes more than one candidate body field".to_owned(),
            )),
        }
    }

    pub(super) fn retained_body(&self) -> Option<&'a RawValue> {
        self.text.or(self.message).or(self.content)
    }

    pub(super) fn normalized_discriminators(&self) -> impl Iterator<Item = String> + '_ {
        [
            self.role.as_deref(),
            self.item_type.as_deref(),
            self.kind.as_deref(),
            self.say.as_deref(),
            self.ask.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(normalize_discriminator)
    }

    pub(super) fn outcome(&self) -> OutputOutcomeMetadata {
        let status = self.status.as_deref().map(normalize_discriminator);
        let outcome = if self.timed_out
            || status
                .as_deref()
                .is_some_and(|value| matches!(value, "timeout" | "timedout"))
        {
            OutputOutcome::Timeout
        } else if self.exit_code.is_some_and(|code| code != 0)
            || self.explicit_failure
            || status.as_deref().is_some_and(status_is_failure)
        {
            OutputOutcome::Failure
        } else if self.exit_code == Some(0)
            || self.explicit_success
            || status.as_deref().is_some_and(status_is_success)
        {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        };
        OutputOutcomeMetadata {
            outcome,
            exit_code: self.exit_code,
            duration_ms: self.duration_ms,
        }
    }
}

impl<'de> Deserialize<'de> for RawEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawEnvelopeVisitor)
    }
}

struct RawEnvelopeVisitor;

impl<'de> Visitor<'de> for RawEnvelopeVisitor {
    type Value = RawEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline native item object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut envelope = RawEnvelope::default();
        let mut seen_id = false;
        let mut seen_role = false;
        let mut seen_type = false;
        let mut seen_kind = false;
        let mut seen_say = false;
        let mut seen_ask = false;
        while let Some(BoundedString(field, _)) =
            map.next_key::<BoundedString<MAX_JSON_KEY_BYTES>>()?
        {
            let Some(field) = field else {
                map.next_value::<IgnoredAny>()?;
                continue;
            };
            match field.as_str() {
                "id" | "uuid" | "messageId" => {
                    let value = map.next_value::<BoundedString<MAX_NATIVE_ID_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    let value = value.0;
                    if seen_id && envelope.native_id != value {
                        envelope.native_id = None;
                    } else if !seen_id {
                        envelope.native_id = value;
                    }
                    seen_id = true;
                }
                "role" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_role;
                    seen_role = true;
                    envelope.role = value.0;
                }
                "type" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_type;
                    seen_type = true;
                    envelope.item_type = value.0;
                }
                "kind" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_kind;
                    seen_kind = true;
                    envelope.kind = value.0;
                }
                "say" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_say;
                    seen_say = true;
                    envelope.say = value.0;
                }
                "ask" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_ask;
                    seen_ask = true;
                    envelope.ask = value.0;
                }
                "name" | "tool" | "tool_name" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    if envelope.name.is_none() {
                        envelope.name = value.0;
                    }
                }
                "tool_use_id" | "toolUseId" | "call_id" | "callId" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    if envelope.call_id.is_none() {
                        envelope.call_id = value.0;
                    }
                }
                "ts" | "timestamp" | "createdAt" => {
                    envelope.occurred_at_millis = map.next_value::<LooseTimestamp>()?.0;
                }
                "content" => set_raw_once(
                    &mut envelope.content,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "text" => set_raw_once(
                    &mut envelope.text,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "message" => set_raw_once(
                    &mut envelope.message,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "output" => set_raw_once(
                    &mut envelope.output,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "result" => set_raw_once(
                    &mut envelope.result,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "response" => set_raw_once(
                    &mut envelope.response,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "timed_out" | "timedOut" | "timeout" => {
                    envelope.timed_out |= map.next_value::<LooseBool>()?.0.unwrap_or(false);
                }
                "exit_code" | "exitCode" => {
                    envelope.exit_code = map.next_value::<LooseI32>()?.0;
                }
                "duration_ms" | "durationMs" => {
                    envelope.duration_ms = map.next_value::<LooseU64>()?.0;
                }
                "success" | "ok" => match map.next_value::<LooseBool>()?.0 {
                    Some(true) => envelope.explicit_success = true,
                    Some(false) => envelope.explicit_failure = true,
                    None => {}
                },
                "isError" | "is_error" | "failed" => {
                    envelope.explicit_failure |= map.next_value::<LooseBool>()?.0.unwrap_or(false);
                }
                "status" | "state" | "outcome" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.status = value.0;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }
}

pub(super) fn set_raw_once<'a>(
    slot: &mut Option<&'a RawValue>,
    value: &'a RawValue,
    duplicate: &mut bool,
) {
    if slot.replace(value).is_some() {
        *duplicate = true;
    }
}

pub(super) struct OutputCandidate<'a> {
    pub(super) kind: OutputObservationKind,
    pub(super) sub_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) outcome: OutputOutcomeMetadata,
    pub(super) body: Option<&'a RawValue>,
}

pub(super) struct OutputCandidateContext {
    pub(super) kind: OutputObservationKind,
    pub(super) base_sub_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn push_explicit_outputs<'a>(
    selected: Option<&'a RawValue>,
    context: OutputCandidateContext,
    outputs: &mut Vec<OutputCandidate<'a>>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let mut leaves = Vec::new();
    if let Some(selected) = selected {
        collect_explicit_output_leaves(selected, &mut leaves, 0)?;
    }
    for (inner_index, leaf) in leaves.into_iter().enumerate() {
        if inner_index >= CLINE_NATIVE_PAGE_MAX_UNITS {
            return Err((
                ClineItemRejectionKind::UnsupportedShape,
                "Cline result has more than 64 explicit inner outputs".to_owned(),
            ));
        }
        outputs.push(OutputCandidate {
            kind: context.kind,
            sub_index: context
                .base_sub_index
                .saturating_add(u32::try_from(inner_index).unwrap_or(u32::MAX)),
            call_id: context.call_id.clone(),
            outcome: context.outcome.clone(),
            body: Some(leaf),
        });
    }
    Ok(())
}

pub(super) fn push_explicit_result_blocks<'a>(
    content: &'a RawValue,
    kind: OutputObservationKind,
    outer: &RawEnvelope<'a>,
    outputs: &mut Vec<OutputCandidate<'a>>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let blocks = deserialize_bounded_raw_array(content, "Cline explicit result block array")?;
    for (index, raw_block) in blocks.into_iter().enumerate() {
        if !raw_block.get().trim_start().starts_with('{') {
            continue;
        }
        let block = serde_json::from_str::<RawEnvelope<'_>>(raw_block.get()).map_err(|error| {
            (
                ClineItemRejectionKind::MalformedRecord,
                format!("malformed Cline explicit result block: {error}"),
            )
        })?;
        if block.conflicting_discriminator || block.oversized_discriminator {
            return Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline explicit result block has conflicting or oversized discriminator fields"
                    .to_owned(),
            ));
        }
        if !block
            .normalized_discriminators()
            .any(|value| is_result_discriminator(&value))
        {
            continue;
        }
        let block_outcome = block.outcome();
        let outcome = if block_outcome.outcome == OutputOutcome::Unknown
            && block_outcome.exit_code.is_none()
            && block_outcome.duration_ms.is_none()
        {
            outer.outcome()
        } else {
            block_outcome
        };
        push_explicit_outputs(
            block.unique_result_body()?,
            OutputCandidateContext {
                kind,
                base_sub_index: u32::try_from(index)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(1_024),
                call_id: block.call_id.clone().or_else(|| outer.call_id.clone()),
                outcome,
            },
            outputs,
        )?;
    }
    Ok(())
}

pub(super) fn collect_explicit_output_leaves<'a>(
    raw: &'a RawValue,
    leaves: &mut Vec<&'a RawValue>,
    depth: usize,
) -> Result<(), (ClineItemRejectionKind, String)> {
    if depth >= MAX_EXPLICIT_RESULT_DEPTH {
        return Err((
            ClineItemRejectionKind::UnsupportedShape,
            "Cline explicit result exceeds the bounded nesting depth".to_owned(),
        ));
    }
    let text = raw.get().trim_start();
    if text == "null" {
        return Ok(());
    }
    if text.starts_with('[') {
        let items = deserialize_bounded_raw_array(raw, "explicit Cline result array")?;
        for item in items {
            if leaves.len() > CLINE_NATIVE_PAGE_MAX_UNITS {
                break;
            }
            let selected = if item.get().trim_start().starts_with('{') {
                serde_json::from_str::<RawExplicitInner<'_>>(item.get())
                    .map_err(|error| {
                        (
                            ClineItemRejectionKind::MalformedRecord,
                            format!("malformed explicit Cline result value: {error}"),
                        )
                    })?
                    .selected()?
                    .unwrap_or(item)
            } else {
                item
            };
            collect_explicit_output_leaves(selected, leaves, depth.saturating_add(1))?;
        }
        return Ok(());
    }
    if text.starts_with('{') {
        let selected = serde_json::from_str::<RawExplicitInner<'_>>(raw.get())
            .map_err(|error| {
                (
                    ClineItemRejectionKind::MalformedRecord,
                    format!("malformed explicit Cline result value: {error}"),
                )
            })?
            .selected()?;
        if let Some(selected) = selected {
            return collect_explicit_output_leaves(selected, leaves, depth.saturating_add(1));
        }
    }
    leaves.push(raw);
    Ok(())
}

pub(super) fn deserialize_bounded_raw_array<'a>(
    raw: &'a RawValue,
    context: &'static str,
) -> Result<Vec<&'a RawValue>, (ClineItemRejectionKind, String)> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let values = deserializer
        .deserialize_seq(BoundedRawArrayVisitor)
        .map_err(|error| {
            let kind = if error.to_string().contains("more than 64") {
                ClineItemRejectionKind::UnsupportedShape
            } else {
                ClineItemRejectionKind::MalformedRecord
            };
            (kind, format!("malformed {context}: {error}"))
        })?;
    deserializer.end().map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("trailing {context} data: {error}"),
        )
    })?;
    Ok(values)
}

struct BoundedRawArrayVisitor;

impl<'de> Visitor<'de> for BoundedRawArrayVisitor {
    type Value = Vec<&'de RawValue>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline array with no more than 64 values")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(CLINE_NATIVE_PAGE_MAX_UNITS);
        while values.len() < CLINE_NATIVE_PAGE_MAX_UNITS {
            let Some(value) = sequence.next_element::<&RawValue>()? else {
                return Ok(values);
            };
            values.push(value);
        }
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Cline array has more than 64 independently publishable values",
            ));
        }
        Ok(values)
    }
}

#[derive(Default)]
struct RawExplicitInner<'a> {
    text: Option<&'a RawValue>,
    content: Option<&'a RawValue>,
    output: Option<&'a RawValue>,
    result: Option<&'a RawValue>,
    ambiguous: bool,
}

impl<'a> RawExplicitInner<'a> {
    fn selected(&self) -> Result<Option<&'a RawValue>, (ClineItemRejectionKind, String)> {
        if self.ambiguous {
            return Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline explicit result object exposes more than one candidate body field"
                    .to_owned(),
            ));
        }
        Ok(self.text.or(self.content).or(self.output).or(self.result))
    }
}

impl<'de> Deserialize<'de> for RawExplicitInner<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawExplicitInnerVisitor)
    }
}

struct RawExplicitInnerVisitor;

impl<'de> Visitor<'de> for RawExplicitInnerVisitor {
    type Value = RawExplicitInner<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an explicit Cline result object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut inner = RawExplicitInner::default();
        while let Some(BoundedString(field, _)) =
            map.next_key::<BoundedString<MAX_JSON_KEY_BYTES>>()?
        {
            match field.as_deref() {
                Some("text") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.text.replace(value).is_some()
                        || inner.content.is_some()
                        || inner.output.is_some()
                        || inner.result.is_some();
                }
                Some("content") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.content.replace(value).is_some()
                        || inner.text.is_some()
                        || inner.output.is_some()
                        || inner.result.is_some();
                }
                Some("output") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.output.replace(value).is_some()
                        || inner.text.is_some()
                        || inner.content.is_some()
                        || inner.result.is_some();
                }
                Some("result") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.result.replace(value).is_some()
                        || inner.text.is_some()
                        || inner.content.is_some()
                        || inner.output.is_some();
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(inner)
    }
}

pub(super) fn parse_item(
    raw: &RawValue,
    context: ItemParseContext<'_>,
    native_index: u64,
    native_id_occurrences: &mut BTreeMap<String, u64>,
    stats: &mut ClinePublicationStats,
) -> ParsedItem {
    let ItemParseContext {
        identity,
        component,
        max_item_units,
    } = context;
    let observed_bytes = u64::try_from(raw.get().len()).unwrap_or(u64::MAX);
    let envelope = match serde_json::from_str::<RawEnvelope<'_>>(raw.get()) {
        Ok(envelope) => envelope,
        Err(error) => {
            return rejected_item(
                component,
                native_index,
                None,
                observed_bytes,
                ClineItemRejectionKind::MalformedRecord,
                &error.to_string(),
                stats,
            );
        }
    };
    let native_key = native_key(
        envelope.native_id.as_deref(),
        native_index,
        Some(native_id_occurrences),
    );
    if envelope.conflicting_discriminator || envelope.oversized_discriminator {
        let kind = if envelope.conflicting_discriminator {
            ClineItemRejectionKind::ConflictingDiscriminator
        } else {
            ClineItemRejectionKind::OversizedRetainedItem
        };
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            kind,
            "Cline item has conflicting or oversized discriminator fields",
            native_key,
            stats,
        );
    }

    let parsed = match component {
        ClineEventComponent::ApiHistory | ClineEventComponent::FallbackHistory => {
            parse_api_projection(&envelope, component, identity, &native_key, native_index)
        }
        ClineEventComponent::UiMessages => {
            parse_ui_projection(&envelope, identity, &native_key, native_index)
        }
    };
    let mut projection = match parsed {
        Ok(projection) => projection,
        Err((kind, detail)) => {
            return rejected_item_with_key(
                component,
                native_index,
                envelope.native_id,
                observed_bytes,
                kind,
                &detail,
                native_key,
                stats,
            );
        }
    };
    let output_rows = projection.outputs.len();
    let retained_units = projection
        .rows
        .len()
        .saturating_add(output_rows)
        .saturating_add(projection.rows.iter().fold(0_usize, |count, row| {
            count.saturating_add(row.file_touches.len())
        }));
    if retained_units > max_item_units {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::UnsupportedShape,
            "Cline item exceeds its activation-invariant page unit budget",
            native_key,
            stats,
        );
    }
    let mut output_outcomes = Vec::with_capacity(projection.outputs.len());
    for output in projection.outputs {
        stats.output_outcomes_observed = stats.output_outcomes_observed.saturating_add(1);
        output_outcomes.push(output.outcome.clone());
        let body = match output.body.map(decode_explicit_output_text).transpose() {
            Ok(Some(body)) if !body.trim().is_empty() => body,
            Ok(_) => continue,
            Err((kind, detail)) => {
                return rejected_item_with_key(
                    component,
                    native_index,
                    envelope.native_id,
                    observed_bytes,
                    kind,
                    &detail,
                    native_key,
                    stats,
                );
            }
        };
        let output_bytes = body.len();
        projection.rows.push(ClineEventRow::output(
            ClineEventContext {
                task: identity,
                component,
                item: &native_key,
                item_index: native_index,
                role: ClineEventRole::Unknown,
                occurred_at_millis: projection.occurred_at_millis,
            },
            output.sub_index,
            match output.kind {
                OutputObservationKind::Command => ClineEventKind::CommandOutput,
                OutputObservationKind::Tool => ClineEventKind::ToolOutput,
            },
            body,
            ClineSparseOutputDiagnostic {
                outcome: output.outcome.outcome,
                exit_code: output.outcome.exit_code,
                duration_ms: output.outcome.duration_ms,
                output_bytes,
                call_id: output.call_id.map(String::into_boxed_str),
            },
        ));
    }
    let retained_body_bytes = projection
        .rows
        .iter()
        .map(|row| row.body.as_deref().map_or(0, str::len))
        .sum::<usize>();
    if retained_body_bytes > CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline selected item content exceeds the shared Core content bound",
            native_key,
            stats,
        );
    }
    projection
        .rows
        .sort_by_key(|row| (row.native_order.item_index, row.native_order.sub_index));
    let core_bytes = projection
        .rows
        .iter()
        .map(estimated_event_bytes)
        .sum::<usize>();
    if core_bytes > CLINE_NATIVE_CORE_PAGE_MAX_BYTES {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline Core projection exceeds the shared encoded Core record bound",
            native_key,
            stats,
        );
    }
    let checkpoint = ClineItemCheckpoint::new(native_key, &projection.rows, &output_outcomes, None);
    stats.core_rows = stats.core_rows.saturating_add(projection.rows.len());
    ParsedItem {
        checkpoint,
        rows: projection.rows,
        rejection: None,
        core_bytes,
        source_record: None,
    }
}

fn decode_explicit_output_text(raw: &RawValue) -> Result<String, (ClineItemRejectionKind, String)> {
    let value = serde_json::from_str::<Value>(raw.get()).map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("invalid selected Cline result content: {error}"),
        )
    })?;
    Ok(provider_normalized_result_value(&value))
}

#[cfg(test)]
mod result_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retains_complete_results_and_rejects_ambiguity_for_cline_and_roo() {
        let identity = ClineTaskIdentity::new("shared-task");
        let parse = |value: serde_json::Value| {
            let raw = RawValue::from_string(value.to_string()).unwrap();
            parse_item(
                &raw,
                ItemParseContext {
                    identity: &identity,
                    component: ClineEventComponent::ApiHistory,
                    max_item_units: 60,
                },
                0,
                &mut BTreeMap::new(),
                &mut ClinePublicationStats::default(),
            )
        };

        for (status, expected) in [
            (Some("success"), OutputOutcome::Success),
            (Some("failure"), OutputOutcome::Failure),
            (None, OutputOutcome::Unknown),
        ] {
            let mut value = json!({
                "role": "tool",
                "tool_use_id": "call-1",
                "content": format!("complete-{expected:?}"),
            });
            if let Some(status) = status {
                value["status"] = json!(status);
            }
            let item = parse(value);
            assert!(item.rejection.is_none());
            assert_eq!(item.rows.len(), 1);
            let expected_body = format!("complete-{expected:?}");
            assert_eq!(item.rows[0].body.as_deref(), Some(expected_body.as_str()));
            let output = item.rows[0].sparse_output.as_ref().unwrap();
            assert_eq!(output.outcome, expected);
            assert_eq!(output.call_id.as_deref(), Some("call-1"));
        }

        let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
        let item = parse(json!({
            "role": "tool",
            "tool_use_id": "large-call",
            "content": large,
            "status": "success",
        }));
        assert!(item.rejection.is_none());
        assert_eq!(
            item.rows[0].body.as_deref().unwrap().len(),
            9 * 1024 * 1024 + 4
        );
        assert!(item.rows[0].body.as_deref().unwrap().ends_with("tail"));

        let ambiguous = parse(json!({
            "role": "tool",
            "content": "first",
            "output": "second",
        }));
        assert_eq!(
            ambiguous.rejection.as_ref().map(|value| value.kind),
            Some(ClineItemRejectionKind::ConflictingDiscriminator)
        );
    }
}
