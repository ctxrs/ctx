use super::*;

pub(super) fn core_record<R: crate::JsonlProviderRuntime>(
    compound: &KimiCompoundObservation,
    session_id: StableEntityId,
    fallback_identities: &mut FallbackEventIdentityState<R>,
    bytes: &[u8],
    ordinal: u64,
    value: &Value,
    fallback_timestamp: DateTime<Utc>,
) -> KimiSourceBackedResult<Option<CoreRecord>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((event_type, body)) =
        kimi_lexical_body(value, ordinal, compound.native.session.cwd.as_deref())?
    else {
        return Ok(None);
    };
    let role = kimi_event_role(record_type, value, event_type);
    let occurred_at =
        kimi_record_timestamp(value, fallback_timestamp).unwrap_or(fallback_timestamp);
    let assignment = fallback_identities.assign(fallback_fingerprint(bytes)?, None)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &compound.source,
        session_id,
        logical_item_kind: KIMI_LOGICAL_EVENT_KIND,
        native_item_key: assignment.native_item_key(),
        subrecord_selector: None,
    })?;
    let mut facts = Vec::new();
    if let Some(cwd) = &compound.native.session.cwd {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.clone(), facts.len())
        {
            facts.push(fact);
        }
    }
    for fact in kimi_literal_facts(value)? {
        if let Some(fact) = admit_provider_declared_fact(fact.kind, fact.value, facts.len()) {
            facts.push(fact);
        }
    }
    let parent_session_id = compound
        .native
        .session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| lineage_session_identity(parent, compound.source_anchor_scope))
        .transpose()?;
    let root_session_id = compound
        .native
        .session
        .root_provider_session_id
        .as_deref()
        .map(|root| lineage_session_identity(root, compound.source_anchor_scope))
        .transpose()?;
    let event = value.get("event").unwrap_or(value);
    let activity = kimi_activity(event, event_type, &body, facts)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        compound.source.clone(),
        ordinal,
        event_type.as_str(),
        KIMI_SOURCE_PARSER_REVISION,
        body.clone(),
    )?;
    if let Some(parent_session_id) = parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.root_session_id = root_session_id;
        if compound.native.session.agent_scope == Some(ctx_history_core::AgentScope::Subagent) {
            record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
        }
    }
    record.provider_session_id = Some(compound.native.session.provider_session_id.clone());
    record.native_event_id = Some(assignment.native_event_id().clone());
    record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
    record.role = Some(role.as_str().to_owned());
    record.agent_scope = compound.native.session.agent_scope;
    record.content.structured_content = Some(value.clone());
    record.content.activity = activity;
    ctx_history_jsonl::fit_jsonl_activity(
        &body,
        record.content.structured_content.as_ref(),
        &mut record.content.activity,
        ctx_history_jsonl::JsonlActivityObservedBytes::infer_from_present(),
        MAX_CORE_CONTENT_BYTES,
    );
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(Some(record))
}

fn kimi_activity(
    event: &Value,
    event_type: EventType,
    body: &str,
    facts: Vec<ProviderDeclaredFact>,
) -> KimiSourceBackedResult<Option<CoreActivity>> {
    let call_ids = ["toolCallId", "callId", "call_id", "id"]
        .into_iter()
        .filter_map(|field| event.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let provider_call_id = match call_ids.as_slice() {
        [id] => admit_optional_provider_call_id(Some((*id).to_owned())),
        _ => None,
    };
    let tools = ["toolName", "tool_name", "name"]
        .into_iter()
        .filter_map(|field| event.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let invocation = if provider_call_id.is_some() && event_type == EventType::ToolCall {
        match tools.as_slice() {
            [tool] => admit_optional_metadata_text(Some((*tool).to_owned())).map(|tool| {
                ActivityInvocation {
                    protocol: None,
                    server: None,
                    tool,
                    arguments: event.get("args").or_else(|| event.get("arguments")).map_or(
                        ActivityJsonCapture::Absent,
                        |value| ActivityJsonCapture::Present {
                            value: value.clone(),
                        },
                    ),
                    started_at_unix_ms: None,
                }
            }),
            _ => None,
        }
    } else {
        None
    };
    let result = (provider_call_id.is_some()
        && matches!(event_type, EventType::ToolOutput | EventType::CommandOutput))
    .then(|| ActivityResult {
        status: None,
        completed_at_unix_ms: None,
        duration_ns: None,
        text: ActivityTextCapture::Present {
            value: body.to_owned(),
        },
        structured_content: ActivityJsonCapture::Present {
            value: event.clone(),
        },
    });
    if invocation.is_none() && result.is_none() && facts.is_empty() {
        return Ok(None);
    }
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
}

pub(super) fn kimi_lexical_body(
    value: &Value,
    _ordinal: u64,
    _cwd: Option<&str>,
) -> KimiSourceBackedResult<Option<(EventType, String)>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = kimi_event_type(record_type, value);
    let body = if event_type == EventType::ToolOutput {
        kimi_output_content(value).unwrap_or_default()
    } else {
        kimi_event_text(record_type, value, event_type)
    };
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some((event_type, body)))
}

