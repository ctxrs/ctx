use std::{path::Path, time::Instant};

use anyhow::Result;
use ctx_pro_host_protocol::{BlameTarget, LineRange, MAX_BLAME_CURSOR_BYTES};
use serde_json::{json, Map, Value};

use super::{invalid_tool_request, object_schema, optional_usize, telemetry::McpHandled};
use crate::analytics::{
    pro_failure_bucket, pro_helper_connection_outcome, pro_operation_event, Outcome,
    ProAccessStateV1, ProBlameTargetV1, ProBlameTelemetryV1, ProFailureBucketV1,
    ProHelperConnectionOutcomeV1, ProHostOperationV1, ProStatusTelemetryV1, ProSurfaceV1,
};

pub(super) const MCP_BLAME_LIMIT: usize = 8;
pub(super) const MCP_BLAME_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub(super) fn tool_pro_status(data_root: &Path) -> McpHandled<Result<Value>> {
    let started = Instant::now();
    let mut value = crate::pro::lifecycle_status_json(data_root);
    if let Some(object) = value.as_object_mut() {
        let access_state = object
            .get("access_state")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let usage = crate::config::AppConfig::load(data_root)
            .map(|config| {
                crate::local_usage::read_report(data_root, config.local_usage.enabled, false)
            })
            .unwrap_or_else(|_| crate::local_usage::UsageReport::config_error());
        object.insert(
            "local_usage".to_owned(),
            serde_json::to_value(usage).unwrap_or_else(|_| {
                json!({
                    "schema_version": 1,
                    "enabled": false,
                    "state": "error",
                    "error": {
                        "code": "usage_report_serialization",
                        "message": "local usage report could not be serialized",
                    }
                })
            }),
        );
        object.insert(
            "conversion_action".to_owned(),
            crate::local_usage::pro_conversion_action(access_state.as_deref())
                .unwrap_or(Value::Null),
        );
    }
    let error_code = value.get("error_code").and_then(Value::as_str);
    let mut telemetry = ProStatusTelemetryV1::new(ProSurfaceV1::Mcp);
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
        ProHostOperationV1::Status(telemetry),
        if error_code.is_none() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    McpHandled::with_pro_event(Ok(value), event)
}

pub(super) fn tool_pro_blame(
    arguments: &Value,
    data_root: &Path,
    parsed_target: Result<BlameTarget>,
) -> McpHandled<Result<Value>> {
    tool_pro_blame_with(
        arguments,
        data_root,
        parsed_target,
        crate::pro::blame,
        crate::commands::blame::hydrate_evidence_context,
    )
}

fn tool_pro_blame_with(
    arguments: &Value,
    data_root: &Path,
    parsed_target: Result<BlameTarget>,
    blame: impl FnOnce(
        &Path,
        BlameTarget,
        u32,
        Option<String>,
    ) -> Result<ctx_pro_host_protocol::BlameResult>,
    hydrate: impl FnOnce(
        &Path,
        &ctx_pro_host_protocol::BlameResult,
    ) -> crate::pro::evidence_preview::EvidencePreviewModel,
) -> McpHandled<Result<Value>> {
    let started = Instant::now();
    let target_kind = parsed_target
        .as_ref()
        .ok()
        .map(ProBlameTargetV1::from_protocol);
    let mut telemetry = ProBlameTelemetryV1::new(target_kind, ProSurfaceV1::Mcp);
    let result = (|| {
        let target = parsed_target?;
        let object = arguments
            .as_object()
            .ok_or_else(|| invalid_tool_request("arguments must be an object"))?;
        if let Some(key) = object
            .keys()
            .find(|key| !matches!(key.as_str(), "target" | "limit" | "cursor"))
        {
            return Err(invalid_tool_request(format!(
                "unknown blame argument {key}"
            )));
        }
        let limit = optional_usize(arguments, "limit")?.unwrap_or(MCP_BLAME_LIMIT);
        if limit == 0 || limit > MCP_BLAME_LIMIT {
            return Err(invalid_tool_request(format!(
                "limit must be between 1 and {MCP_BLAME_LIMIT}"
            )));
        }
        let cursor = optional_cursor(arguments)?;
        let result = blame(
            data_root,
            target,
            u32::try_from(limit).map_err(|_| invalid_tool_request("limit is too large"))?,
            cursor,
        )?;
        telemetry.complete(result.matches.len(), result.next.is_some());
        let previews = matches!(
            &result.target,
            ctx_pro_host_protocol::ResolvedBlameTarget::File { .. }
        )
        .then(|| hydrate(data_root, &result));
        Ok(crate::pro::blame_result_json(&result, previews.as_ref()))
    })();
    finish_mcp_blame_telemetry(&mut telemetry, started, result)
}

