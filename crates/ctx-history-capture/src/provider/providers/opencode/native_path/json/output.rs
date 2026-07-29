use super::*;

pub(super) fn project_output(body: &Value, effective_type: &str) -> Vec<u8> {
    let aggregate = collect_outcome(body);
    encode_output(OutputWire {
        diagnostic: output_diagnostic(body, effective_type, &aggregate),
    })
}

fn output_diagnostic(
    body: &Value,
    effective_type: &str,
    aggregate: &OutcomeAggregate,
) -> Option<RetainedWire> {
    if !matches!(aggregate.outcome(), 2 | 3) {
        return None;
    }
    let mut diagnostic = Map::new();
    diagnostic.insert("type".to_owned(), Value::String(effective_type.to_owned()));
    diagnostic.insert("role".to_owned(), Value::String("tool".to_owned()));
    diagnostic.insert(
        "result_outcome".to_owned(),
        Value::String("failure".to_owned()),
    );
    diagnostic.insert(
        "timed_out".to_owned(),
        Value::Bool(aggregate.outcome() == 3),
    );
    if let Some(exit_code) = aggregate.exit_code {
        diagnostic.insert("exit_code".to_owned(), Value::from(exit_code));
    }
    if let Some(duration_ms) = aggregate.duration_ms {
        diagnostic.insert("duration_ms".to_owned(), Value::from(duration_ms));
    }
    if let Some(call_id) = string_at(
        body,
        &[
            "/call_id",
            "/callId",
            "/callID",
            "/tool_call_id",
            "/state/call_id",
            "/state/callId",
            "/id",
        ],
    ) {
        diagnostic.insert("call_id".to_owned(), Value::String(call_id));
    }
    if let Some(tool) = string_at(body, &["/tool", "/tool_name", "/name"]) {
        diagnostic.insert("tool".to_owned(), Value::String(tool));
    }
    if let Some(command) = string_at(
        body,
        &[
            "/command",
            "/cmd",
            "/state/input/command",
            "/state/metadata/command",
        ],
    ) {
        diagnostic.insert("command".to_owned(), Value::String(command));
    }
    if let Some(cwd) = string_at(
        body,
        &[
            "/working_directory",
            "/workingDirectory",
            "/cwd",
            "/state/metadata/cwd",
        ],
    ) {
        diagnostic.insert("cwd".to_owned(), Value::String(cwd));
    }
    Some(RetainedWire {
        effective_type: effective_type.to_owned(),
        role: "tool".to_owned(),
        body: Value::Object(diagnostic),
    })
}

pub(super) fn tag(value: u8) -> Vec<u8> {
    vec![value]
}

pub(super) fn hex_tag(value: u8) -> String {
    format!("{value:02x}")
}

pub(super) fn output_predicate_sql(
    data: &str,
    column_type: &str,
    parent_data: Option<&str>,
) -> String {
    let direct_tokens = "'result','toolresult','toolresponse','commandresult','output',\
                         'tooloutput','commandoutput'";
    let normalized_column = normalized_sql(column_type);
    let value_predicate = |value: &str| {
        format!(
            "{normalized} in ({direct_tokens})
             or {normalized} like '%result'
             or {normalized} like '%output'",
            normalized = normalized_sql(value),
        )
    };
    let tree_predicate = |value: &str| {
        format!(
            "exists(
                 select 1 from json_tree({value}) jt
                 where (
                     ({key} in ({direct_tokens})
                       or {key} like '%result'
                       or {key} like '%output')
                     and not (
                         {key} = 'output'
                         and jt.type in ('integer', 'real')
                         and replace(jt.fullkey, '[', '.') like '%.tokens.output'
                     )
                 )
                 or (
                     {key} in ('type', 'role')
                     and ({atom})
                 )
             )",
            key = normalized_sql("coalesce(jt.key, '')"),
            atom = value_predicate("coalesce(jt.atom, '')"),
        )
    };
    let mut predicates = vec![value_predicate(&normalized_column), tree_predicate(data)];
    if let Some(parent) = parent_data {
        predicates.push(tree_predicate(parent));
    }
    predicates
        .into_iter()
        .map(|predicate| format!("({predicate})"))
        .collect::<Vec<_>>()
        .join(" or ")
}