fn kimi_literal_facts(value: &Value) -> KimiSourceBackedResult<Vec<ProviderDeclaredFact>> {
    let mut facts = Vec::new();
    let outcome = visit_provider_file_reference_drafts_with_limit(
        value,
        MAX_PROVIDER_FILE_REFERENCES_PER_EVENT,
        |(_, reference)| {
            if let Some(fact) =
                admit_provider_declared_fact(reference.kind, reference.value, facts.len())
            {
                facts.push(fact);
            }
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(if outcome.limit_exceeded() {
        Vec::new()
    } else {
        facts
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_tool_call_id_publishes_exact_call_and_result_activity() {
        let call = serde_json::json!({
            "type": "tool.call",
            "toolCallId": "call_1",
            "name": "Read",
            "args": {"path": "/tmp/splines.txt"}
        });
        let result = serde_json::json!({
            "type": "tool.result",
            "toolCallId": "call_1",
            "result": {"output": "spline data", "isError": false}
        });

        let call_activity =
            kimi_activity(&call, EventType::ToolCall, "tool call: Read", Vec::new())
                .unwrap()
                .unwrap();
        assert_eq!(
            call_activity.provider_call_id,
            Some(TypedKey::utf8("call_1").unwrap())
        );
        assert_eq!(
            call_activity.invocation,
            Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool: "Read".to_owned(),
                arguments: ActivityJsonCapture::Present {
                    value: serde_json::json!({"path": "/tmp/splines.txt"}),
                },
                started_at_unix_ms: None,
            })
        );
        assert_eq!(call_activity.result, None);

        let result_activity =
            kimi_activity(&result, EventType::ToolOutput, "spline data", Vec::new())
                .unwrap()
                .unwrap();
        assert_eq!(
            result_activity.provider_call_id,
            Some(TypedKey::utf8("call_1").unwrap())
        );
        assert_eq!(result_activity.invocation, None);
        assert_eq!(
            result_activity.result,
            Some(ActivityResult {
                status: None,
                completed_at_unix_ms: None,
                duration_ns: None,
                text: ActivityTextCapture::Present {
                    value: "spline data".to_owned(),
                },
                structured_content: ActivityJsonCapture::Present { value: result },
            })
        );
    }

    #[test]
    fn missing_tool_call_id_withholds_linkage_and_preserves_content_and_fact_order() {
        let call = serde_json::json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.call",
                "toolName": "Write",
                "input": {"path": "src/kimi_cli_native.txt", "content": "proof"}
            }
        });
        let result = serde_json::json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.result",
                "toolName": "Write",
                "output": "wrote src/kimi_cli_native.txt"
            }
        });
        let cwd_fact = ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: "/workspace/kimi".to_owned(),
        };
        let file_fact = ProviderDeclaredFact {
            kind: LiteralFactKind::File,
            value: "src/kimi_cli_native.txt".to_owned(),
        };

        for (record, expected_event_type, expected_body, expected_facts) in [
            (
                &call,
                EventType::ToolCall,
                "tool call: Write",
                vec![cwd_fact.clone(), file_fact],
            ),
            (
                &result,
                EventType::ToolOutput,
                "wrote src/kimi_cli_native.txt",
                vec![cwd_fact.clone()],
            ),
        ] {
            let original = record.clone();
            let (event_type, body) = kimi_lexical_body(record, 0, None).unwrap().unwrap();
            assert_eq!(event_type, expected_event_type);
            assert_eq!(body, expected_body);
            let mut facts = kimi_literal_facts(record).unwrap();
            facts.insert(0, cwd_fact.clone());
            assert_eq!(facts, expected_facts);

            let activity = kimi_activity(
                record.get("event").unwrap(),
                event_type,
                &body,
                facts.clone(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(activity.provider_call_id, None);
            assert_eq!(activity.invocation, None);
            assert_eq!(activity.result, None);
            assert_eq!(activity.facts, expected_facts);
            assert_eq!(record, &original);
        }

        assert_eq!(
            kimi_activity(
                result.get("event").unwrap(),
                EventType::ToolOutput,
                "wrote src/kimi_cli_native.txt",
                Vec::new(),
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn unlinked_tool_events_do_not_invent_activity_linkage() {
        let event = serde_json::json!({
            "type": "tool.call",
            "toolName": "Write",
            "input": {"path": "src/example.rs"}
        });

        assert_eq!(
            kimi_activity(&event, EventType::ToolCall, "body", Vec::new()).unwrap(),
            None
        );
    }

    #[test]
    fn unadmitted_optional_linkage_does_not_emit_empty_activity() {
        let oversized = "x".repeat(64 * 1024 + 1);
        let output = serde_json::json!({"toolCallId": oversized});
        assert_eq!(
            kimi_activity(&output, EventType::ToolOutput, "output", Vec::new()).unwrap(),
            None
        );

        let call = serde_json::json!({
            "toolCallId": "kimi-call-1",
            "toolName": oversized,
        });
        assert_eq!(
            kimi_activity(&call, EventType::ToolCall, "call", Vec::new()).unwrap(),
            None
        );
    }

    #[test]
    fn provider_textual_result_over_16k_is_complete() {
        let tail = "kimi_success_result_tail_complete";
        let output = format!("{} {tail}", "successful kimi output ".repeat(800));
        assert!(output.len() > 16_000);
        let value = serde_json::json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.result",
                "toolName": "bash",
                "call_id": "complete-success",
                "exit_code": 0,
                "output": output,
            }
        });

        let (event_type, body) = kimi_lexical_body(&value, 0, None).unwrap().unwrap();
        assert_eq!(event_type, EventType::ToolOutput);
        assert_eq!(body, output);
        assert!(body.ends_with(tail));
    }
}