fn finish_mcp_blame_telemetry(
    telemetry: &mut ProBlameTelemetryV1,
    started: Instant,
    result: Result<Value>,
) -> McpHandled<Result<Value>> {
    if let Err(error) = &result {
        let code = crate::pro::stable_error_code(error);
        telemetry.failure = Some(code.map_or(ProFailureBucketV1::InvalidRequest, |code| {
            pro_failure_bucket(Some(code))
        }));
    }
    let event = pro_operation_event(
        ProHostOperationV1::Blame(*telemetry),
        if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    McpHandled::with_pro_event(result, event)
}

pub(super) fn required_blame_target(arguments: &Value) -> Result<BlameTarget> {
    let target = arguments
        .get("target")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_tool_request("target must be an object"))?;
    let kind = target
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_tool_request("target.kind is required"))?;
    let repository = optional_nonempty_string(target, "repository")?;
    let result = match kind {
        "file" => {
            reject_unknown_target_keys(target, &["kind", "path", "repository", "lines"])?;
            let path = required_nonempty_string(target, "path")?;
            let lines = optional_line_range(target.get("lines"))?;
            BlameTarget::File {
                path,
                repository,
                lines,
            }
        }
        "commit" => {
            reject_unknown_target_keys(target, &["kind", "oid", "repository"])?;
            BlameTarget::Commit {
                oid: required_nonempty_string(target, "oid")?,
                repository,
            }
        }
        "pull_request" => {
            reject_unknown_target_keys(target, &["kind", "selector", "repository"])?;
            BlameTarget::PullRequest {
                selector: required_nonempty_string(target, "selector")?,
                repository,
            }
        }
        _ => {
            return Err(invalid_tool_request(
                "target.kind must be file, commit, or pull_request",
            ));
        }
    };
    result
        .validate()
        .map_err(|error| invalid_tool_request(error.message))?;
    Ok(result)
}

fn optional_cursor(arguments: &Value) -> Result<Option<String>> {
    match arguments.get("cursor") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.is_empty() && value.len() <= MAX_BLAME_CURSOR_BYTES && value.is_ascii() =>
        {
            Ok(Some(value.clone()))
        }
        Some(Value::String(_)) => Err(invalid_tool_request(format!(
            "cursor must contain 1 to {MAX_BLAME_CURSOR_BYTES} ASCII bytes"
        ))),
        Some(_) => Err(invalid_tool_request("cursor must be a string")),
    }
}

fn optional_line_range(value: Option<&Value>) -> Result<Option<LineRange>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value
        .as_object()
        .ok_or_else(|| invalid_tool_request("target.lines must be an object"))?;
    reject_unknown_target_keys(object, &["start", "end"])?;
    let start = required_positive_u32(object, "start")?;
    let end = required_positive_u32(object, "end")?;
    let range = LineRange { start, end };
    range
        .validate()
        .map_err(|error| invalid_tool_request(error.message))?;
    Ok(Some(range))
}

