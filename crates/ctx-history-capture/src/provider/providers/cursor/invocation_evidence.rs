use ctx_history_core::{
    EventType, RepositoryFileInvocationKind, RepositoryFileInvocationTextRange,
};

use super::{
    parser::{CursorInputPathEvidence, MAX_CURSOR_INPUT_PATHS},
    projection::{CursorEventBody, CursorNativeEvent},
};

use crate::repository_attribution::UnscopedRepositoryFileInvocationEvidence;

pub(super) fn cursor_repository_file_invocation_evidence(
    event: &CursorNativeEvent,
    normalized_body: Option<&str>,
) -> Vec<UnscopedRepositoryFileInvocationEvidence> {
    let CursorEventBody::ToolCall {
        native_content,
        tool_name: Some(tool_name),
        input_paths: CursorInputPathEvidence::Exact(paths),
        ambiguous_native_fields: false,
        ..
    } = &event.body
    else {
        return Vec::new();
    };
    if event.event_type != EventType::ToolCall
        || paths.is_empty()
        || paths.len() > MAX_CURSOR_INPUT_PATHS
        || paths.iter().any(String::is_empty)
    {
        return Vec::new();
    }
    let Some(action) = cursor_file_invocation_action(tool_name) else {
        return Vec::new();
    };
    let normalized_text_range =
        exact_complete_normalized_body_range(native_content, normalized_body);
    paths
        .iter()
        .cloned()
        .map(|path| UnscopedRepositoryFileInvocationEvidence {
            operation_ordinal: event.native_order.part_ordinal,
            path,
            prior_path: None,
            kind: action,
            tool_name: Some(tool_name.clone()),
            normalized_text_range,
        })
        .collect()
}

fn cursor_file_invocation_action(tool_name: &str) -> Option<RepositoryFileInvocationKind> {
    match tool_name {
        "read_file" => Some(RepositoryFileInvocationKind::Read),
        "write_file" => Some(RepositoryFileInvocationKind::Write),
        _ => None,
    }
}

