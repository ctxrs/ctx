use super::*;
use ctx_history_capture_model::file_references::visit_provider_file_reference_drafts_with_limit;
use ctx_history_capture_runtime::SourceBackedRecordRejectionDrafts;
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture,
    CoreActivity, CoreRecord, EventIdentityInput, EventType, LiteralFactKind, NativeItemKey,
    ProviderDeclaredFact, CORE_ACTIVITY_REVISION,
};
use ctx_history_jsonl::{JsonlFamilyWorkerContext, JsonlRecordRejections};
use serde_json::Map;

pub(super) struct PrimeProjector<R> {
    source: SourceKey,
    session: StableEntityId,
    binding: Binding,
    rejections: JsonlRecordRejections,
    runtime: PhantomData<fn() -> R>,
}

impl<R> PrimeProjector<R> {
    pub(super) fn new(leaf: &JsonlFamilyLeaf, binding: Binding) -> Result<Self> {
        Ok(Self {
            source: leaf.source().clone(),
            session: session_id(leaf.source(), &binding.native_id)?,
            binding,
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::PrimeAgent,
                leaf.source_path().display().to_string(),
            ),
            runtime: PhantomData,
        })
    }

    fn records(&self, value: &Value, ordinal: u64) -> Result<Vec<CoreRecord>> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Prime Agent record has no type"))?;
        let occurred_at =
            timestamp(value).ok_or_else(|| invalid("invalid Prime Agent event timestamp"))?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid("Prime Agent record has no native ID"))?;
        if kind == "session" && (ordinal != 0 || id != self.binding.native_id) {
            return Err(invalid("unexpected Prime Agent session header"));
        }
        let selected = selected_entry(value)?;
        let message = selected.get("message").unwrap_or(&selected);
        let role = message.get("role").and_then(Value::as_str);
        let event_type = match (kind, role) {
            ("message", Some("toolResult")) => EventType::ToolOutput,
            ("message", Some("bashExecution")) => EventType::CommandOutput,
            ("compaction" | "branch_summary", _)
            | ("message", Some("branchSummary" | "compactionSummary")) => EventType::Summary,
            ("message", Some("user" | "assistant" | "system" | "developer")) => EventType::Message,
            _ => EventType::Notice,
        };
        let mut records = Vec::new();
        let body = entry_text(&selected);
        // Keep real text as a message and each tool block as a separate call.
        // This preserves text ranking and exact invocation/result linkage.
        let calls = if kind == "message" && role == Some("assistant") {
            message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, block)| block.get("type").and_then(Value::as_str) == Some("toolCall"))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if !body.is_empty() || calls.is_empty() {
            records.push(self.record(
                id,
                ordinal,
                0,
                occurred_at,
                event_type,
                role,
                body,
                selected.clone(),
                None,
            )?);
        }
        for (index, call) in calls {
            let body = serde_json::to_string(call.get("arguments").unwrap_or(&Value::Null))?;
            records.push(self.record(
                id,
                ordinal,
                u32::try_from(index + 1).map_err(invalid)?,
                occurred_at,
                EventType::ToolCall,
                None,
                body,
                call.clone(),
                Some(call),
            )?);
        }
        Ok(records)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        id: &str,
        ordinal: u64,
        part: u32,
        occurred_at: DateTime<Utc>,
        event_type: EventType,
        role: Option<&str>,
        body: String,
        structured: Value,
        call: Option<&Value>,
    ) -> Result<CoreRecord> {
        let key = TypedKey::composite(vec![
            TypedKey::utf8(id).map_err(invalid)?,
            TypedKey::utf8(part.to_string()).map_err(invalid)?,
        ])
        .map_err(invalid)?;
        let item = NativeItemKey::native_id("prime-agent.entry", key.clone()).map_err(invalid)?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session,
            logical_item_kind: "prime-agent-event",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .map_err(invalid)?;
        let sequence = ordinal
            .checked_mul(u64::from(u32::MAX) + 1)
            .and_then(|n| n.checked_add(u64::from(part)))
            .ok_or_else(|| invalid("Prime Agent sequence overflow"))?;
        // Initialize with selected content, then keep metadata-only events
        // structured. No placeholder prose becomes searchable conversation.
        let mut record = CoreRecord::new_selected(
            event_id,
            self.session,
            self.source.clone(),
            sequence,
            event_type.as_str(),
            PARSER_REVISION,
            serde_json::to_string(&structured)?,
        )
        .map_err(invalid)?;
        record.content.normalized_body = (!body.is_empty()).then_some(body.clone());
        let message = structured.get("message").unwrap_or(&structured);
        record.content.activity = activity(message, call, event_type, &body, &self.binding.cwd)?;
        record.content.structured_content = Some(structured);
        record.provider_session_id = Some(self.binding.native_id.clone());
        record.parent_session_id = self.binding.parent;
        record.session_relationship = self.binding.relationship;
        record.agent_scope = self.binding.agent_scope;
        record.native_event_id = Some(key);
        record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
        record.role = match role {
            Some("user" | "assistant" | "system" | "developer") => role.map(str::to_owned),
            Some("toolResult") => Some("tool".to_owned()),
            _ => None,
        };
        // Reject an indivisible over-bound record instead of retaining a prefix.
        record.validate_contract().map_err(invalid)?;
        Ok(record)
    }
}