fn required_positive_u32(object: &Map<String, Value>, field: &str) -> Result<u32> {
    let value = object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_tool_request(format!("target.lines.{field} must be positive")))?;
    let value = u32::try_from(value)
        .map_err(|_| invalid_tool_request(format!("target.lines.{field} is too large")))?;
    if value == 0 {
        return Err(invalid_tool_request(format!(
            "target.lines.{field} must be positive"
        )));
    }
    Ok(value)
}

fn required_nonempty_string(object: &Map<String, Value>, field: &str) -> Result<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_tool_request(format!("target.{field} is required")))
}

fn optional_nonempty_string(object: &Map<String, Value>, field: &str) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(invalid_tool_request(format!(
            "target.{field} cannot be empty"
        ))),
        Some(_) => Err(invalid_tool_request(format!(
            "target.{field} must be a string"
        ))),
    }
}

fn reject_unknown_target_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_tool_request(format!(
            "unknown target argument {key}"
        )));
    }
    Ok(())
}

pub(super) fn pro_blame_tool() -> Value {
    let target = json!({
        "oneOf": [
            object_schema(
                json!({
                    "kind": { "type": "string", "const": "file" },
                    "path": { "type": "string", "minLength": 1, "description": "Repository-relative committed file path." },
                    "repository": repository_schema(),
                    "lines": object_schema(
                        json!({
                            "start": { "type": "integer", "minimum": 1 },
                            "end": { "type": "integer", "minimum": 1 }
                        }),
                        vec!["start", "end"]
                    )
                }),
                vec!["kind", "path"]
            ),
            object_schema(
                json!({
                    "kind": { "type": "string", "const": "commit" },
                    "oid": { "type": "string", "minLength": 1, "description": "Full or unambiguous abbreviated Git commit ID." },
                    "repository": repository_schema()
                }),
                vec!["kind", "oid"]
            ),
            object_schema(
                json!({
                    "kind": { "type": "string", "const": "pull_request" },
                    "selector": { "type": "string", "minLength": 1, "description": "Positive PR number or canonical GitHub, GitLab, or Codeberg PR/MR URL." },
                    "repository": repository_schema()
                }),
                vec!["kind", "selector"]
            )
        ]
    });
    json!({
        "name": "blame",
        "title": "Agent Blame",
        "description": "Return complete cited provenance for committed file lines, a commit, or a pull request. PR activity and commit production remain separate. Blame may perform bounded local catch-up that updates the canonical Core index, writes the encrypted derived Pro graph, and writes the projection acknowledgement. It never writes provider history or repositories.",
        "inputSchema": object_schema(
            json!({
                "target": target,
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MCP_BLAME_LIMIT,
                    "default": MCP_BLAME_LIMIT,
                    "description": "Maximum complete matches to return."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_BLAME_CURSOR_BYTES,
                    "description": "Opaque continuation cursor from a previous blame page."
                }
            }),
            vec!["target"]
        ),
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": true
        },
    })
}

