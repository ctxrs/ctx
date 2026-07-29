use serde_json::Value;

pub(super) fn is_blame_result(value: &Value) -> bool {
    value
        .pointer("/target/kind")
        .and_then(Value::as_str)
        .is_some()
        && value.get("matches").and_then(Value::as_array).is_some()
        && value.get("evidence").and_then(Value::as_array).is_some()
}

pub(super) fn render_blame_text(value: &Value) -> String {
    let mut out = String::from("ctx blame\n");
    if let Some(target) = value.get("target") {
        push_scalar(&mut out, "target.kind", target.get("kind"), "");
        match target.get("kind").and_then(Value::as_str) {
            Some("file") => {
                push_scalar(&mut out, "target.path", target.get("path"), "");
                push_resource(&mut out, "target.repository", target.get("repository"), "");
                if let Some(lines) = target.get("requested_lines") {
                    push_scalar(&mut out, "target.lines.start", lines.get("start"), "");
                    push_scalar(&mut out, "target.lines.end", lines.get("end"), "");
                }
            }
            Some("commit") => {
                push_resource(&mut out, "target.commit", target.get("commit"), "");
                push_resource(&mut out, "target.repository", target.get("repository"), "");
            }
            Some("pull_request") => {
                push_scalar(&mut out, "target.selector", target.get("selector"), "");
                push_resource(
                    &mut out,
                    "target.pull_request",
                    target.get("pull_request"),
                    "",
                );
                push_resource(&mut out, "target.repository", target.get("repository"), "");
            }
            Some(_) | None => {}
        }
    }
    if let Some(snapshot) = value.get("git_snapshot") {
        push_scalar(
            &mut out,
            "git_snapshot.head_oid",
            snapshot.get("head_oid"),
            "",
        );
        push_scalar(
            &mut out,
            "git_snapshot.worktree_status",
            snapshot.get("worktree_status"),
            "",
        );
    }

    let matches = array(value, "matches");
    out.push_str(&format!("matches: {}\n", matches.len()));
    for (index, value) in matches.iter().enumerate() {
        render_match(&mut out, index + 1, value);
    }

    let evidence = array(value, "evidence");
    out.push_str(&format!("\nevidence: {}\n", evidence.len()));
    for item in evidence {
        render_evidence(&mut out, item);
    }

    if let Some(next) = value.get("next") {
        push_scalar(&mut out, "next.reason", next.get("reason"), "");
        push_scalar(&mut out, "next.cursor", next.get("cursor"), "");
    }
    out
}

fn render_match(out: &mut String, index: usize, value: &Value) {
    out.push_str(&format!("\nmatch {index}\n"));
    push_scalar(out, "kind", value.get("kind"), "  ");
    let body = value.get("value").unwrap_or(value);
    match value.get("kind").and_then(Value::as_str) {
        Some("file") => render_file_match(out, body),
        Some("commit") => render_commit_match(out, body),
        Some("pull_request") => render_pull_request_match(out, body),
        Some(_) | None => {}
    }
}

fn render_file_match(out: &mut String, value: &Value) {
    push_scalar(out, "id", value.get("id"), "  ");
    if let Some(lines) = value.get("lines") {
        push_scalar(out, "lines.start", lines.get("start"), "  ");
        push_scalar(out, "lines.end", lines.get("end"), "  ");
    }
    push_resource(out, "commit", value.get("commit"), "  ");
    push_number_list(
        out,
        "line_evidence_numbers",
        value.get("line_evidence_numbers"),
        "  ",
    );
    for (index, attribution) in array(value, "production").iter().enumerate() {
        out.push_str(&format!("  production {}\n", index + 1));
        render_attribution(out, attribution, "    ");
    }
}

fn render_commit_match(out: &mut String, value: &Value) {
    for field in [
        "fact_id",
        "fact_type",
        "predicate",
        "fact_occurred_at_ms",
        "confidence",
        "state",
    ] {
        push_scalar(out, field, value.get(field), "  ");
    }
    push_resource(out, "subject", value.get("subject"), "  ");
    push_resource(out, "object", value.get("object"), "  ");
    push_resource(out, "direct_actor", value.get("direct_actor"), "  ");
    push_resource(out, "owning_root", value.get("owning_root"), "  ");
    push_number_list(out, "evidence_numbers", value.get("evidence_numbers"), "  ");
}