impl<R: JsonlProviderRuntime> JsonlFamilyProjector for PrimeProjector<R> {
    type Runtime = R;
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if record.evidence().physical_ordinal() == 0 {
            let header: Value = serde_json::from_slice(record.bytes())
                .map_err(|_| CaptureError::source_changed())?;
            if selected_entry(&header).map_err(|_| CaptureError::source_changed())?
                != self.binding.header
            {
                return Err(CaptureError::source_changed());
            }
        }
        if record.oversized() {
            self.rejections
                .malformed(record, "Prime Agent record exceeds the physical byte limit");
            return Ok(());
        }
        let parsed = serde_json::from_slice::<Value>(record.bytes())
            .map_err(invalid)
            .and_then(|value| self.records(&value, record.evidence().physical_ordinal()));
        match parsed {
            Ok(records) => {
                for record in records {
                    emit(record)?;
                }
            }
            Err(error) => self.rejections.malformed(record, error.to_string()),
        }
        Ok(())
    }
    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }
    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

fn select(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect::<Map<_, _>>(),
    )
}

fn selected_content(value: &Value) -> Result<Value> {
    if value.is_string() {
        return Ok(value.clone());
    }
    let blocks = value
        .as_array()
        .ok_or_else(|| invalid("Prime Agent content must be text or blocks"))?;
    let mut selected = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                selected.push(select(block, &["type", "text"]));
            }
            Some("thinking") if block.get("redacted").and_then(Value::as_bool) != Some(true) => {
                selected.push(select(block, &["type", "thinking"]));
            }
            Some("toolCall") => {
                selected.push(select(block, &["type", "id", "name", "arguments"]));
            }
            // Image data, reasoning signatures and opaque provider blocks are
            // outside the native content policy, including custom-message images.
            _ => {}
        }
    }
    Ok(Value::Array(selected))
}

