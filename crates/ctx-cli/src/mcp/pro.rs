use std::{path::Path, time::Instant};

use anyhow::{anyhow, Result};
use ctx_pro_host_protocol::{QueryKind, ResourceKind, ResourceSelector, MAX_QUERY_RESULTS};
use serde_json::{json, Value};

use super::{object_schema, optional_usize, telemetry::McpHandled};
use crate::analytics::{
    pro_failure_bucket, pro_helper_connection_outcome, pro_operation_event, Outcome,
    ProAccessStateV1, ProFailureBucketV1, ProHelperConnectionOutcomeV1, ProHostOperationV1,
    ProQueryKindV1, ProQuerySurfaceV1, ProQueryTelemetryV1,
};

pub(super) fn tool_pro_status(data_root: &Path) -> McpHandled<Result<Value>> {
    let started = Instant::now();
    let value = crate::pro::lifecycle_status_json(data_root);
    let error_code = value.get("error_code").and_then(Value::as_str);
    let mut telemetry = ProQueryTelemetryV1::new(ProQueryKindV1::Status, ProQuerySurfaceV1::Mcp);
    telemetry.access_state = value
        .get("access_state")
        .and_then(Value::as_str)
        .and_then(ProAccessStateV1::from_safe_name);
    telemetry.helper_connection = if value.get("helper_version").is_some_and(Value::is_string) {
        ProHelperConnectionOutcomeV1::Connected
    } else {
        pro_helper_connection_outcome(error_code)
    };
    if error_code.is_some() {
        telemetry.fail(error_code);
    }
    let event = pro_operation_event(
        ProHostOperationV1::Query(telemetry),
        if error_code.is_none() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    McpHandled::with_pro_event(Ok(value), event)
}

pub(super) fn tool_pro_blame(arguments: &Value, data_root: &Path) -> McpHandled<Result<Value>> {
    let started = Instant::now();
    let mut telemetry = ProQueryTelemetryV1::new(ProQueryKindV1::Blame, ProQuerySurfaceV1::Mcp);
    let result = (|| {
        let target = required_resource_selector(arguments)?;
        if target.kind != ResourceKind::File {
            return Err(anyhow!("blame target.kind must be file"));
        }
        tool_pro_query_target(
            arguments,
            data_root,
            QueryKind::Blame,
            "pro_blame",
            target,
            &mut telemetry,
        )
    })();
    finish_mcp_query_telemetry(&mut telemetry, started, result)
}

pub(super) fn tool_pro_query(
    arguments: &Value,
    data_root: &Path,
    kind: QueryKind,
    payload_type: &str,
) -> McpHandled<Result<Value>> {
    let started = Instant::now();
    let mut telemetry =
        ProQueryTelemetryV1::new(ProQueryKindV1::from_protocol(kind), ProQuerySurfaceV1::Mcp);
    let result = (|| {
        let target = required_resource_selector(arguments)?;
        tool_pro_query_target(
            arguments,
            data_root,
            kind,
            payload_type,
            target,
            &mut telemetry,
        )
    })();
    finish_mcp_query_telemetry(&mut telemetry, started, result)
}

fn tool_pro_query_target(
    arguments: &Value,
    data_root: &Path,
    kind: QueryKind,
    payload_type: &str,
    target: ResourceSelector,
    telemetry: &mut ProQueryTelemetryV1,
) -> Result<Value> {
    let object = arguments
        .as_object()
        .ok_or_else(|| anyhow!("arguments must be an object"))?;
    let supports_cursor = matches!(
        kind,
        QueryKind::Timeline | QueryKind::Related | QueryKind::Facts
    );
    if let Some(key) = object.keys().find(|key| {
        !(matches!(key.as_str(), "target" | "limit") || supports_cursor && key.as_str() == "cursor")
    }) {
        return Err(anyhow!("unknown query argument {key}"));
    }
    let limit =
        optional_usize(arguments, "limit")?.unwrap_or(crate::pro::DEFAULT_QUERY_LIMIT as usize);
    if limit == 0 || limit > MAX_QUERY_RESULTS as usize {
        return Err(anyhow!("limit must be between 1 and {MAX_QUERY_RESULTS}"));
    }
    let cursor = if supports_cursor {
        match arguments.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            Some(Value::String(_)) => return Err(anyhow!("cursor cannot be empty")),
            Some(_) => return Err(anyhow!("cursor must be a string")),
        }
    } else {
        None
    };
    let result = crate::pro::query(
        data_root,
        kind,
        target.clone(),
        u32::try_from(limit).map_err(|_| anyhow!("limit is too large"))?,
        cursor,
        telemetry,
    )?;
    telemetry.complete(result.records.len(), result.truncated, result.stale);
    Ok(crate::pro::query_result_json(
        payload_type,
        &target,
        &result,
    ))
}

