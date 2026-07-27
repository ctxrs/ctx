use serde_json::Value;

use super::{clip_chars, clip_inline, push_omitted_line, value_field};

const MAX_RESULTS: usize = 8;
const MAX_FACTS: usize = 12;
const MAX_CITATIONS: usize = 12;
const MAX_COMMANDS: usize = 4;
const MAX_TEXT_CHARS: usize = 320;

#[derive(Clone, Copy)]
pub(super) enum ProTextKind {
    Resource,
    Location,
    Blame,
    Timeline,
    Related,
    Facts,
}

impl ProTextKind {
    pub(super) fn from_payload_type(payload_type: &str) -> Option<Self> {
        match payload_type {
            "pro_resource" => Some(Self::Resource),
            "pro_location" => Some(Self::Location),
            "pro_blame" => Some(Self::Blame),
            "pro_timeline" => Some(Self::Timeline),
            "pro_related" => Some(Self::Related),
            "pro_facts" => Some(Self::Facts),
            _ => None,
        }
    }

    const fn payload_type(self) -> &'static str {
        match self {
            Self::Resource => "pro_resource",
            Self::Location => "pro_location",
            Self::Blame => "pro_blame",
            Self::Timeline => "pro_timeline",
            Self::Related => "pro_related",
            Self::Facts => "pro_facts",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Resource => "ctx show resource",
            Self::Location => "ctx locate resource",
            Self::Blame => "ctx blame",
            Self::Timeline => "ctx timeline",
            Self::Related => "ctx related",
            Self::Facts => "ctx facts",
        }
    }
}

pub(super) fn render_pro_text(value: &Value, kind: ProTextKind) -> String {
    let records = array(value, "results");
    let flattened_citations = array(value, "citations");
    let commands = array(value, "suggested_next_commands");
    let mut out = format!("{}\npayload_type: {}\n", kind.title(), kind.payload_type());
    push_scalar(&mut out, "schema_version", value.get("schema_version"), "");

    if let Some(target) = value.get("target") {
        push_resource_fields(&mut out, "target", target, "");
        push_scalar(&mut out, "target.repository", target.get("repository"), "");
        push_scalar(&mut out, "target.line", target.get("line"), "");
    }
    push_scalar(&mut out, "stale", value.get("stale"), "");
    out.push_str(&format!("results: {}\n", records.len()));
    out.push_str(&format!("citations: {}\n", flattened_citations.len()));
    push_pagination(&mut out, value.get("pagination"));

    for (index, record) in records.iter().take(MAX_RESULTS).enumerate() {
        push_record(&mut out, index + 1, record);
    }
    push_omitted_line(&mut out, records.len(), MAX_RESULTS, "results");

    if !commands.is_empty() {
        out.push_str(&format!("\nsuggested_next_commands: {}\n", commands.len()));
        for (index, command) in commands.iter().take(MAX_COMMANDS).enumerate() {
            push_scalar(
                &mut out,
                &format!("{}. command", index + 1),
                Some(command),
                "",
            );
        }
        push_omitted_line(&mut out, commands.len(), MAX_COMMANDS, "commands");
    }
    out
}

pub(super) fn render_unknown_pro_text(value: &Value) -> String {
    let payload_type = value_field(value, "payload_type").unwrap_or_else(|| "unknown".to_owned());
    format!(
        "ctx pro result\npayload_type: {}\nerror_code: unsupported_payload_type\nstatus: not_rendered\n",
        clip_inline(&payload_type, MAX_TEXT_CHARS)
    )
}

fn push_pagination(out: &mut String, pagination: Option<&Value>) {
    let Some(pagination) = pagination else {
        return;
    };
    push_scalar(out, "pagination.truncated", pagination.get("truncated"), "");
    push_scalar(
        out,
        "pagination.next_cursor",
        pagination.get("next_cursor"),
        "",
    );
}