fn repository_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "Logical repository identity; required with a PR number and optional for other selectors."
    })
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::BTreeSet};

    use ctx_history_core::RepositoryFileInvocationKind;
    use ctx_pro_host_protocol::{BlameResult, ResolvedBlameTarget, ResourceKind, ResourceRef};

    use super::*;

    fn protocol_snapshot() -> ctx_pro_host_protocol::QuerySnapshotExpectation {
        ctx_pro_host_protocol::QuerySnapshotExpectation::Core {
            receipt: ctx_pro_host_protocol::CoreMaterializationReceiptIdentity {
                core_generation_id: "a".repeat(64),
                materializer_revision: "materializer-v1".to_owned(),
            },
        }
    }

    fn private_tempdir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
        root
    }

    #[test]
    fn pro_status_usage_config_error_is_content_free() {
        let root = private_tempdir();
        let marker = "SECRET_PATH_TOKEN_7f98";
        std::fs::write(
            root.path().join("config.toml"),
            format!("invalid config /tmp/{marker}/bearer-secret\n"),
        )
        .unwrap();

        let response = tool_pro_status(root.path()).value.unwrap();
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(!encoded.contains(marker));
        assert!(!encoded.contains("bearer-secret"));
        assert_eq!(response["local_usage"]["state"], "error");
        assert_eq!(
            response["local_usage"]["error"]["code"],
            "local_usage_config_unavailable"
        );
        assert_eq!(
            response["local_usage"]["error"]["message"],
            "local usage configuration could not be read"
        );
    }

    #[test]
    fn blame_schema_discloses_local_writes_and_is_agent_sized() {
        let schema = pro_blame_tool();
        let disclosure = "Blame may perform bounded local catch-up that updates the canonical Core index, writes the encrypted derived Pro graph, and writes the projection acknowledgement. It never writes provider history or repositories.";
        assert_eq!(schema["annotations"]["readOnlyHint"], false);
        assert_eq!(schema["annotations"]["destructiveHint"], false);
        assert_eq!(schema["annotations"]["idempotentHint"], true);
        assert!(schema["description"]
            .as_str()
            .is_some_and(|description| description.contains(disclosure)));
        assert_eq!(
            schema["inputSchema"]["properties"]["limit"]["maximum"],
            MCP_BLAME_LIMIT
        );
        assert_eq!(
            schema["inputSchema"]["properties"]["target"]["oneOf"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
    }

    #[test]
    fn blame_mcp_schema_has_no_evidence_switch() {
        let schema = pro_blame_tool();
        let properties = schema["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(properties, BTreeSet::from(["cursor", "limit", "target"]));
        assert_eq!(schema["inputSchema"]["additionalProperties"], false);
    }

    #[test]
    fn mcp_and_cli_json_share_available_and_unavailable_file_context() {
        let temp = private_tempdir();
        let unopened_data_root = temp.path().join("unopened-data-root");
        let repository = ResourceRef {
            id: "repository:fixture".to_owned(),
            kind: ResourceKind::Repository,
            display: "fixture/repository".to_owned(),
        };
        let result = BlameResult {
            snapshot: protocol_snapshot(),
            target: ResolvedBlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository,
                requested_lines: None,
            },
            git_snapshot: Some(ctx_pro_host_protocol::GitSnapshot {
                head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                worktree_status: ctx_pro_host_protocol::WorktreeStatus::Clean,
            }),
            matches: Vec::new(),
            evidence: Vec::new(),
            next: None,
        };
        let model = crate::pro::evidence_preview::EvidencePreviewModel {
            previews: vec![crate::pro::evidence_preview::EvidencePreview {
                citation_numbers: vec![1],
                operation: RepositoryFileInvocationKind::Modify,
                path: "src/lib.rs".to_owned(),
                prior_path: None,
                tool_name: "test_tool".to_owned(),
                event_occurred_at_ms: Some(1_721_000_000_000),
                excerpt: "modified: src/lib.rs".to_owned(),
            }],
        };
        let expected = crate::pro::blame_result_json(&result, Some(&model));
        let reads = Cell::new(0usize);
        let mcp = tool_pro_blame_with(
            &json!({"target": {"kind": "file", "path": "src/lib.rs"}}),
            &unopened_data_root,
            Ok(BlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: None,
                lines: None,
            }),
            |_, _, _, _| Ok(result.clone()),
            |_, _| {
                reads.set(reads.get() + 1);
                model.clone()
            },
        )
        .value
        .unwrap();
        assert_eq!(reads.get(), 1);
        assert_eq!(mcp, expected);
        assert_eq!(mcp["evidence_context"]["status"], "available");
        assert_eq!(
            mcp["evidence_context"]["items"].as_array().map(Vec::len),
            Some(1)
        );
        assert!(!serde_json::to_string(&mcp).unwrap().contains('\u{1b}'));

        let unavailable = tool_pro_blame_with(
            &json!({"target": {"kind": "file", "path": "src/lib.rs"}}),
            &unopened_data_root,
            Ok(BlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: None,
                lines: None,
            }),
            |_, _, _, _| Ok(result.clone()),
            |_, _| crate::pro::evidence_preview::EvidencePreviewModel {
                previews: Vec::new(),
            },
        )
        .value
        .unwrap();
        let empty_model = crate::pro::evidence_preview::EvidencePreviewModel {
            previews: Vec::new(),
        };
        let expected_unavailable = crate::pro::blame_result_json(&result, Some(&empty_model));
        assert_eq!(unavailable, expected_unavailable);
        assert_eq!(unavailable["evidence_context"]["status"], "unavailable");
        assert_eq!(
            unavailable["evidence_context"]["items"],
            serde_json::json!([])
        );
    }

    #[test]
    fn mcp_commit_and_pr_context_is_not_applicable_without_hydration_reads() {
        let repository = ResourceRef {
            id: "repository:fixture".to_owned(),
            kind: ResourceKind::Repository,
            display: "fixture/repository".to_owned(),
        };
        for (arguments, target, result) in [
            (
                json!({"target": {"kind": "commit", "oid": "abc1234"}}),
                BlameTarget::Commit {
                    oid: "abc1234".to_owned(),
                    repository: None,
                },
                BlameResult {
                    snapshot: protocol_snapshot(),
                    target: ResolvedBlameTarget::Commit {
                        commit: ResourceRef {
                            id: "commit:abc1234".to_owned(),
                            kind: ResourceKind::Commit,
                            display: "abc1234".to_owned(),
                        },
                        repository: repository.clone(),
                    },
                    git_snapshot: None,
                    matches: Vec::new(),
                    evidence: Vec::new(),
                    next: None,
                },
            ),
            (
                json!({"target": {"kind": "pull_request", "selector": "42", "repository": "fixture/repository"}}),
                BlameTarget::PullRequest {
                    selector: "42".to_owned(),
                    repository: Some("fixture/repository".to_owned()),
                },
                BlameResult {
                    snapshot: protocol_snapshot(),
                    target: ResolvedBlameTarget::PullRequest {
                        selector: "42".to_owned(),
                        pull_request: ResourceRef {
                            id: "pull_request:fixture:42".to_owned(),
                            kind: ResourceKind::PullRequest,
                            display: "fixture/repository#42".to_owned(),
                        },
                        repository: repository.clone(),
                    },
                    git_snapshot: None,
                    matches: Vec::new(),
                    evidence: Vec::new(),
                    next: None,
                },
            ),
        ] {
            let expected = crate::pro::blame_result_json(&result, None);
            let output = tool_pro_blame_with(
                &arguments,
                std::path::Path::new("/unopened"),
                Ok(target),
                |_, _, _, _| Ok(result.clone()),
                |_, _| panic!("non-file MCP blame performed an evidence hydration read"),
            )
            .value
            .unwrap();
            assert_eq!(output, expected);
            assert_eq!(output["evidence_context"]["status"], "not_applicable");
            assert_eq!(output["evidence_context"]["items"], serde_json::json!([]));
        }
    }

    #[test]
    fn target_parser_accepts_only_launch_targets() {
        for target in [
            json!({"kind": "file", "path": "src/lib.rs", "lines": {"start": 2, "end": 4}}),
            json!({"kind": "commit", "oid": "abc1234"}),
            json!({"kind": "pull_request", "selector": "42", "repository": "ctxrs/ctx"}),
            json!({"kind": "pull_request", "selector": "https://gitlab.example.com/a/b/-/merge_requests/42"}),
        ] {
            assert!(
                required_blame_target(&json!({"target": target})).is_ok(),
                "{target}"
            );
        }
        for target in [
            json!({"kind": "issue", "selector": "42"}),
            json!({"kind": "pull_request", "selector": "42"}),
            json!({"kind": "commit", "oid": "abc", "line": 2}),
        ] {
            assert!(
                required_blame_target(&json!({"target": target})).is_err(),
                "{target}"
            );
        }
    }
}