fn finish_mcp_query_telemetry(
    telemetry: &mut ProQueryTelemetryV1,
    started: Instant,
    result: Result<Value>,
) -> McpHandled<Result<Value>> {
    if let Err(error) = &result {
        let code = crate::pro::stable_error_code(error);
        if telemetry.helper_connection == ProHelperConnectionOutcomeV1::NotAttempted {
            telemetry.failure = Some(code.map_or(ProFailureBucketV1::InvalidRequest, |code| {
                pro_failure_bucket(Some(code))
            }));
        } else {
            telemetry.fail(code);
        }
    }
    let event = pro_operation_event(
        ProHostOperationV1::Query(*telemetry),
        if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    McpHandled::with_pro_event(result, event)
}

fn required_resource_selector(arguments: &Value) -> Result<ResourceSelector> {
    let target = arguments
        .get("target")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("target must be an object"))?;
    if let Some(key) = target
        .keys()
        .find(|key| !matches!(key.as_str(), "kind" | "value" | "repository" | "line"))
    {
        return Err(anyhow!("unknown target argument {key}"));
    }
    let kind = target
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("target.kind is required"))?;
    let kind = match kind {
        "repository" | "repo" => ResourceKind::Repository,
        "checkout" => ResourceKind::Checkout,
        "worktree" => ResourceKind::Worktree,
        "branch" => ResourceKind::Branch,
        "commit" => ResourceKind::Commit,
        "file" => ResourceKind::File,
        "pull_request" | "pull-request" | "pr" => ResourceKind::PullRequest,
        "issue" => ResourceKind::Issue,
        "remote" => ResourceKind::Remote,
        "release" => ResourceKind::Release,
        "command" => ResourceKind::Command,
        "check" => ResourceKind::Check,
        "session" => ResourceKind::Session,
        "agent" => ResourceKind::Agent,
        "run" => ResourceKind::Run,
        _ => return Err(anyhow!("target.kind is not a supported resource kind")),
    };
    let value = target
        .get("value")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("target.value is required"))?
        .to_owned();
    let repository = match target.get("repository") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(Value::String(_)) => return Err(anyhow!("target.repository cannot be empty")),
        Some(_) => return Err(anyhow!("target.repository must be a string")),
    };
    let line = match target.get("line") {
        None | Some(Value::Null) => None,
        Some(Value::Number(value)) => Some(
            u32::try_from(
                value
                    .as_u64()
                    .ok_or_else(|| anyhow!("target.line must be a positive integer"))?,
            )
            .map_err(|_| anyhow!("target.line is too large"))?,
        ),
        Some(_) => return Err(anyhow!("target.line must be a positive integer")),
    };
    let selector = ResourceSelector {
        kind,
        value,
        repository,
        line,
    };
    selector
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    Ok(selector)
}

pub(super) fn pro_query_tool(
    name: &str,
    title: &str,
    description: &str,
    fixed_kind: Option<&str>,
    supports_cursor: bool,
) -> Value {
    let target = resource_target_schema(fixed_kind);
    let mut properties = serde_json::Map::from_iter([
        ("target".to_owned(), target),
        (
            "limit".to_owned(),
            json!({ "type": "integer", "minimum": 1, "maximum": MAX_QUERY_RESULTS, "default": crate::pro::DEFAULT_QUERY_LIMIT, "description": "Maximum cited records to return." }),
        ),
    ]);
    if supports_cursor {
        properties.insert(
            "cursor".to_owned(),
            json!({ "type": "string", "minLength": 1, "maxLength": ctx_pro_host_protocol::MAX_QUERY_CURSOR_BYTES }),
        );
    }
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": object_schema(Value::Object(properties), vec!["target"]),
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true },
    })
}

fn resource_target_schema(fixed_kind: Option<&str>) -> Value {
    let kind = fixed_kind.map_or_else(
        || {
            let resource_kinds = ResourceKind::ALL
                .into_iter()
                .map(ResourceKind::wire_name)
                .collect::<Vec<_>>();
            json!({ "type": "string", "enum": resource_kinds })
        },
        |kind| json!({ "type": "string", "const": kind }),
    );
    let mut schema = object_schema(
        json!({
            "kind": kind,
            "value": { "type": "string", "minLength": 1, "description": "Resource value, such as a SHA, path, number, URL, name, or opaque resource ID." },
            "repository": { "type": "string", "minLength": 1, "description": "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path." },
            "line": { "type": "integer", "minimum": 1, "description": "Positive 1-based source line; valid only when kind is file." }
        }),
        vec!["kind", "value"],
    );
    if fixed_kind != Some("file") {
        if let Some(object) = schema.as_object_mut() {
            object.insert(
                "allOf".to_owned(),
                json!([{
                    "if": {
                        "properties": { "kind": { "not": { "const": "file" } } },
                        "required": ["kind"]
                    },
                    "then": { "not": { "required": ["line"] } }
                }]),
            );
        }
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_query_tool_schemas_do_not_advertise_cursors() {
        for (name, kind) in [
            ("pro_show", "commit"),
            ("pro_locate", "commit"),
            ("pro_blame", "file"),
        ] {
            let schema = pro_query_tool(name, name, name, Some(kind), false);
            assert!(schema["inputSchema"]["properties"].get("cursor").is_none());
        }

        let timeline = pro_query_tool("pro_timeline", "timeline", "timeline", None, true);
        assert!(timeline["inputSchema"]["properties"]["cursor"].is_object());
        assert_eq!(
            timeline["inputSchema"]["properties"]["target"]["allOf"][0]["then"]["not"]["required"]
                [0],
            "line"
        );

        let blame = pro_query_tool("pro_blame", "blame", "blame", Some("file"), false);
        assert!(blame["inputSchema"]["properties"]["target"]
            .get("allOf")
            .is_none());
    }

    #[test]
    fn point_queries_reject_cursor_before_helper_access() {
        let arguments = json!({
            "target": {"kind": "commit", "value": "abc123"},
            "cursor": "unexpected"
        });
        let handled = tool_pro_query(
            &arguments,
            Path::new("/definitely/not/a/ctx/data/root"),
            QueryKind::Show,
            "pro_show",
        );
        let error = handled
            .value
            .expect_err("show must reject continuation cursors");
        assert_eq!(error.to_string(), "unknown query argument cursor");
        assert!(handled.pro_event.is_some());
    }
}