fn push_record(out: &mut String, index: usize, record: &Value) {
    let display = record
        .get("resource")
        .and_then(|resource| value_field(resource, "display"))
        .unwrap_or_else(|| "resource".to_owned());
    out.push_str(&format!(
        "\n{}. {}\n",
        index,
        clip_inline(&display, MAX_TEXT_CHARS)
    ));
    if let Some(resource) = record.get("resource") {
        push_resource_fields(out, "resource", resource, "   ");
    }
    push_scalar(out, "summary", record.get("summary"), "   ");
    push_scalar(out, "occurred_at_ms", record.get("occurred_at_ms"), "   ");

    let facts = array(record, "facts");
    let citations = array(record, "citations");
    out.push_str(&format!("   facts: {}\n", facts.len()));
    out.push_str(&format!("   citations: {}\n", citations.len()));
    for (fact_index, fact) in facts.iter().take(MAX_FACTS).enumerate() {
        push_fact(out, fact_index + 1, fact);
    }
    push_omitted_line_indented(out, facts.len(), MAX_FACTS, "facts", "   ");
    for (citation_index, citation) in citations.iter().take(MAX_CITATIONS).enumerate() {
        push_citation(out, citation_index + 1, citation, "record", "   ");
    }
    push_omitted_line_indented(out, citations.len(), MAX_CITATIONS, "citations", "   ");
}

fn push_fact(out: &mut String, index: usize, fact: &Value) {
    out.push_str(&format!("\n   fact {index}\n"));
    for field in ["id", "fact_type"] {
        push_scalar(out, field, fact.get(field), "      ");
    }
    if let Some(subject) = fact.get("subject") {
        push_resource_fields(out, "subject", subject, "      ");
    }
    push_scalar(out, "predicate", fact.get("predicate"), "      ");
    if let Some(object) = fact.get("object") {
        push_fact_object(out, object);
    }
    for field in [
        "confidence",
        "state",
        "detector_version",
        "owning_root_session_id",
        "direct_actor_session_id",
    ] {
        push_scalar(out, field, fact.get(field), "      ");
    }

    let citations = array(fact, "citations");
    out.push_str(&format!("      citations: {}\n", citations.len()));
    for (citation_index, citation) in citations.iter().take(MAX_CITATIONS).enumerate() {
        push_citation(out, citation_index + 1, citation, "fact", "      ");
    }
    push_omitted_line_indented(out, citations.len(), MAX_CITATIONS, "citations", "      ");
}

fn push_fact_object(out: &mut String, object: &Value) {
    push_scalar(out, "object.type", object.get("type"), "      ");
    let Some(value) = object.get("value") else {
        return;
    };
    if object.get("type").and_then(Value::as_str) == Some("resource") {
        push_resource_fields(out, "object.value", value, "      ");
    } else {
        push_scalar(out, "object.value", Some(value), "      ");
    }
}

fn push_resource_fields(out: &mut String, label: &str, resource: &Value, indent: &str) {
    for field in ["id", "kind", "display"] {
        push_scalar(
            out,
            &format!("{label}.{field}"),
            resource.get(field),
            indent,
        );
    }
    if label == "target" {
        push_scalar(out, "target.value", resource.get("value"), indent);
    }
}

fn push_citation(out: &mut String, index: usize, citation: &Value, scope: &str, indent: &str) {
    out.push_str(&format!("{indent}{scope}_citation {index}\n"));
    let field_indent = format!("{indent}   ");
    for field in [
        "observation_id",
        "observation_seq",
        "observation_kind",
        "session_id",
        "event_id",
        "event_seq",
        "source_path",
        "fixture_line",
        "source_record_ordinal",
        "source_record_subrecord_index",
        "source_sha256",
    ] {
        push_scalar(out, field, citation.get(field), &field_indent);
    }
    if let Some(range) = citation.get("byte_range") {
        push_scalar(out, "byte_range.start", range.get("start"), &field_indent);
        push_scalar(
            out,
            "byte_range.end_exclusive",
            range.get("end_exclusive"),
            &field_indent,
        );
    }
}

fn push_scalar(out: &mut String, label: &str, value: Option<&Value>, indent: &str) {
    let Some(value) = value else {
        return;
    };
    let text = match value {
        Value::Null => return,
        Value::String(value) => single_line_text(value),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value)
            .unwrap_or_else(|_| "[unrenderable structured value]".to_owned()),
    };
    out.push_str(&format!(
        "{indent}{label}: {}\n",
        clip_chars(&text, MAX_TEXT_CHARS)
    ));
}

fn single_line_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => escaped.extend(character.escape_unicode()),
            character => escaped.push(character),
        }
    }
    escaped
}

