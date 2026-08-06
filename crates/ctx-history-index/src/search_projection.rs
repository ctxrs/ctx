use ctx_history_core::{CoreContent, CoreContentPolicyStatus, McpJsonCapture};

use crate::{IndexError, Result};

/// Derives the complete analyzed text stored in the lexical `body_search` field.
///
/// Callers must pass decoded, contract-validated Core content. The content is
/// consumed so its normalized-body allocation can become the projection and be
/// extended in place instead of cloning complete body text. Components appear in
/// order as normalized body, server, tool, and compact present argument JSON. No
/// response, capture-state, structured-content, or attribution-only data enters
/// this projection.
pub fn project_body_search(mut content: CoreContent) -> Result<Option<String>> {
    if !content.is_discovery_eligible()
        || !matches!(content.policy_status, CoreContentPolicyStatus::Selected)
    {
        return Ok(None);
    }

    let normalized_body = content
        .normalized_body
        .take()
        .filter(|body| !body.is_empty());
    let invocation = content
        .mcp_exchange
        .take()
        .and_then(|exchange| exchange.invocation);
    let (server, tool, argument_json) = match invocation {
        Some(invocation) => {
            let argument_json = match invocation.arguments {
                McpJsonCapture::Present { value } => Some(serde_json::to_string(&value)?),
                McpJsonCapture::Absent
                | McpJsonCapture::Unavailable
                | McpJsonCapture::Omitted { .. } => None,
            };
            (
                (!invocation.server.is_empty()).then_some(invocation.server),
                (!invocation.tool.is_empty()).then_some(invocation.tool),
                argument_json.filter(|arguments| !arguments.is_empty()),
            )
        }
        None => (None, None, None),
    };
    let components = [normalized_body, server, tool, argument_json];
    let component_count = components.iter().flatten().count();
    if component_count == 0 {
        return Ok(None);
    }
    let component_bytes = components
        .iter()
        .flatten()
        .try_fold(0_usize, |total, value| {
            total
                .checked_add(value.len())
                .ok_or(IndexError::CountOverflow)
        })?;
    let capacity = component_bytes
        .checked_add(component_count - 1)
        .ok_or(IndexError::CountOverflow)?;
    let mut components = components.into_iter().flatten();
    let mut projection = components.next().ok_or(IndexError::WriterInvariant(
        "body search projection component count drift",
    ))?;
    projection.reserve(capacity - projection.len());
    for component in components {
        projection.push('\n');
        projection.push_str(&component);
    }
    debug_assert_eq!(projection.len(), capacity);
    Ok(Some(projection))
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        CoreContentPolicyStatus, McpExchangeContent, McpInvocationContent, McpPayloadOmissionReason,
    };

    use super::*;

    fn selected_content(
        normalized_body: Option<&str>,
        arguments: Option<McpJsonCapture>,
    ) -> CoreContent {
        CoreContent {
            policy_revision: 1,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: normalized_body.map(str::to_owned),
            structured_content: None,
            discovery_exclusion: None,
            mcp_exchange: arguments.map(|arguments| McpExchangeContent {
                provider_call_id: "excluded-call-id".to_owned(),
                invocation: Some(McpInvocationContent {
                    server: "服务器".to_owned(),
                    tool: "lookup_tool".to_owned(),
                    arguments,
                }),
                response: None,
            }),
        }
    }

    #[test]
    fn ordinary_body_projection_moves_and_reuses_the_body_allocation() {
        let content = selected_content(Some("ordinary normalized body"), None);
        let body_pointer = content.normalized_body.as_ref().unwrap().as_ptr();
        let projection = project_body_search(content).unwrap().unwrap();

        assert_eq!(projection.as_ptr(), body_pointer);
        assert_eq!(projection, "ordinary normalized body");
    }

    #[test]
    fn retrieval_derived_content_has_no_body_projection() {
        let mut content = selected_content(Some("retrieval payload canary"), None);
        content.discovery_exclusion =
            Some(ctx_history_core::CoreDiscoveryExclusion::CtxRetrievalDerived);

        assert_eq!(project_body_search(content).unwrap(), None);
    }

    #[test]
    fn invocation_projection_appends_into_spare_body_capacity() {
        let mut body = String::with_capacity(256);
        body.push_str("normalized body");
        let body_pointer = body.as_ptr();
        let mut content = selected_content(
            None,
            Some(McpJsonCapture::Present {
                value: serde_json::json!({}),
            }),
        );
        content.normalized_body = Some(body);

        let projection = project_body_search(content).unwrap().unwrap();

        assert_eq!(projection.as_ptr(), body_pointer);
        assert_eq!(projection, "normalized body\n服务器\nlookup_tool\n{}");
    }

    #[test]
    fn invocation_projection_is_ordered_compact_and_deterministic() {
        let mut first = serde_json::Map::new();
        first.insert("zeta".to_owned(), serde_json::json!("München"));
        first.insert("empty".to_owned(), serde_json::json!({}));
        first.insert(
            "alpha".to_owned(),
            serde_json::json!(["東京", {"control": "line\nbreak\t\"quote\"\\slash"}, [3, 1, 2]]),
        );
        let mut second = serde_json::Map::new();
        second.insert(
            "alpha".to_owned(),
            serde_json::json!(["東京", {"control": "line\nbreak\t\"quote\"\\slash"}, [3, 1, 2]]),
        );
        second.insert("empty".to_owned(), serde_json::json!({}));
        second.insert("zeta".to_owned(), serde_json::json!("München"));

        let first = selected_content(
            Some("normalized body"),
            Some(McpJsonCapture::Present {
                value: serde_json::Value::Object(first),
            }),
        );
        let second = selected_content(
            Some("normalized body"),
            Some(McpJsonCapture::Present {
                value: serde_json::Value::Object(second),
            }),
        );
        let first = project_body_search(first).unwrap().unwrap();
        let second = project_body_search(second).unwrap().unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first,
            "normalized body\n服务器\nlookup_tool\n{\"alpha\":[\"東京\",{\"control\":\"line\\nbreak\\t\\\"quote\\\"\\\\slash\"},[3,1,2]],\"empty\":{},\"zeta\":\"München\"}"
        );
        assert!(!first.ends_with('\n'));
    }

    #[test]
    fn non_present_argument_states_add_no_projection_component() {
        let captures = [
            McpJsonCapture::Absent,
            McpJsonCapture::Unavailable,
            McpJsonCapture::Omitted {
                reason: McpPayloadOmissionReason::SizeLimit,
                observed_encoded_bytes: Some(998_877),
            },
        ];

        for capture in captures {
            let content = selected_content(Some("body"), Some(capture));
            assert_eq!(
                project_body_search(content).unwrap().as_deref(),
                Some("body\n服务器\nlookup_tool")
            );
        }
    }

    #[test]
    fn empty_object_is_serialized_but_excluded_content_is_ignored() {
        let mut content = selected_content(
            None,
            Some(McpJsonCapture::Present {
                value: serde_json::json!({}),
            }),
        );
        content.structured_content = Some(serde_json::json!({
            "structured_only_canary": "must_not_leak"
        }));

        assert_eq!(
            project_body_search(content).unwrap().as_deref(),
            Some("服务器\nlookup_tool\n{}")
        );
    }
}
