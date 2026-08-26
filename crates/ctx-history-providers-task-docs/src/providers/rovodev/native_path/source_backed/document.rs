use super::*;
use serde::{
    de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};

#[derive(Debug, Default)]
struct ContextHeaderProbe {
    session_id: Option<String>,
    camel_session_id: Option<String>,
    has_message_history: bool,
    session_id_conflict: bool,
}

impl ContextHeaderProbe {
    fn provider_session_id(&self) -> Option<String> {
        if self.session_id_conflict
            || self
                .session_id
                .as_ref()
                .zip(self.camel_session_id.as_ref())
                .is_some_and(|(left, right)| left != right)
        {
            return None;
        }
        self.session_id
            .as_deref()
            .or(self.camel_session_id.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    }
}

#[derive(Default)]
struct HeaderBounds(usize);

impl HeaderBounds {
    fn admit<E: serde::de::Error>(&mut self) -> Result<(), E> {
        self.0 = self.0.saturating_add(1);
        if self.0 > SOURCE_BACKED_MAX_COLLECTION_ELEMENTS {
            return Err(E::custom("Rovo Dev JSON exceeds its element budget"));
        }
        Ok(())
    }
}

struct BoundedValueSeed<'a>(&'a mut HeaderBounds, usize);

impl<'de> DeserializeSeed<'de> for BoundedValueSeed<'_> {
    type Value = bool;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.0.admit()?;
        if self.1 > SOURCE_BACKED_MAX_JSON_DEPTH {
            return Err(serde::de::Error::custom("Rovo Dev JSON is too deep"));
        }
        deserializer.deserialize_any(BoundedValueVisitor(self.0, self.1))
    }
}

struct BoundedValueVisitor<'a>(&'a mut HeaderBounds, usize);

impl<'de> Visitor<'de> for BoundedValueVisitor<'_> {
    type Value = bool;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded JSON value")
    }

    fn visit_unit<E>(self) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_bool<E>(self, _: bool) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_i64<E>(self, _: i64) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_u64<E>(self, _: u64) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_f64<E>(self, _: f64) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_str<E>(self, _: &str) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<bool, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(BoundedValueSeed(self.0, self.1.saturating_add(1)))?
            .is_some()
        {}
        Ok(true)
    }

    fn visit_map<A>(self, mut map: A) -> Result<bool, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value_seed(BoundedValueSeed(self.0, self.1.saturating_add(1)))?;
        }
        Ok(false)
    }
}

impl<'de> Deserialize<'de> for ContextHeaderProbe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ContextVisitor;

        impl<'de> Visitor<'de> for ContextVisitor {
            type Value = ContextHeaderProbe;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded Rovo Dev context object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut header = ContextHeaderProbe::default();
                let mut bounds = HeaderBounds::default();
                while let Some(field) = map.next_key::<String>()? {
                    match field.as_str() {
                        "session_id" | "sessionId" => {
                            bounds.admit()?;
                            let value: Option<String> = map.next_value()?;
                            let slot = if field == "session_id" {
                                &mut header.session_id
                            } else {
                                &mut header.camel_session_id
                            };
                            if slot
                                .as_ref()
                                .zip(value.as_ref())
                                .is_some_and(|(left, right)| left != right)
                            {
                                header.session_id_conflict = true;
                                *slot = None;
                            } else if slot.is_none() {
                                *slot = value;
                            }
                        }
                        "message_history" | "messages" => {
                            let is_array = map.next_value_seed(BoundedValueSeed(&mut bounds, 1))?;
                            if !is_array {
                                return Err(serde::de::Error::custom(
                                    "Rovo Dev message history is not an array",
                                ));
                            }
                            header.has_message_history = true;
                        }
                        _ => {
                            map.next_value_seed(BoundedValueSeed(&mut bounds, 1))?;
                        }
                    }
                }
                Ok(header)
            }
        }

        deserializer.deserialize_map(ContextVisitor)
    }
}

impl RovoDevTreeAuthority {
    pub(super) fn source(
        &self,
        leaf: &RovoDevDocumentLeaf,
    ) -> RovoDevSourceBackedResult<&RovoDevOpenedSource> {
        self.sources
            .get(leaf.source_index)
            .ok_or(RovoDevSourceBackedError::CountMismatch)
    }