fn array<'a>(value: &'a Value, field: &str) -> &'a [Value] {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn push_omitted_line_indented(
    out: &mut String,
    total: usize,
    shown: usize,
    noun: &str,
    indent: &str,
) {
    if total > shown {
        out.push_str(&format!(
            "{indent}... {} more {noun} omitted from text\n",
            total - shown
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn query_payload(payload_type: &str) -> Value {
        let canonical_citation = json!({
            "observation_id": "11111111-1111-4111-8111-111111111111",
            "observation_seq": 9,
            "observation_kind": "vcs_change",
            "session_id": "22222222-2222-4222-8222-222222222222",
            "event_id": "33333333-3333-4333-8333-333333333333",
            "event_seq": 4,
            "source_path": "/history/session.jsonl",
            "fixture_line": 7,
            "source_record_ordinal": 8,
            "source_record_subrecord_index": 2,
            "byte_range": {"start": 10, "end_exclusive": 42},
            "source_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });
        json!({
            "schema_version": 1,
            "payload_type": payload_type,
            "target": {
                "kind": "file",
                "value": "src/lib.rs",
                "repository": "ctxrs/ctx",
                "line": 42
            },
            "results": [{
                "resource": {"id": "file:1", "kind": "file", "display": "src/lib.rs:42"},
                "summary": "Line provenance",
                "occurred_at_ms": 1770000000000_i64,
                "facts": [{
                    "id": "fact:1",
                    "fact_type": "vcs_commit",
                    "subject": {"id": "session:1", "kind": "session", "display": "agent session"},
                    "predicate": "produced_commit",
                    "object": {
                        "type": "resource",
                        "value": {"id": "commit:abc", "kind": "commit", "display": "abc123"}
                    },
                    "confidence": "explicit",
                    "state": "asserted",
                    "detector_version": "commit-v1",
                    "owning_root_session_id": "44444444-4444-4444-8444-444444444444",
                    "direct_actor_session_id": "55555555-5555-4555-8555-555555555555",
                    "citations": [canonical_citation.clone()]
                }],
                "citations": [canonical_citation.clone()]
            }],
            "citations": [canonical_citation.clone(), canonical_citation],
            "pagination": {"next_cursor": "cursor-2", "truncated": true},
            "stale": false,
            "suggested_next_commands": [
                "ctx facts file src/lib.rs --repository ctxrs/ctx --line 42",
                "ctx timeline file src/lib.rs --repository ctxrs/ctx --line 42"
            ]
        })
    }

    fn empty_query_payload(payload_type: &str) -> Value {
        json!({
            "schema_version": 1,
            "payload_type": payload_type,
            "target": {"kind": "commit", "value": "abc123"},
            "results": [],
            "citations": [],
            "pagination": {"next_cursor": null, "truncated": false},
            "stale": false,
            "suggested_next_commands": []
        })
    }

    #[test]
    fn every_pro_query_payload_has_a_distinct_exact_golden() {
        for (payload_type, title) in [
            ("pro_resource", "ctx show resource"),
            ("pro_location", "ctx locate resource"),
            ("pro_blame", "ctx blame"),
            ("pro_timeline", "ctx timeline"),
            ("pro_related", "ctx related"),
            ("pro_facts", "ctx facts"),
        ] {
            let kind = ProTextKind::from_payload_type(payload_type).expect("known payload type");
            assert_eq!(
                render_pro_text(&empty_query_payload(payload_type), kind),
                format!(
                    "{title}\npayload_type: {payload_type}\nschema_version: 1\ntarget.kind: commit\ntarget.value: abc123\nstale: false\nresults: 0\ncitations: 0\npagination.truncated: false\n"
                )
            );
        }
    }

    #[test]
    fn pro_golden_preserves_graph_semantics_citations_and_pagination() {
        let rendered = render_pro_text(&query_payload("pro_facts"), ProTextKind::Facts);
        assert_eq!(
            rendered,
            include_str!("../../../testdata/mcp/pro_facts.golden.txt")
        );
    }

    #[test]
    fn unknown_pro_query_payload_fails_closed() {
        let value = json!({"payload_type": "pro_future", "results": [{"title": "wrong"}]});
        assert_eq!(
            render_unknown_pro_text(&value),
            "ctx pro result\npayload_type: pro_future\nerror_code: unsupported_payload_type\nstatus: not_rendered\n"
        );
    }

    #[test]
    fn graph_values_escape_controls_without_collapsing_meaningful_spaces() {
        let mut rendered = String::new();
        push_scalar(
            &mut rendered,
            "source_path",
            Some(&json!("/repo/two  spaces\\literal\nnext")),
            "",
        );
        assert_eq!(
            rendered,
            "source_path: /repo/two  spaces\\\\literal\\nnext\n"
        );
    }
}
