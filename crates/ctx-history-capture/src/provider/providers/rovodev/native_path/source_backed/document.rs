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
}

impl ContextHeaderProbe {
    fn provider_session_id(&self) -> Option<String> {
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
                            let value = map.next_value()?;
                            if field == "session_id" {
                                header.session_id = value;
                            } else {
                                header.camel_session_id = value;
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
    ) -> RovoDevSourceBackedResult<RovoDevBoundDocument> {
        let source = self.source(leaf)?;
        let header = document_header_from_snapshot(source, snapshot)?;
        if header != leaf.header {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let parent_session_id = header
            .parent_provider_session_id
            .as_deref()
            .map(provider_thread_session_identity)
            .transpose()?;
        Ok(RovoDevBoundDocument {
            source_key: header.source_key,
            provider_session_id: header.provider_session_id,
            session_id: header.session_id,
            parent_session_id,
            root_session_id: leaf.root_session_id,
            unique_message_ids: unique_message_ids(snapshot),
        })
    }
}

fn document_header_from_snapshot(
    source: &RovoDevOpenedSource,
    snapshot: &RovoDevSnapshot,
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
    document_header(provider_session_id, parent_provider_session_id)
}

fn document_header(
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
) -> RovoDevSourceBackedResult<RovoDevDocumentHeader> {
    let source_key = rovodev_source_key(&provider_session_id)?;
    let session_id = rovodev_session_identity(&source_key, &provider_session_id)?;
    Ok(RovoDevDocumentHeader {
        source_key,
        provider_session_id,
        parent_provider_session_id,
        session_id,
    })
}

fn probe_document_header(
    source: &RovoDevOpenedSource,
) -> RovoDevSourceBackedResult<RovoDevDocumentHeader> {
    let files = source.open_files()?;
    let fallback = || document_header(source.source.provider_session_id.clone(), None);
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
                provider_string_field(&context_json, &["session_id", "sessionId"])
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
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .filter(|value| validate_json_bounds(value).is_ok())
                .unwrap_or(serde_json::Value::Null)
        }
        (None, None) => serde_json::Value::Null,
        _ => return Err(CaptureError::SourceChangedDuringCapture.into()),
    };
    let provider_session_id = provider_string_field(&metadata, &["session_id", "sessionId"])
        .or(context_provider_session_id)
        .unwrap_or_else(|| source.source.provider_session_id.clone());
    let parent_provider_session_id = provider_string_field(
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
    document_header(provider_session_id, parent_provider_session_id)
}

fn register_document_header(
    lineage: &mut RovoDevLineageCache,
    source_index: usize,
    header: RovoDevDocumentHeader,
) -> RovoDevSourceBackedResult<()> {
    let selected = lineage
        .headers
        .get_mut(source_index)
        .ok_or(RovoDevSourceBackedError::CountMismatch)?;
    if let Some(existing) = selected.as_ref() {
        if existing == &header {
            return Ok(());
        }
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    let source_digest = header.source_key.identity().digest();
    if let Some(existing) = lineage.source_owners.insert(source_digest, source_index) {
        if existing != source_index {
            return Err(RovoDevSourceBackedError::DuplicateSession(
                header.provider_session_id,
            ));
        }
    }
    *selected = Some(header);
    Ok(())
}

pub(super) fn ensure_document_header(
    lineage: &mut RovoDevLineageCache,
    sources: &[RovoDevOpenedSource],
    source_index: usize,
) -> RovoDevSourceBackedResult<RovoDevDocumentHeader> {
    if let Some(header) = lineage.headers.get(source_index).and_then(Option::as_ref) {
        return Ok(header.clone());
    }
    let source = sources
        .get(source_index)
        .ok_or(RovoDevSourceBackedError::CountMismatch)?;
    let header = probe_document_header(source)?;
    register_document_header(lineage, source_index, header.clone())?;
    Ok(header)
}

fn find_document_header(
    lineage: &mut RovoDevLineageCache,
    sources: &[RovoDevOpenedSource],
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<Option<RovoDevDocumentHeader>> {
    let source = rovodev_source_key(provider_session_id)?;
    if let Some(index) = lineage
        .source_owners
        .get(&source.identity().digest())
        .copied()
    {
        return lineage
            .headers
            .get(index)
            .and_then(Option::as_ref)
            .cloned()
            .map(Some)
            .ok_or(RovoDevSourceBackedError::CountMismatch);
    }

    let directory_candidates = lineage
        .directory_owners
        .get(provider_session_id)
        .cloned()
        .unwrap_or_default();
    for index in directory_candidates {
        if lineage.headers.get(index).is_some_and(Option::is_none) {
            let header = ensure_document_header(lineage, sources, index)?;
            if header.provider_session_id == provider_session_id {
                return Ok(Some(header));
            }
        }
    }
    while lineage.next_unprobed < sources.len() {
        let index = lineage.next_unprobed;
        lineage.next_unprobed = lineage.next_unprobed.saturating_add(1);
        let header = ensure_document_header(lineage, sources, index)?;
        if header.provider_session_id == provider_session_id {
            return Ok(Some(header));
        }
    }
    Ok(None)
}

pub(super) fn resolve_root_session(
    lineage: &mut RovoDevLineageCache,
    sources: &[RovoDevOpenedSource],
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<StableEntityId> {
    if let Some(root) = lineage.roots.get(provider_session_id).copied() {
        return Ok(root);
    }
    let mut path = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = provider_session_id.to_owned();
    let root = loop {
        if let Some(root) = lineage.roots.get(&cursor).copied() {
            break root;
        }
        if !visited.insert(cursor.clone()) {
            return Err(RovoDevSourceBackedError::LineageCycle(cursor));
        }
        path.push(cursor.clone());
        let Some(header) = find_document_header(lineage, sources, &cursor)? else {
            break provider_thread_session_identity(&cursor)?;
        };
        let Some(parent) = header.parent_provider_session_id else {
            break header.session_id;
        };
        cursor = parent;
    };
    for session in path {
        lineage.roots.insert(session, root);
    }
    Ok(root)
}

#[derive(Debug)]
struct ProjectedMessage {
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    body: String,
    output: Option<ProjectedOutput>,
    touched_files: Vec<String>,
    touch_limit_exceeded: bool,
}

#[derive(Debug)]
struct ProjectedOutput {
    outcome: OutputOutcome,
    call_id: Option<String>,
    tool_name: Option<String>,
}

fn project_message(
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
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(serde_json::Value::as_str);
    let mut event_type = rovodev_event_type(message, role_text);
    let mut output = None;
    let body;
    if event_type == EventType::ToolOutput {
        let outcome = output_outcome(message);
        let Some(selected_body) = explicit_output_body(message)? else {
            return Ok(None);
        };
        body = selected_body;
        if output_kind(message) == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        let (call_id, tool_name) = output_linkage(message)?;
        output = Some(ProjectedOutput {
            outcome,
            call_id,
            tool_name,
        });
    } else {
        body = lexical_body(message, event_type);
    }
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    let role = Some(provider_role_from_message(message, role_text));
    let mut touched_files = Vec::new();
    let include_structured = event_type_supports_structured_file_touches(event_type);
    let outcome = visit_provider_file_touch_drafts_with_limit(
        message,
        include_structured,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| {
            touched_files.push(touch.path);
            Ok::<(), CaptureError>(())
        },
    )
    .map_err(|error| bounded_failure(error.to_string()))?;
    Ok(Some(ProjectedMessage {
        event_type,
        role,
        occurred_at,
        body,
        output,
        touched_files,
        touch_limit_exceeded: outcome.limit_exceeded(),
    }))
}

fn explicit_output_body(value: &serde_json::Value) -> std::result::Result<Option<String>, String> {
    let mut result_parts = Vec::new();
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        for part in parts {
            let kind = part
                .get("kind")
                .or_else(|| part.get("type"))
                .and_then(serde_json::Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase());
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
                return Err(bounded_failure(
                    "Rovo Dev tool result exposes more than one candidate body field",
                ));
            }
        };
        if let Some(body) =
            provider_explicit_result_value_text(candidate).filter(|body| !body.trim().is_empty())
        {
            bodies.push(body);
        }
    }
    Ok((!bodies.is_empty()).then(|| bodies.join("\n")))
}

fn output_linkage(
    value: &serde_json::Value,
) -> std::result::Result<(Option<String>, Option<String>), String> {
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
                return Err(bounded_failure(format!(
                    "Rovo Dev tool result has ambiguous {label}"
                )));
            }
            selected = Some(value);
        }
        Ok(selected)
    }

    let mut result_objects = vec![value];
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        result_objects.extend(parts.iter());
    }
    let call_id = unique(
        result_objects.iter().filter_map(|value| {
            [
                "tool_use_id",
                "toolUseId",
                "tool_call_id",
                "toolCallId",
                "call_id",
                "callId",
            ]
            .into_iter()
            .find_map(|field| value.get(field).and_then(serde_json::Value::as_str))
            .map(str::to_owned)
        }),
        "call id",
    )?;
    let tool_name = unique(
        result_objects.iter().filter_map(|value| {
            ["tool_name", "toolName", "name", "tool"]
                .into_iter()
                .find_map(|field| value.get(field).and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        }),
        "tool name",
    )?;
    Ok((call_id, tool_name))
}