    fn bind_document(
        &self,
        leaf: &RovoDevDocumentLeaf,
        snapshot: &RovoDevSnapshot,
        source_anchor_scope: SourceAnchorScope,
    ) -> RovoDevSourceBackedResult<RovoDevBoundDocument> {
        let source = self.source(leaf)?;
        let header = document_header_from_snapshot(source, snapshot, source_anchor_scope)?;
        if header != leaf.header {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let parent_session_id = header
            .parent_provider_session_id
            .as_deref()
            .map(|provider_session_id| {
                provider_thread_session_identity_scoped(provider_session_id, source_anchor_scope)
            })
            .transpose()?;
        Ok(RovoDevBoundDocument {
            source_key: header.source_key,
            provider_session_id: header.provider_session_id,
            session_id: header.session_id,
            parent_session_id,
            unique_message_ids: unique_message_ids(snapshot),
        })
    }
}

fn document_header_from_snapshot(
    source: &RovoDevOpenedSource,
    snapshot: &RovoDevSnapshot,
    source_anchor_scope: SourceAnchorScope,
) -> RovoDevSourceBackedResult<RovoDevDocumentHeader> {
    let (provider_session_id, parent_provider_session_id) = snapshot
        .document
        .as_ref()
        .map(|document| {
            (
                document.provider_session_id.clone(),
                document.parent_provider_session_id.clone(),
            )
        })
        .unwrap_or_else(|_| (source.source.provider_session_id.clone(), None));
    document_header(
        provider_session_id,
        parent_provider_session_id,
        source_anchor_scope,
    )
}

fn document_header(
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    source_anchor_scope: SourceAnchorScope,
) -> RovoDevSourceBackedResult<RovoDevDocumentHeader> {
    let source_key = rovodev_source_key_scoped(&provider_session_id, source_anchor_scope)?;
    let session_id = rovodev_session_identity(&source_key, &provider_session_id)?;
    Ok(RovoDevDocumentHeader {
        source_key,
        provider_session_id,
        parent_provider_session_id,
        session_id,
    })
}

pub(super) fn probe_document_header(
    source: &RovoDevOpenedSource,
    source_anchor_scope: SourceAnchorScope,
) -> RovoDevSourceBackedResult<RovoDevDocumentHeader> {
    let files = source.open_files()?;
    let fallback = || {
        document_header(
            source.source.provider_session_id.clone(),
            None,
            source_anchor_scope,
        )
    };
    if source.opening.context_length() > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
        files.revalidate()?;
        return fallback();
    }
    let context_bytes = files.context.read_exact_range(
        0,
        usize::try_from(source.opening.context_length())
            .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
        MAX_PROVIDER_JSONL_LINE_BYTES,
    )?;
    if json_has_duplicate_key(&context_bytes).unwrap_or(true) {
        files.revalidate()?;
        return fallback();
    }
    let context_provider_session_id =
        match serde_json::from_slice::<ContextHeaderProbe>(&context_bytes) {
            Ok(header) if header.has_message_history => header.provider_session_id(),
            _ => {
                let context_json = match serde_json::from_slice::<serde_json::Value>(&context_bytes)
                {
                    Ok(value)
                        if validate_json_bounds(&value).is_ok()
                            && message_history(&value).is_some() =>
                    {
                        value
                    }
                    _ => {
                        files.revalidate()?;
                        return fallback();
                    }
                };
                exact_provider_string_field(&context_json, &["session_id", "sessionId"])
            }
        };
    let metadata = match (files.metadata.as_ref(), source.opening.metadata_length()) {
        (Some(_), Some(length)) if length > MAX_PROVIDER_JSONL_LINE_BYTES as u64 => {
            serde_json::Value::Null
        }
        (Some(file), Some(length)) => {
            let bytes = file.read_exact_range(
                0,
                usize::try_from(length).map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
                MAX_PROVIDER_JSONL_LINE_BYTES,
            )?;
            if json_has_duplicate_key(&bytes).unwrap_or(true) {
                serde_json::Value::Null
            } else {
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .filter(|value| validate_json_bounds(value).is_ok())
                    .unwrap_or(serde_json::Value::Null)
            }
        }
        (None, None) => serde_json::Value::Null,
        _ => return Err(CaptureError::SourceChangedDuringCapture.into()),
    };
    let provider_session_id = exact_provider_string_field(&metadata, &["session_id", "sessionId"])
        .or(context_provider_session_id)
        .unwrap_or_else(|| source.source.provider_session_id.clone());
    let parent_provider_session_id = exact_provider_string_field(
        &metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    files.revalidate()?;
    document_header(
        provider_session_id,
        parent_provider_session_id,
        source_anchor_scope,
    )
}

#[derive(Debug)]
pub(super) struct ProjectedMessage {
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    body: String,
    output: Option<ProjectedOutput>,
}

#[derive(Debug)]
struct ProjectedOutput {
    call_id: Option<String>,
    capture_unavailable: bool,
}

enum ExplicitOutputBody {
    Absent,
    Present(String),
    Ambiguous,
}

pub(super) fn project_message(
    message: &serde_json::Value,
    _index: usize,
    document: &PreparedDocument,
) -> std::result::Result<Option<ProjectedMessage>, String> {
    if !message.is_object() {
        return Err(bounded_failure(
            "Rovo Dev message_history member must be an object",
        ));
    }
    let role_text = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| rovodev_string_alias(message, &["kind", "type"]));
    if message.get("role").is_none()
        && (message.get("kind").is_some() || message.get("type").is_some())
        && role_text.is_none()
    {
        return Err(bounded_failure(
            "Rovo Dev message has conflicting kind/type selectors",
        ));
    }
    let role_text = role_text.as_deref();
    let event_type = rovodev_event_type(message, role_text);
    let mut output = None;
    let body;
    if event_type == EventType::ToolOutput {
        let capture_unavailable;
        body = match explicit_output_body(message)? {
            ExplicitOutputBody::Absent => return Ok(None),
            ExplicitOutputBody::Present(body) => {
                capture_unavailable = false;
                body
            }
            ExplicitOutputBody::Ambiguous => {
                capture_unavailable = true;
                serde_json::to_string(message)
                    .map_err(|error| bounded_failure(error.to_string()))?
            }
        };
        let call_id = output_linkage(message)?;
        output = Some(ProjectedOutput {
            call_id,
            capture_unavailable,
        });
    } else {
        body = lexical_body(message, event_type);
    }
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    let role = Some(provider_role_from_message(message, role_text));
    Ok(Some(ProjectedMessage {
        event_type,
        role,
        occurred_at,
        body,
        output,
    }))
}