pub(super) fn failure_predicate_sql(data: &str, parent_data: Option<&str>) -> String {
    let predicate = |value: &str| {
        format!(
            "exists(
                 select 1 from json_tree({value}) jt
                 where (
                     {key} in ('timedout', 'timeout', 'iserror')
                     and (
                         (jt.type = 'true')
                         or {atom} in (
                             'timeout','timedout','failed','failure','error','errored',
                             'cancelled','canceled'
                         )
                     )
                 )
                 or (
                     {key} in ('status', 'outcome', 'state')
                     and {atom} in (
                         'timeout','timedout','failed','failure','error','errored',
                         'cancelled','canceled'
                     )
                 )
                 or (
                     {key} in ('exit', 'exitcode')
                     and jt.type = 'integer'
                     and cast(jt.atom as integer) <> 0
                 )
                 or (
                     {key} = 'success' and jt.type = 'false'
                 )
                 or (
                     {key} = 'error' and jt.type not in ('null', 'false')
                     and cast(jt.atom as text) <> ''
                 )
             )",
            key = normalized_sql("coalesce(jt.key, '')"),
            atom = normalized_sql("coalesce(cast(jt.atom as text), '')"),
        )
    };
    let mut predicates = vec![predicate(data)];
    if let Some(parent) = parent_data {
        predicates.push(predicate(parent));
    }
    predicates
        .into_iter()
        .map(|predicate| format!("({predicate})"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn normalized_sql(value: &str) -> String {
    format!("lower(replace(replace(replace(trim({value}), '_', ''), '-', ''), ' ', ''))")
}

pub(super) fn family_from_label(label: &str) -> Option<OpenCodeNativeSchemaFamily> {
    match label {
        "session_message_seq" => Some(OpenCodeNativeSchemaFamily::SessionMessageSeq),
        "session_message_synthesized_seq" => {
            Some(OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq)
        }
        "session_entry" => Some(OpenCodeNativeSchemaFamily::SessionEntry),
        "legacy_message" => Some(OpenCodeNativeSchemaFamily::LegacyMessage),
        "message_part" => Some(OpenCodeNativeSchemaFamily::MessagePart),
        _ => None,
    }
}

pub(super) fn effective_type(
    column_type: &str,
    body_role: Option<&str>,
    body_type: Option<&str>,
    parent_role: Option<&str>,
) -> String {
    let column = column_type.trim().to_ascii_lowercase();
    if !column.is_empty() && column != "message" && column != "part" {
        return column;
    }
    first_nonempty(&[body_role, body_type, parent_role])
        .unwrap_or(column.as_str())
        .trim()
        .to_ascii_lowercase()
}

pub(super) fn first_nonempty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values
        .iter()
        .flatten()
        .copied()
        .find(|value| !value.trim().is_empty())
}

pub(super) fn object_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(super) fn tool_call_is_retained(body: &Value) -> bool {
    let status = body
        .pointer("/state/status")
        .or_else(|| body.pointer("/state/outcome"))
        .or_else(|| body.get("status"))
        .or_else(|| body.get("outcome"))
        .and_then(Value::as_str)
        .map(normalize_token);
    let has_input = body.pointer("/state/input").is_some()
        || body.get("input").is_some()
        || body.get("arguments").is_some()
        || body.get("command").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_calls").is_some();
    has_input
        && status
            .as_deref()
            .is_none_or(|status| matches!(status, "pending" | "running"))
}

pub(super) fn is_retained_type(family: Option<OpenCodeNativeSchemaFamily>, value: &str) -> bool {
    matches!(
        normalize_token(value).as_str(),
        "user"
            | "assistant"
            | "system"
            | "text"
            | "reasoning"
            | "summary"
            | "notice"
            | "patch"
            | "stepstart"
            | "stepfinish"
            | "snapshot"
            | "toolcall"
            | "tooluse"
            | "agentswitched"
            | "modelswitched"
            | "synthetic"
            | "compaction"
    ) || (family != Some(OpenCodeNativeSchemaFamily::MessagePart)
        && normalize_token(value) == "message")
}

pub(super) fn is_tool_token(value: &str) -> bool {
    matches!(normalize_token(value).as_str(), "tool" | "shell")
}

pub(super) fn is_direct_output_token(value: &str) -> bool {
    let value = normalize_token(value);
    matches!(
        value.as_str(),
        "result"
            | "toolresult"
            | "toolresponse"
            | "commandresult"
            | "output"
            | "tooloutput"
            | "commandoutput"
    ) || value.ends_with("result")
}

pub(super) fn is_output_key(value: &str, child: &Value, inside_tokens: bool) -> bool {
    let value = normalize_token(value);
    if inside_tokens && value == "output" && child.is_number() {
        return false;
    }
    matches!(
        value.as_str(),
        "output"
            | "result"
            | "stdout"
            | "stderr"
            | "toolresult"
            | "commandresult"
            | "tooloutput"
            | "commandoutput"
    ) || value.ends_with("result")
        || value.ends_with("output")
}

pub(super) fn is_terminal_status(value: &str) -> bool {
    matches!(
        normalize_token(value).as_str(),
        "complete"
            | "completed"
            | "success"
            | "succeeded"
            | "ok"
            | "failed"
            | "failure"
            | "error"
            | "errored"
            | "timeout"
            | "timedout"
            | "cancelled"
            | "canceled"
    )
}

pub(super) fn normalize_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Default)]
struct OutcomeAggregate {
    timeout: bool,
    failure: bool,
    success: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

impl OutcomeAggregate {
    fn outcome(&self) -> u8 {
        if self.timeout {
            3
        } else if self.failure {
            2
        } else if self.success {
            1
        } else {
            0
        }
    }