fn render_pull_request_match(out: &mut String, value: &Value) {
    push_resource(out, "pull_request", value.get("pull_request"), "  ");
    let Some(relationship) = value.get("relationship") else {
        return;
    };
    push_scalar(out, "relationship.kind", relationship.get("kind"), "  ");
    let body = relationship.get("value").unwrap_or(relationship);
    match relationship.get("kind").and_then(Value::as_str) {
        Some("activity") => {
            for field in [
                "fact_id",
                "action",
                "fact_occurred_at_ms",
                "confidence",
                "state",
            ] {
                push_scalar(out, field, body.get(field), "  ");
            }
            push_resource(out, "session", body.get("session"), "  ");
            push_resource(out, "direct_actor", body.get("direct_actor"), "  ");
            push_resource(out, "owning_root", body.get("owning_root"), "  ");
            push_number_list(out, "evidence_numbers", body.get("evidence_numbers"), "  ");
        }
        Some("commit") => {
            push_scalar(out, "fact_id", body.get("fact_id"), "  ");
            push_scalar(out, "relationship", body.get("relationship"), "  ");
            push_resource(out, "commit", body.get("commit"), "  ");
            push_number_list(out, "evidence_numbers", body.get("evidence_numbers"), "  ");
            for (index, attribution) in array(body, "production").iter().enumerate() {
                out.push_str(&format!("  production {}\n", index + 1));
                render_attribution(out, attribution, "    ");
            }
        }
        Some(_) | None => {}
    }
}

fn render_attribution(out: &mut String, value: &Value, indent: &str) {
    push_scalar(out, "id", value.get("id"), indent);
    push_scalar(out, "relationship", value.get("relationship"), indent);
    push_resource(
        out,
        "producing_session",
        value.get("producing_session"),
        indent,
    );
    push_resource(out, "direct_actor", value.get("direct_actor"), indent);
    push_resource(out, "owning_root", value.get("owning_root"), indent);
    push_scalar(out, "confidence", value.get("confidence"), indent);
    push_scalar(out, "state", value.get("state"), indent);
    push_number_list(
        out,
        "evidence_numbers",
        value.get("evidence_numbers"),
        indent,
    );
}

fn render_evidence(out: &mut String, value: &Value) {
    let number = value
        .get("number")
        .and_then(Value::as_u64)
        .map(|number| number.to_string())
        .unwrap_or_else(|| "?".to_owned());
    out.push_str(&format!("evidence {number}\n"));
    let Some(citation) = value.get("citation") else {
        return;
    };
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
        push_scalar(out, field, citation.get(field), "  ");
    }
    if let Some(range) = citation.get("byte_range") {
        push_scalar(out, "byte_range.start", range.get("start"), "  ");
        push_scalar(
            out,
            "byte_range.end_exclusive",
            range.get("end_exclusive"),
            "  ",
        );
    }
}

fn push_resource(out: &mut String, label: &str, value: Option<&Value>, indent: &str) {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return;
    };
    for field in ["id", "kind", "display"] {
        push_scalar(out, &format!("{label}.{field}"), value.get(field), indent);
    }
}

fn push_number_list(out: &mut String, label: &str, value: Option<&Value>, indent: &str) {
    let Some(values) = value.and_then(Value::as_array) else {
        return;
    };
    let rendered = values
        .iter()
        .filter_map(Value::as_u64)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    out.push_str(&format!("{indent}{label}: {rendered}\n"));
}

fn push_scalar(out: &mut String, label: &str, value: Option<&Value>, indent: &str) {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return;
    };
    let rendered = match value {
        Value::String(value) => escape_controls(value),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value)
            .unwrap_or_else(|_| "[unrenderable structured value]".to_owned()),
        Value::Null => return,
    };
    out.push_str(&format!("{indent}{label}: {rendered}\n"));
}

fn escape_controls(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fallback_contains_every_match_and_evidence_without_payload_labels() {
        let value = json!({
            "target": {
                "kind": "commit",
                "commit": {"id": "commit:abc", "kind": "commit", "display": "abc"},
                "repository": {"id": "repo:ctx", "kind": "repository", "display": "ctxrs/ctx"}
            },
            "git_snapshot": null,
            "matches": [{
                "kind": "commit",
                "value": {
                    "fact_id": "fact:1",
                    "fact_type": "git.commit.produced",
                    "predicate": "produced_by",
                    "subject": {"id": "commit:abc", "kind": "commit", "display": "abc"},
                    "object": {"id": "session:full", "kind": "session", "display": "session:full"},
                    "fact_occurred_at_ms": null,
                    "confidence": "explicit",
                    "state": "asserted",
                    "direct_actor": null,
                    "owning_root": null,
                    "evidence_numbers": [1]
                }
            }],
            "evidence": [{
                "number": 1,
                "citation": {"event_id": "33333333-3333-4333-8333-333333333333"}
            }],
            "next": null
        });
        let rendered = render_blame_text(&value);
        assert!(rendered.contains("matches: 1"));
        assert!(rendered.contains("object.display: session:full"));
        assert!(rendered.contains("event_id: 33333333-3333-4333-8333-333333333333"));
        assert!(!rendered.contains("payload_type"));
        assert!(!rendered.contains("omitted"));
    }

}