fn explicit_output_body(
    value: &serde_json::Value,
) -> std::result::Result<ExplicitOutputBody, String> {
    let mut result_parts = Vec::new();
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        for part in parts {
            let kind = rovodev_string_alias(part, &["kind", "type"])
                .map(|value| value.trim().to_ascii_lowercase());
            if (part.get("kind").is_some() || part.get("type").is_some()) && kind.is_none() {
                return Err(bounded_failure(
                    "Rovo Dev result part has conflicting kind/type selectors",
                ));
            }
            if kind.as_deref().is_some_and(|kind| {
                matches!(
                    kind,
                    "tool_result" | "tool-result" | "tool_use_result" | "function_result"
                )
            }) {
                result_parts.push(part);
            }
        }
    }
    let selected = if result_parts.is_empty() {
        vec![value]
    } else {
        result_parts
    };
    let mut bodies = Vec::new();
    for result in selected {
        let candidates = ["content", "result", "output", "text"]
            .into_iter()
            .filter_map(|field| result.get(field))
            .filter(|value| !value.is_null())
            .collect::<Vec<_>>();
        let candidate = match candidates.as_slice() {
            [] => continue,
            [candidate] => *candidate,
            _ => {
                return Ok(ExplicitOutputBody::Ambiguous);
            }
        };
        if let Some(body) =
            provider_explicit_result_value_text(candidate).filter(|body| !body.trim().is_empty())
        {
            bodies.push(body);
        }
    }
    Ok(if bodies.is_empty() {
        ExplicitOutputBody::Absent
    } else {
        ExplicitOutputBody::Present(bodies.join("\n"))
    })
}

fn output_linkage(value: &serde_json::Value) -> std::result::Result<Option<String>, String> {
    fn unique(
        values: impl Iterator<Item = String>,
        label: &str,
    ) -> std::result::Result<Option<String>, String> {
        let mut selected = None;
        for value in values.filter(|value| !value.trim().is_empty()) {
            if value.len() > 4 * 1024 {
                return Err(bounded_failure(format!(
                    "Rovo Dev {label} exceeds the linkage bound"
                )));
            }
            if selected.as_ref().is_some_and(|selected| selected != &value) {
                return Ok(None);
            }
            selected = Some(value);
        }
        Ok(selected)
    }

    let mut result_objects = vec![value];
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        result_objects.extend(parts.iter());
    }
    unique(
        result_objects.iter().flat_map(|value| {
            [
                "tool_use_id",
                "toolUseId",
                "tool_call_id",
                "toolCallId",
                "call_id",
                "callId",
            ]
            .into_iter()
            .filter_map(|field| value.get(field).and_then(serde_json::Value::as_str))
            .map(str::to_owned)
        }),
        "call id",
    )
}