    fn has_signal(&self) -> bool {
        self.timeout
            || self.failure
            || self.success
            || self.exit_code.is_some()
            || self.duration_ms.is_some()
    }
}

fn collect_outcome(value: &Value) -> OutcomeAggregate {
    let mut aggregate = OutcomeAggregate::default();
    collect_outcome_into(value, &mut aggregate);
    aggregate
}

fn collect_outcome_into(value: &Value, aggregate: &mut OutcomeAggregate) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_outcome_into(value, aggregate);
            }
        }
        Value::Object(object) => {
            aggregate.timeout |= ["timed_out", "timedOut", "timeout"]
                .iter()
                .any(|key| object.get(*key).and_then(Value::as_bool).unwrap_or(false));
            if let Some(success) = object.get("success").and_then(Value::as_bool) {
                aggregate.success |= success;
                aggregate.failure |= !success;
            }
            aggregate.failure |= object
                .get("isError")
                .or_else(|| object.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(code) = ["exit", "exit_code", "exitCode"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_i64))
            {
                aggregate.exit_code = i32::try_from(code).ok();
                aggregate.success |= code == 0;
                aggregate.failure |= code != 0;
            }
            if aggregate.duration_ms.is_none() {
                aggregate.duration_ms = ["duration_ms", "durationMs"]
                    .iter()
                    .find_map(|key| object.get(*key).and_then(Value::as_u64));
            }
            for key in ["status", "state", "outcome"] {
                if let Some(status) = object.get(key).and_then(Value::as_str) {
                    let status = normalize_token(status);
                    aggregate.timeout |= matches!(status.as_str(), "timeout" | "timedout");
                    aggregate.failure |= matches!(
                        status.as_str(),
                        "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
                    );
                    aggregate.success |= matches!(
                        status.as_str(),
                        "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
                    );
                }
            }
            aggregate.failure |= object.get("error").is_some_and(nonempty_error);
            for child in object.values() {
                collect_outcome_into(child, aggregate);
            }
        }
        _ => {}
    }
}

fn nonempty_error(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_none_or(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn string_at(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}