pub(super) fn selected_entry(value: &Value) -> Result<Value> {
    let mut selected = select(
        value,
        &[
            "type",
            "id",
            "parentId",
            "timestamp",
            "version",
            "cwd",
            "parentSession",
            "rlmDepth",
            "provider",
            "modelId",
            "thinkingLevel",
            "serviceTier",
            "state",
            "name",
            "label",
            "summary",
            "customInstructions",
            "harnessDigest",
            "customType",
        ],
    );
    if let Some(content) = value.get("content") {
        selected["content"] = selected_content(content)?;
    }
    if value.get("type").and_then(Value::as_str) == Some("message") {
        let message = value
            .get("message")
            .filter(|value| value.is_object())
            .ok_or_else(|| invalid("Prime Agent message payload is missing"))?;
        let mut fields = select(
            message,
            &[
                "role",
                "timestamp",
                "model",
                "provider",
                "stopReason",
                "errorMessage",
                "toolCallId",
                "toolName",
                "isError",
                "command",
                "output",
                "exitCode",
                "cancelled",
                "truncated",
                "summary",
                "customType",
                "customInstructions",
                "harnessDigest",
            ],
        );
        if let Some(content) = message.get("content") {
            fields["content"] = selected_content(content)?;
        }
        if message.get("role").and_then(Value::as_str) == Some("toolResult") {
            if let Some(details) = message.get("details") {
                fields["details"] = select(
                    details,
                    &[
                        "status",
                        "durationMs",
                        "stdout",
                        "stderr",
                        "result",
                        "bashCommands",
                        "backgroundOutput",
                        "error",
                        "diffs",
                        "kernelRestarted",
                    ],
                );
            }
        }
        selected["message"] = fields;
    }
    if value.get("type").and_then(Value::as_str) == Some("tool_started") {
        for key in ["toolCallId", "toolName", "args"] {
            if let Some(value) = value.get(key) {
                selected[key] = value.clone();
            }
        }
    }
    if let Some(git) = value.get("git") {
        selected["git"] = select(git, &["repoUrl", "commit", "branch"]);
    }
    Ok(selected)
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block.get("text").and_then(Value::as_str),
            Some("thinking") => block.get("thinking").and_then(Value::as_str),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn entry_text(value: &Value) -> String {
    let message = value.get("message").unwrap_or(value);
    let mut parts = Vec::new();
    if let Some(content) = message.get("content") {
        let text = content_text(content);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    for key in [
        "summary",
        "output",
        "errorMessage",
        "customInstructions",
        "harnessDigest",
    ] {
        if let Some(text) = message.get(key).and_then(Value::as_str) {
            parts.push(text.to_owned());
        }
    }
    if let Some(details) = message.get("details") {
        // Diffs and other tool details remain complete and searchable even when
        // the rendered tool text has no copy of them.
        parts.push(details.to_string());
    }
    parts.join("\n")
}

fn activity(
    message: &Value,
    call: Option<&Value>,
    kind: EventType,
    body: &str,
    cwd: &str,
) -> Result<Option<CoreActivity>> {
    let mut facts = Vec::<ProviderDeclaredFact>::new();
    if let Some(fact) = admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.to_owned(), 0)
    {
        facts.push(fact);
    }
    let references =
        visit_provider_file_reference_drafts_with_limit(message, 63, |(_, reference)| {
            if let Some(fact) =
                admit_provider_declared_fact(reference.kind, reference.value, facts.len())
            {
                facts.push(fact);
            }
            Ok::<_, CaptureError>(())
        })?;
    if references.limit_exceeded() {
        return Err(invalid("Prime Agent record has too many file references"));
    }
    let raw_id = call
        .and_then(|call| call.get("id"))
        .or_else(|| message.get("toolCallId"))
        .and_then(Value::as_str);
    let provider_call_id = admit_optional_provider_call_id(raw_id.map(str::to_owned));
    let invocation = if provider_call_id.is_some() {
        call.and_then(|call| {
            admit_optional_metadata_text(
                call.get("name").and_then(Value::as_str).map(str::to_owned),
            )
            .map(|tool| ActivityInvocation {
                protocol: None,
                server: None,
                tool,
                arguments: call
                    .get("arguments")
                    .map_or(ActivityJsonCapture::Absent, |value| {
                        ActivityJsonCapture::Present {
                            value: value.clone(),
                        }
                    }),
                started_at_unix_ms: None,
            })
        })
    } else {
        None
    };
    let result =
        (provider_call_id.is_some() && kind == EventType::ToolOutput).then(|| ActivityResult {
            status: message
                .pointer("/details/status")
                .and_then(Value::as_str)
                .map(str::to_owned),
            completed_at_unix_ms: message.get("timestamp").and_then(Value::as_i64),
            duration_ns: message
                .pointer("/details/durationMs")
                .and_then(Value::as_u64)
                .and_then(|ms| ms.checked_mul(1_000_000)),
            text: if body.is_empty() {
                ActivityTextCapture::Absent
            } else {
                ActivityTextCapture::NormalizedBody
            },
            structured_content: ActivityJsonCapture::Present {
                value: message.clone(),
            },
        });
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
}

#[cfg(test)]
mod tests;