fn known_message_string_field(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    let mut values = value
        .as_object()
        .into_iter()
        .flat_map(|object| fields.iter().filter_map(|field| object.get(*field)))
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        values.extend(parts.iter().flat_map(|part| {
            part.as_object()
                .into_iter()
                .flat_map(|object| fields.iter().filter_map(|field| object.get(*field)))
                .filter_map(serde_json::Value::as_str)
        }));
    }
    let mut selected = None;
    for candidate in values {
        if selected
            .as_ref()
            .is_some_and(|selected| selected != candidate)
        {
            return None;
        }
        selected = Some(candidate.to_owned());
    }
    selected
}

fn known_message_json_alias_capture(
    value: &serde_json::Value,
    fields: &[&str],
) -> ActivityJsonCapture {
    let mut selected = None;
    let mut objects = value.as_object().into_iter().collect::<Vec<_>>();
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        objects.extend(parts.iter().filter_map(serde_json::Value::as_object));
    }
    for object in objects {
        for field in fields {
            let Some(candidate) = object.get(*field).filter(|value| !value.is_null()) else {
                continue;
            };
            if selected.is_some_and(|selected| selected != candidate) {
                return ActivityJsonCapture::Unavailable;
            }
            selected = Some(candidate);
        }
    }
    selected
        .cloned()
        .map_or(ActivityJsonCapture::Absent, |value| {
            ActivityJsonCapture::Present { value }
        })
}