fn exact_complete_normalized_body_range(
    native_content: &serde_json::Value,
    normalized_body: Option<&str>,
) -> Option<RepositoryFileInvocationTextRange> {
    let normalized_body = normalized_body?;
    let expected = serde_json::to_string(native_content).ok()?;
    if normalized_body != expected {
        return None;
    }
    Some(RepositoryFileInvocationTextRange {
        start: 0,
        end: u32::try_from(normalized_body.len()).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    use super::*;
    use crate::provider::providers::cursor::parser::project_cursor_jsonl_record;

    fn events(row: &str) -> Vec<CursorNativeEvent> {
        project_cursor_jsonl_record(row.as_bytes(), 7, 7, 0, row.len() as u64)
            .unwrap()
            .unwrap()
    }

    fn normalized_body(event: &CursorNativeEvent) -> String {
        let CursorEventBody::ToolCall { native_content, .. } = &event.body else {
            panic!("expected Cursor tool call");
        };
        serde_json::to_string(native_content).unwrap()
    }

    const PATH_ALIASES: [&str; 4] = ["path", "file_path", "filePath", "paths"];
    const AGREED_PATH: &str = "/tmp/cursor-alias-matrix/src/lib.rs";

    fn alias_value(alias: &str) -> Value {
        if alias == "paths" {
            json!([AGREED_PATH])
        } else {
            json!(AGREED_PATH)
        }
    }

    fn project_tool_call(input: Map<String, Value>) -> CursorNativeEvent {
        let encoded = json!({
            "role": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "alias-matrix",
                    "name": "write_file",
                    "input": Value::Object(input)
                }]
            }
        })
        .to_string();
        let mut projected = events(&encoded);
        assert_eq!(projected.len(), 1);
        projected.remove(0)
    }

    #[test]
    fn exact_paths_emit_but_inexact_and_generic_paths_do_not() {
        let exact = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","input":{"path":"src/lib.rs"}}]}}"#,
        );
        let exact_evidence = cursor_repository_file_invocation_evidence(&exact[0], None);
        assert_eq!(exact_evidence.len(), 1);
        assert_eq!(exact_evidence[0].path, "src/lib.rs");
        assert_eq!(exact_evidence[0].kind, RepositoryFileInvocationKind::Write);
        assert_eq!(exact_evidence[0].tool_name.as_deref(), Some("write_file"));
        assert_eq!(exact_evidence[0].operation_ordinal, 0);
        assert_eq!(exact_evidence[0].normalized_text_range, None);

        let inexact = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","input":{"paths":["src/lib.rs",{"path":"src/main.rs"}]}}]}}"#,
        );
        assert!(cursor_repository_file_invocation_evidence(&inexact[0], None).is_empty());

        let generic = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"shell","name":"run_shell_command","input":{"path":"src/lib.rs","command":"cat src/lib.rs"}}]}}"#,
        );
        assert!(cursor_repository_file_invocation_evidence(&generic[0], None).is_empty());

        let read = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"read","name":"read_file","input":{"file_path":"src/lib.rs"}}]}}"#,
        );
        assert_eq!(
            cursor_repository_file_invocation_evidence(&read[0], None)[0].kind,
            RepositoryFileInvocationKind::Read
        );
    }

    #[test]
    fn only_originating_tool_calls_emit_invocation_evidence() {
        let call = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","input":{"path":"src/lib.rs"}}]}}"#,
        );
        let result = events(
            r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"write","content":"done"}]}}"#,
        );
        assert_eq!(
            cursor_repository_file_invocation_evidence(&call[0], None).len(),
            1
        );
        assert!(cursor_repository_file_invocation_evidence(&result[0], None).is_empty());
    }

    #[test]
    fn multiple_operations_preserve_native_ordinals_names_and_actions() {
        let projected = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first"},{"type":"tool_use","id":"write","name":"write_file","input":{"paths":["src/a.rs","src/b.rs"]}},{"type":"tool_use","id":"read","name":"read_file","input":{"path":"src/c.rs"}}]}}"#,
        );
        assert_eq!(projected.len(), 3);
        let write = cursor_repository_file_invocation_evidence(&projected[1], None);
        assert_eq!(write.len(), 2);
        assert!(write.iter().all(|evidence| {
            evidence.operation_ordinal == 1
                && evidence.tool_name.as_deref() == Some("write_file")
                && evidence.kind == RepositoryFileInvocationKind::Write
        }));
        assert_eq!(
            write
                .iter()
                .map(|evidence| evidence.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/a.rs", "src/b.rs"]
        );
        let read = cursor_repository_file_invocation_evidence(&projected[2], None);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].operation_ordinal, 2);
        assert_eq!(read[0].tool_name.as_deref(), Some("read_file"));
        assert_eq!(read[0].kind, RepositoryFileInvocationKind::Read);
    }

    #[test]
    fn ambiguous_native_fields_abstain_instead_of_selecting_a_path() {
        let duplicate_name = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","name":"read_file","input":{"path":"src/lib.rs"}}]}}"#,
        );
        assert!(cursor_repository_file_invocation_evidence(&duplicate_name[0], None).is_empty());

        let duplicate_input = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","input":{"path":"src/a.rs"},"input":{"path":"src/b.rs"}}]}}"#,
        );
        assert!(cursor_repository_file_invocation_evidence(&duplicate_input[0], None).is_empty());

        let duplicate_path = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","input":{"path":"src/a.rs","path":"src/b.rs"}}]}}"#,
        );
        assert!(cursor_repository_file_invocation_evidence(&duplicate_path[0], None).is_empty());
    }

    #[test]
    fn every_distinct_path_alias_pair_is_ambiguous_even_when_values_agree() {
        let mut observed_pairs = 0;
        for (left_index, left) in PATH_ALIASES.iter().enumerate() {
            for right in PATH_ALIASES.iter().skip(left_index + 1) {
                observed_pairs += 1;
                let mut input = Map::new();
                input.insert((*left).to_owned(), alias_value(left));
                input.insert((*right).to_owned(), alias_value(right));
                let expected_input = Value::Object(input.clone());
                let event = project_tool_call(input);
                let CursorEventBody::ToolCall {
                    native_content,
                    input_paths,
                    ..
                } = &event.body
                else {
                    panic!("expected Cursor tool call for {left} + {right}");
                };
                assert_eq!(native_content.get("input"), Some(&expected_input));
                assert!(matches!(
                    input_paths,
                    CursorInputPathEvidence::Inexact {
                        candidate_limit_exceeded: false,
                        invalid_shape: true,
                        ..
                    }
                ));
                assert!(cursor_repository_file_invocation_evidence(&event, None).is_empty());
            }
        }
        assert_eq!(observed_pairs, 6);
    }

    #[test]
    fn each_path_alias_is_exact_by_itself() {
        for alias in PATH_ALIASES {
            let mut input = Map::new();
            input.insert(alias.to_owned(), alias_value(alias));
            let event = project_tool_call(input);
            assert_eq!(
                cursor_repository_file_invocation_evidence(&event, None)
                    .iter()
                    .map(|evidence| evidence.path.as_str())
                    .collect::<Vec<_>>(),
                vec![AGREED_PATH],
                "{alias}"
            );
        }
    }

    #[test]
    fn path_capacity_is_all_or_nothing_without_truncation() {
        let paths = (0..MAX_CURSOR_INPUT_PATHS)
            .map(|index| format!("src/{index}.rs"))
            .collect::<Vec<_>>();
        let exact_row = json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "exact",
                "name": "write_file",
                "input": {"paths": paths}
            }]}
        })
        .to_string();
        let exact = events(&exact_row);
        assert_eq!(
            cursor_repository_file_invocation_evidence(&exact[0], None).len(),
            MAX_CURSOR_INPUT_PATHS
        );

        let overflow_paths = (0..=MAX_CURSOR_INPUT_PATHS)
            .map(|index| format!("src/{index}.rs"))
            .collect::<Vec<_>>();
        let overflow_row = json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "overflow",
                "name": "write_file",
                "input": {"paths": overflow_paths}
            }]}
        })
        .to_string();
        let overflow = events(&overflow_row);
        assert!(cursor_repository_file_invocation_evidence(&overflow[0], None).is_empty());
    }

    #[test]
    fn normalized_range_requires_the_exact_complete_target_bearing_body() {
        let projected = events(
            r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"write","name":"write_file","input":{"path":"src/quoted\"name.rs","contents":"complete"}}]}}"#,
        );
        let body = normalized_body(&projected[0]);
        let exact = cursor_repository_file_invocation_evidence(&projected[0], Some(&body));
        assert_eq!(exact.len(), 1);
        let range = exact[0].normalized_text_range.unwrap();
        assert_eq!(range.start, 0);
        assert_eq!(range.end, body.len() as u32);
        assert_eq!(&body[range.start as usize..range.end as usize], body);

        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["input"]["path"], "src/quoted\"name.rs");
        let partial = &body[..body.len() - 1];
        let no_partial_range =
            cursor_repository_file_invocation_evidence(&projected[0], Some(partial));
        assert_eq!(no_partial_range.len(), 1);
        assert_eq!(no_partial_range[0].normalized_text_range, None);
    }
}