fn output_kind(value: &serde_json::Value) -> OutputObservationKind {
    let tool_name = recursive_string_field(value, &["tool_name", "toolName", "name", "tool"])
        .unwrap_or_else(|| "tool".to_owned());
    if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    }
}

fn output_outcome(value: &serde_json::Value) -> OutputOutcome {
    if value_timed_out(value) {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    }
}

fn recursive_string_field(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| recursive_string_field(value, fields)),
        serde_json::Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(serde_json::Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| recursive_string_field(value, fields))
            }),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => None,
    }
}

fn value_timed_out(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(value_timed_out),
        serde_json::Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

pub(super) fn scan_rovodev_document(
    authority: &RovoDevTreeAuthority,
    leaf: &RovoDevDocumentLeaf,
    context: &ProviderAdapterContext,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> SourceBackedRouteResult<DocumentSourceTerminal> {
    let snapshot = open_leaf(authority, leaf, context).map_err(rovodev_route_error)?;
    let source = authority.source(leaf).map_err(rovodev_route_error)?;
    let bound = authority
        .bind_document(leaf, &snapshot)
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
                        if event.touch_limit_exceeded {
                            counts.rejected_records = checked_add(counts.rejected_records, 1)
                                .map_err(rovodev_route_error)?;
                        }
                        sink.emit_core_record(
                            core_record(&bound, &snapshot, document, raw_message, index, event)
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
    snapshot: &RovoDevSnapshot,
    document: &PreparedDocument,
    raw_message: &serde_json::Value,
    index: usize,
    event: ProjectedMessage,
) -> RovoDevSourceBackedResult<CoreRecord> {
    let native_item_key = native_item_key(bound, snapshot, raw_message, index)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &bound.source_key,
        session_id: bound.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let message_index =
        u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
    let native_record_id = provider_message_id(raw_message, message_index);
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(MESSAGE_OBJECT_KIND)?,
        TypedKey::U64(message_index),
        TypedKey::utf8(&native_record_id)?,
    ])?;
    let body = event.body.clone();
    let branch = provider_string_field(
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
    let native_file_touches =
        (!event.touched_files.is_empty()).then(|| serde_json::json!(&event.touched_files));
    let native_tool = matches!(
        event.event_type,
        EventType::ToolCall | EventType::ToolOutput | EventType::CommandOutput
    )
    .then(|| {
        let projected_output = event.output.as_ref();
        serde_json::json!({
            "name": projected_output.and_then(|output| output.tool_name.as_deref())
                .map(str::to_owned)
                .or_else(|| projected_output.is_none().then(|| recursive_string_field(raw_message, &["tool_name", "toolName", "name", "tool"])).flatten()),
            "call_id": projected_output.and_then(|output| output.call_id.as_deref())
                .map(str::to_owned)
                .or_else(|| projected_output.is_none().then(|| recursive_string_field(raw_message, &["tool_call_id", "toolCallId", "call_id", "callId"])).flatten()),
            "arguments": projected_output.is_none().then(|| raw_message.get("arguments").or_else(|| raw_message.get("input"))).flatten(),
            "result_outcome": projected_output.map(|output| match output.outcome {
                OutputOutcome::Success => "success",
                OutputOutcome::Failure => "failure",
                OutputOutcome::Timeout => "timeout",
                OutputOutcome::Unknown => "unknown",
            }),
        })
    });
    let is_primary = document.parent_provider_session_id.is_none();
    let agent_type = if is_primary {
        AgentType::Primary
    } else {
        AgentType::Subagent
    };
    let mut record = CoreRecord::new_selected(
        event_id,
        bound.session_id,
        bound.root_session_id,
        bound.source_key.clone(),
        message_index,
        event.event_type.as_str(),
        agent_type.as_str(),
        is_primary,
        PARSER_REVISION,
        body,
    )?;
    record.parent_session_id = bound.parent_session_id;
    record.provider_session_id = Some(bound.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.branch = branch;
    record.workspace = document.cwd.clone();
    record.cwd = document.cwd.clone();
    if let Some(native_file_touches) = native_file_touches {
        record.metadata.insert(
            "provider_native_file_touches".to_owned(),
            native_file_touches,
        );
    }
    if let Some(native_tool) = native_tool {
        record.content.structured_content = Some(serde_json::json!({
            "provider_native_tool": native_tool,
        }));
    }
    record.validate_contract()?;
    Ok(record)
}

fn native_item_key(
    bound: &RovoDevBoundDocument,
    snapshot: &RovoDevSnapshot,
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
        TypedKey::bytes(snapshot.source_sha256.to_vec())?,
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
mod result_tests {
    use super::*;

    fn document() -> PreparedDocument {
        PreparedDocument {
            metadata: serde_json::Value::Null,
            context_branch: None,
            messages: Vec::new(),
            provider_session_id: "session".to_owned(),
            parent_provider_session_id: None,
            started_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            cwd: None,
            initial_failure_count: 0,
        }
    }

    #[test]
    fn typed_tool_results_keep_success_failure_unknown_and_large_bodies() {
        for (status, expected) in [
            (Some("success"), OutputOutcome::Success),
            (Some("failure"), OutputOutcome::Failure),
            (None, OutputOutcome::Unknown),
        ] {
            let mut part = serde_json::json!({
                "kind": "tool_result",
                "tool_use_id": "call-1",
                "content": format!("complete-{expected:?}"),
            });
            if let Some(status) = status {
                part["status"] = serde_json::json!(status);
            }
            let message = serde_json::json!({"role": "tool", "parts": [part]});
            let projected = project_message(&message, 0, &document()).unwrap().unwrap();
            assert_eq!(projected.body, format!("complete-{expected:?}"));
            let output = projected.output.unwrap();
            assert_eq!(output.outcome, expected);
            assert_eq!(output.call_id.as_deref(), Some("call-1"));
        }

        let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
        let message = serde_json::json!({
            "role": "tool",
            "parts": [{"kind": "tool_result", "content": large}],
        });
        assert!(serde_json::to_vec(&message).unwrap().len() > 8 * 1024 * 1024);
        let projected = project_message(&message, 0, &document()).unwrap().unwrap();
        assert_eq!(projected.body.len(), 9 * 1024 * 1024 + 4);
        assert!(projected.body.ends_with("tail"));

        let ambiguous = serde_json::json!({
            "role": "tool",
            "parts": [{
                "kind": "tool_result",
                "content": "one",
                "output": "two",
            }],
        });
        assert!(project_message(&ambiguous, 0, &document()).is_err());
    }
}