pub(super) fn scan_rovodev_document<L, S>(
    authority: &RovoDevTreeAuthority,
    leaf: &RovoDevDocumentLeaf,
    context: &ProviderAdapterContext,
    source_anchor_scope: SourceAnchorScope,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> SourceBackedRouteResult<DocumentSourceTerminal>
where
    L: crate::CaptureLifecycleSink,
    S: crate::DocumentRecordSpool,
{
    let snapshot = open_leaf(authority, leaf, context).map_err(rovodev_route_error)?;
    let source = authority.source(leaf).map_err(rovodev_route_error)?;
    let bound = authority
        .bind_document(leaf, &snapshot, source_anchor_scope)
        .map_err(rovodev_route_error)?;
    let observation = snapshot
        .observation(bound.source_key.clone())
        .map_err(rovodev_route_error)?;
    sink.begin_source(bound.source_key.clone())?;

    let mut counts = ScannedSourceCounts::default();
    match snapshot.document.as_ref() {
        Err(_) => {
            counts.rejected_records = 1;
        }
        Ok(document) => {
            counts.rejected_records = document.initial_failure_count;
            for (index, raw_message) in document.messages.iter().enumerate() {
                match project_message(raw_message, index, document) {
                    Err(_) => {
                        counts.rejected_records =
                            checked_add(counts.rejected_records, 1).map_err(rovodev_route_error)?;
                    }
                    Ok(None) => {
                        counts.ignored_records =
                            checked_add(counts.ignored_records, 1).map_err(rovodev_route_error)?;
                    }
                    Ok(Some(event)) => {
                        sink.emit_core_record(
                            core_record(
                                &bound,
                                snapshot.source_sha256,
                                document,
                                raw_message,
                                index,
                                event,
                            )
                            .map_err(rovodev_route_error)?,
                        )?;
                        counts.retained_records =
                            checked_add(counts.retained_records, 1).map_err(rovodev_route_error)?;
                        counts.indexed_documents = checked_add(counts.indexed_documents, 1)
                            .map_err(rovodev_route_error)?;
                    }
                }
            }
        }
    }
    counts.complete_records = counts
        .retained_records
        .checked_add(counts.rejected_records)
        .and_then(|count| count.checked_add(counts.ignored_records))
        .ok_or_else(|| rovodev_route_error(RovoDevSourceBackedError::CountMismatch))?;
    counts.certified_bytes = snapshot.certified_bytes;
    source.revalidate_current().map_err(rovodev_route_error)?;
    authority
        .authority
        .revalidate()
        .map_err(|error| rovodev_route_error(error.into()))?;
    Ok(DocumentSourceTerminal {
        source: bound.source_key,
        opening: observation.clone(),
        closing: observation,
        parser_revision: PARSER_REVISION,
        content_digest: snapshot.source_sha256,
        counts,
    })
}

fn open_leaf(
    authority: &RovoDevTreeAuthority,
    leaf: &RovoDevDocumentLeaf,
    context: &ProviderAdapterContext,
) -> RovoDevSourceBackedResult<RovoDevSnapshot> {
    let source = authority.source(leaf)?;
    if source.proof()? != leaf.proof {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    RovoDevSnapshot::read(source, context)
}

fn checked_add(left: u64, right: u64) -> RovoDevSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(RovoDevSourceBackedError::CountMismatch)
}

fn core_record(
    bound: &RovoDevBoundDocument,
    source_revision: [u8; 32],
    document: &PreparedDocument,
    raw_message: &serde_json::Value,
    index: usize,
    event: ProjectedMessage,
) -> RovoDevSourceBackedResult<CoreRecord> {
    let native_item_key = native_item_key(bound, source_revision, raw_message, index)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &bound.source_key,
        session_id: bound.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let message_index =
        u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
    let native_record_id = explicit_message_id(raw_message)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("message-{message_index}"));
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(MESSAGE_OBJECT_KIND)?,
        TypedKey::U64(message_index),
        TypedKey::utf8(&native_record_id)?,
    ])?;
    let body = event.body.clone();
    let branch = exact_provider_string_field(
        &document.metadata,
        &[
            "branch",
            "git_branch",
            "gitBranch",
            "vcs_branch",
            "vcsBranch",
        ],
    )
    .or_else(|| document.context_branch.clone());
    let mut record = CoreRecord::new_selected(
        event_id,
        bound.session_id,
        bound.source_key.clone(),
        message_index,
        event.event_type.as_str(),
        PARSER_REVISION,
        body,
    )?;
    apply_direct_session_relationship(&mut record, bound.parent_session_id)?;
    record.provider_session_id = Some(bound.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.content.structured_content = Some(raw_message.clone());
    let mut facts = Vec::new();
    if let Some(branch) = branch {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::Branch, branch, facts.len())
        {
            facts.push(fact);
        }
    }
    if let Some(cwd) = document.cwd.clone() {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd, facts.len())
        {
            facts.push(fact);
        }
    }
    let projected_output = event.output.as_ref();
    let projected_call_id = projected_output.and_then(|output| output.call_id.clone());
    let known_call_id = known_message_string_field(
        raw_message,
        &[
            "tool_use_id",
            "toolUseId",
            "tool_call_id",
            "toolCallId",
            "call_id",
            "callId",
        ],
    );
    let call_id = exact_owned_string_alias(projected_call_id, known_call_id);
    let provider_call_id = admit_optional_provider_call_id(call_id);
    let invocation = (provider_call_id.is_some() && event.event_type == EventType::ToolCall)
        .then(|| {
            admit_optional_metadata_text(known_message_string_field(
                raw_message,
                &["tool_name", "toolName", "name", "tool"],
            ))
            .map(|tool| ActivityInvocation {
                protocol: None,
                server: None,
                tool,
                arguments: known_message_json_alias_capture(
                    raw_message,
                    &["arguments", "args", "input", "parameters"],
                ),
                started_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            })
        })
        .flatten();
    let result = provider_call_id
        .is_some()
        .then_some(projected_output)
        .flatten()
        .map(|_| ActivityResult {
            status: admit_optional_metadata_text(rovodev_string_alias(
                raw_message,
                &["status", "state", "outcome"],
            )),
            completed_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            duration_ns: None,
            text: if projected_output.is_some_and(|output| output.capture_unavailable) {
                ActivityTextCapture::Unavailable
            } else {
                ActivityTextCapture::NormalizedBody
            },
            structured_content: if projected_output.is_some_and(|output| output.capture_unavailable)
            {
                ActivityJsonCapture::Unavailable
            } else {
                ActivityJsonCapture::Present {
                    value: raw_message.clone(),
                }
            },
        });
    if invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn exact_owned_string_alias(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn rovodev_string_alias(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    let mut selected = None;
    for field in fields {
        let Some(candidate) = object.get(*field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if selected
            .as_ref()
            .is_some_and(|selected| selected != candidate)
        {
            return None;
        }
        selected = Some(candidate.to_owned());
    }
    selected
}

fn native_item_key(
    bound: &RovoDevBoundDocument,
    source_revision: [u8; 32],
    message: &serde_json::Value,
    index: usize,
) -> RovoDevSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = explicit_message_id(message)
        .filter(|native_id| bound.unique_message_ids.contains(*native_id))
    {
        return Ok(NativeItemKey::native_id(
            EVENT_KEY_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    let coordinate = TypedKey::composite(vec![
        explicit_message_id(message)
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(
            u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        ),
    ])?;
    Ok(NativeItemKey::revision_scoped_position(
        EVENT_POSITION_KIND,
        coordinate,
        TypedKey::bytes(source_revision.to_vec())?,
    )?)
}

fn lexical_body(raw_message: &serde_json::Value, event_type: EventType) -> String {
    let text = provider_block_text(raw_message).unwrap_or_default();
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text
    }
}

#[cfg(test)]
#[path = "document/result_tests.rs"]
mod result_tests;
