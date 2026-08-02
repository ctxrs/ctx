use ctx_history_core::{RepositoryFileInvocationKind, RepositoryFileInvocationTextRange};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL: usize = 64;

const MAX_NATIVE_TARGET_BYTES: usize = 16 * 1024;
const DIRECT_TARGET_FIELDS: [&str; 3] = ["file_path", "filePath", "path"];
const MULTI_TARGET_FIELDS: [&str; 3] = ["file_paths", "filePaths", "paths"];
const PRIOR_TARGET_FIELDS: [&str; 2] = ["old_path", "oldPath"];
const RENAMED_TARGET_FIELDS: [&str; 2] = ["new_path", "newPath"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaudeExactFileOperation {
    Read,
    Edit,
    Delete,
    Rename,
    Create,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeExactFileTarget {
    pub(super) path: String,
    pub(super) prior_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeExactFileInvocation {
    pub(super) invocation_ordinal: u32,
    pub(super) tool_name: String,
    pub(super) operation: ClaudeExactFileOperation,
    pub(super) target: ClaudeExactFileTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeExactFileInvocationAbstention {
    CapacityExceeded,
    Opaque,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClaudeExactFileInvocations {
    invocations: Vec<ClaudeExactFileInvocation>,
    abstention: Option<ClaudeExactFileInvocationAbstention>,
}

impl ClaudeExactFileInvocations {
    pub(super) fn iter(&self) -> std::slice::Iter<'_, ClaudeExactFileInvocation> {
        self.invocations.iter()
    }

    pub(super) fn abstention(&self) -> Option<ClaudeExactFileInvocationAbstention> {
        self.abstention
    }

    #[cfg(test)]
    fn into_invocations(self) -> Vec<ClaudeExactFileInvocation> {
        self.invocations
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.invocations.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.invocations.len()
    }
}

impl IntoIterator for ClaudeExactFileInvocations {
    type Item = ClaudeExactFileInvocation;
    type IntoIter = std::vec::IntoIter<ClaudeExactFileInvocation>;

    fn into_iter(self) -> Self::IntoIter {
        self.invocations.into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeExactFileInvocationEvidence {
    pub(super) invocation: ClaudeExactFileInvocation,
    pub(super) normalized_text_range: Option<RepositoryFileInvocationTextRange>,
}

impl ClaudeExactFileInvocation {
    pub(super) fn evidence_in(
        &self,
        input: &Value,
        normalized_body: &str,
    ) -> ClaudeExactFileInvocationEvidence {
        let range_unit = serde_json::to_string(input).ok();
        let normalized_text_range = range_unit
            .as_deref()
            .and_then(|unit| unique_complete_unit_range(normalized_body, unit));
        ClaudeExactFileInvocationEvidence {
            invocation: self.clone(),
            normalized_text_range,
        }
    }

    pub(super) fn invocation_kind(&self) -> RepositoryFileInvocationKind {
        match self.operation {
            ClaudeExactFileOperation::Read => RepositoryFileInvocationKind::Read,
            ClaudeExactFileOperation::Edit => RepositoryFileInvocationKind::Modify,
            ClaudeExactFileOperation::Delete => RepositoryFileInvocationKind::Delete,
            ClaudeExactFileOperation::Rename => RepositoryFileInvocationKind::Rename,
            ClaudeExactFileOperation::Create => RepositoryFileInvocationKind::Create,
            ClaudeExactFileOperation::Write => RepositoryFileInvocationKind::Write,
        }
    }
}

pub(super) fn extract_exact_file_invocations(
    invocation_ordinal: u32,
    tool_name: Option<&str>,
    input: &Value,
) -> ClaudeExactFileInvocations {
    let Some(tool_name) = tool_name else {
        return ClaudeExactFileInvocations::default();
    };
    let operation = match tool_name {
        "Read" => ClaudeExactFileOperation::Read,
        "Edit" => ClaudeExactFileOperation::Edit,
        "Delete" => ClaudeExactFileOperation::Delete,
        "Rename" => ClaudeExactFileOperation::Rename,
        "Create" => ClaudeExactFileOperation::Create,
        "Write" => ClaudeExactFileOperation::Write,
        _ => return ClaudeExactFileInvocations::default(),
    };

    let targets = match operation {
        ClaudeExactFileOperation::Rename => exact_rename_target(input).map(|target| vec![target]),
        _ => exact_direct_targets(input),
    };
    match targets {
        Ok(targets) => ClaudeExactFileInvocations {
            invocations: targets
                .into_iter()
                .map(|target| ClaudeExactFileInvocation {
                    invocation_ordinal,
                    tool_name: tool_name.to_owned(),
                    operation,
                    target,
                })
                .collect(),
            abstention: None,
        },
        Err(abstention) => ClaudeExactFileInvocations {
            invocations: Vec::new(),
            abstention: Some(abstention),
        },
    }
}

pub(super) fn normalized_tool_call_body(
    call_id: Option<&str>,
    tool_name: Option<&str>,
    input: &Value,
) -> Option<String> {
    serde_json::to_string(&serde_json::json!({
        "type": "tool_use",
        "id": call_id,
        "name": tool_name,
        "input": input,
    }))
    .ok()
}

fn exact_direct_targets(
    input: &Value,
) -> Result<Vec<ClaudeExactFileTarget>, ClaudeExactFileInvocationAbstention> {
    if any_field_present(input, &PRIOR_TARGET_FIELDS)
        || any_field_present(input, &RENAMED_TARGET_FIELDS)
    {
        return Err(ClaudeExactFileInvocationAbstention::Opaque);
    }
    let object = input
        .as_object()
        .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?;
    let mut present = DIRECT_TARGET_FIELDS
        .iter()
        .chain(MULTI_TARGET_FIELDS.iter())
        .filter_map(|field| object.get_key_value(*field));
    let (field, value) = present
        .next()
        .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?;
    if present.next().is_some() {
        return Err(ClaudeExactFileInvocationAbstention::Opaque);
    }
    let paths = if DIRECT_TARGET_FIELDS.contains(&field.as_str()) {
        vec![bounded_path(
            value
                .as_str()
                .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?,
        )?]
    } else {
        let values = value
            .as_array()
            .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?;
        if values.is_empty() {
            return Err(ClaudeExactFileInvocationAbstention::Opaque);
        }
        if values.len() > CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL {
            return Err(ClaudeExactFileInvocationAbstention::CapacityExceeded);
        }
        let mut paths = Vec::with_capacity(values.len());
        for value in values {
            let path = bounded_path(
                value
                    .as_str()
                    .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?,
            )?;
            if paths.contains(&path) {
                return Err(ClaudeExactFileInvocationAbstention::Opaque);
            }
            paths.push(path);
        }
        paths
    };
    Ok(paths
        .into_iter()
        .map(|path| ClaudeExactFileTarget {
            path,
            prior_path: None,
        })
        .collect())
}

fn exact_rename_target(
    input: &Value,
) -> Result<ClaudeExactFileTarget, ClaudeExactFileInvocationAbstention> {
    if any_field_present(input, &DIRECT_TARGET_FIELDS)
        || any_field_present(input, &MULTI_TARGET_FIELDS)
    {
        return Err(ClaudeExactFileInvocationAbstention::Opaque);
    }
    let prior_path = exactly_one_path(input, &PRIOR_TARGET_FIELDS)?;
    let path = exactly_one_path(input, &RENAMED_TARGET_FIELDS)?;
    if prior_path == path {
        return Err(ClaudeExactFileInvocationAbstention::Opaque);
    }
    Ok(ClaudeExactFileTarget {
        path,
        prior_path: Some(prior_path),
    })
}

fn exactly_one_path(
    input: &Value,
    fields: &[&str],
) -> Result<String, ClaudeExactFileInvocationAbstention> {
    let object = input
        .as_object()
        .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?;
    let mut present = fields.iter().filter_map(|field| object.get(*field));
    let value = present
        .next()
        .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?;
    if present.next().is_some() {
        return Err(ClaudeExactFileInvocationAbstention::Opaque);
    }
    bounded_path(
        value
            .as_str()
            .ok_or(ClaudeExactFileInvocationAbstention::Opaque)?,
    )
}

fn bounded_path(path: &str) -> Result<String, ClaudeExactFileInvocationAbstention> {
    if path.len() > MAX_NATIVE_TARGET_BYTES {
        return Err(ClaudeExactFileInvocationAbstention::CapacityExceeded);
    }
    if path.trim().is_empty() || path.contains('\0') {
        return Err(ClaudeExactFileInvocationAbstention::Opaque);
    }
    Ok(path.to_owned())
}

fn any_field_present(input: &Value, fields: &[&str]) -> bool {
    input
        .as_object()
        .is_some_and(|object| fields.iter().any(|field| object.contains_key(*field)))
}

fn unique_complete_unit_range(
    normalized_body: &str,
    complete_unit: &str,
) -> Option<RepositoryFileInvocationTextRange> {
    let mut matches = normalized_body.match_indices(complete_unit);
    let (start, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let end = start.checked_add(complete_unit.len())?;
    Some(RepositoryFileInvocationTextRange {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(
        ordinal: u32,
        tool_name: &str,
        input: Value,
    ) -> (String, ClaudeExactFileInvocationEvidence) {
        let [invocation] = extract_exact_file_invocations(ordinal, Some(tool_name), &input)
            .into_invocations()
            .try_into()
            .unwrap();
        let body = normalized_tool_call_body(Some("call-1"), Some(tool_name), &input).unwrap();
        let evidence = invocation.evidence_in(&input, &body);
        (body, evidence)
    }

    #[test]
    fn read_edit_and_write_preserve_exact_native_semantics() {
        for (ordinal, tool_name, operation) in [
            (4, "Read", ClaudeExactFileOperation::Read),
            (5, "Edit", ClaudeExactFileOperation::Edit),
            (6, "Write", ClaudeExactFileOperation::Write),
        ] {
            let (body, evidence) = exact(
                ordinal,
                tool_name,
                serde_json::json!({"file_path": "src/lib.rs"}),
            );
            assert_eq!(evidence.invocation.invocation_ordinal, ordinal);
            assert_eq!(evidence.invocation.tool_name, tool_name);
            assert_eq!(evidence.invocation.operation, operation);
            assert_eq!(evidence.invocation.target.path, "src/lib.rs");
            assert_eq!(
                evidence.invocation.invocation_kind(),
                match operation {
                    ClaudeExactFileOperation::Read => RepositoryFileInvocationKind::Read,
                    ClaudeExactFileOperation::Edit => RepositoryFileInvocationKind::Modify,
                    ClaudeExactFileOperation::Write => RepositoryFileInvocationKind::Write,
                    _ => unreachable!(),
                }
            );
            let range = evidence.normalized_text_range.unwrap();
            assert_eq!(
                &body[range.start as usize..range.end as usize],
                serde_json::to_string(&serde_json::json!({"file_path": "src/lib.rs"})).unwrap()
            );
        }
    }

    #[test]
    fn create_delete_and_rename_are_exact() {
        for (tool_name, operation) in [
            ("Create", ClaudeExactFileOperation::Create),
            ("Delete", ClaudeExactFileOperation::Delete),
        ] {
            let (_, evidence) = exact(
                0,
                tool_name,
                serde_json::json!({"path": "src/generated.rs"}),
            );
            assert_eq!(evidence.invocation.operation, operation);
            assert_eq!(evidence.invocation.target.path, "src/generated.rs");
            assert!(evidence.invocation.target.prior_path.is_none());
        }

        let input = serde_json::json!({
            "old_path": "src/old.rs",
            "new_path": "src/new.rs"
        });
        let (body, evidence) = exact(7, "Rename", input.clone());
        assert_eq!(
            evidence.invocation.operation,
            ClaudeExactFileOperation::Rename
        );
        assert_eq!(evidence.invocation.target.path, "src/new.rs");
        assert_eq!(
            evidence.invocation.target.prior_path.as_deref(),
            Some("src/old.rs")
        );
        let range = evidence.normalized_text_range.unwrap();
        assert_eq!(
            &body[range.start as usize..range.end as usize],
            serde_json::to_string(&input).unwrap()
        );
    }

    #[test]
    fn multiple_calls_keep_their_own_ordinal_name_target_and_range() {
        let (read_body, read) = exact(0, "Read", serde_json::json!({"file_path": "src/first.rs"}));
        let (edit_body, edit) = exact(1, "Edit", serde_json::json!({"file_path": "src/second.rs"}));
        assert_eq!(read.invocation.invocation_ordinal, 0);
        assert_eq!(read.invocation.tool_name, "Read");
        assert_eq!(read.invocation.target.path, "src/first.rs");
        assert_eq!(edit.invocation.invocation_ordinal, 1);
        assert_eq!(edit.invocation.tool_name, "Edit");
        assert_eq!(edit.invocation.target.path, "src/second.rs");
        let read_range = read.normalized_text_range.unwrap();
        let edit_range = edit.normalized_text_range.unwrap();
        assert_eq!(
            &read_body[read_range.start as usize..read_range.end as usize],
            serde_json::to_string(&serde_json::json!({"file_path": "src/first.rs"})).unwrap()
        );
        assert_eq!(
            &edit_body[edit_range.start as usize..edit_range.end as usize],
            serde_json::to_string(&serde_json::json!({"file_path": "src/second.rs"})).unwrap()
        );
    }

    #[test]
    fn multi_path_call_is_complete_or_abstains_without_truncation() {
        let input = serde_json::json!({"paths": ["src/a.rs", "src/b.rs"]});
        let body = normalized_tool_call_body(Some("multi"), Some("Read"), &input).unwrap();
        let evidence = extract_exact_file_invocations(9, Some("Read"), &input)
            .into_iter()
            .map(|invocation| invocation.evidence_in(&input, &body))
            .collect::<Vec<_>>();
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|item| {
            item.invocation.invocation_ordinal == 9
                && item.invocation.tool_name == "Read"
                && item.invocation.invocation_kind() == RepositoryFileInvocationKind::Read
        }));
        assert_eq!(
            evidence
                .iter()
                .map(|item| item.invocation.target.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/a.rs", "src/b.rs"]
        );
        for item in evidence {
            let range = item.normalized_text_range.unwrap();
            assert_eq!(
                &body[range.start as usize..range.end as usize],
                serde_json::to_string(&input).unwrap()
            );
        }

        let at_limit = serde_json::json!({
            "paths": (0..CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL)
                .map(|index| format!("src/{index}.rs"))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            extract_exact_file_invocations(0, Some("Read"), &at_limit).len(),
            CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL
        );
        let overflow = serde_json::json!({
            "paths": (0..=CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL)
                .map(|index| format!("src/{index}.rs"))
                .collect::<Vec<_>>()
        });
        assert!(extract_exact_file_invocations(0, Some("Read"), &overflow).is_empty());
        assert!(extract_exact_file_invocations(
            0,
            Some("Read"),
            &serde_json::json!({"paths": ["src/a.rs", "src/a.rs"]})
        )
        .is_empty());
    }

    #[test]
    fn ambiguous_or_non_file_targets_abstain_without_guesses() {
        for (tool_name, input) in [
            (
                "Read",
                serde_json::json!({"file_path": "src/a.rs", "path": "src/b.rs"}),
            ),
            (
                "Rename",
                serde_json::json!({
                    "old_path": "src/a.rs",
                    "oldPath": "src/b.rs",
                    "new_path": "src/c.rs"
                }),
            ),
            (
                "Rename",
                serde_json::json!({
                    "old_path": "src/a.rs",
                    "new_path": "src/b.rs",
                    "paths": ["src/c.rs"]
                }),
            ),
            ("Glob", serde_json::json!({"path": "src"})),
            ("Grep", serde_json::json!({"path": "src/lib.rs"})),
            (
                "apply_patch",
                serde_json::json!({"patch": "*** Update File: src/guess.rs"}),
            ),
        ] {
            assert!(extract_exact_file_invocations(0, Some(tool_name), &input).is_empty());
        }
    }

    #[test]
    fn range_selects_the_complete_input_instead_of_only_the_repeated_target() {
        let input = serde_json::json!({
            "file_path": "src/lib.rs",
            "description": "src/lib.rs"
        });
        let [invocation] = extract_exact_file_invocations(0, Some("Read"), &input)
            .into_invocations()
            .try_into()
            .unwrap();
        let body = normalized_tool_call_body(Some("call"), Some("Read"), &input).unwrap();
        let evidence = invocation.evidence_in(&input, &body);
        let range = evidence.normalized_text_range.unwrap();
        assert_eq!(
            &body[range.start as usize..range.end as usize],
            serde_json::to_string(&input).unwrap()
        );
        assert_eq!(evidence.invocation.target.path, "src/lib.rs");
    }

    #[test]
    fn range_abstains_when_the_complete_input_unit_is_not_unique() {
        let input = serde_json::json!({"file_path": "src/lib.rs"});
        let [invocation] = extract_exact_file_invocations(0, Some("Read"), &input)
            .into_invocations()
            .try_into()
            .unwrap();
        let unit = serde_json::to_string(&input).unwrap();
        let ambiguous_body = format!("{unit}{unit}");
        assert!(invocation
            .evidence_in(&input, &ambiguous_body)
            .normalized_text_range
            .is_none());
    }

    #[test]
    fn complete_input_range_is_retained_above_the_consumer_preview_cap() {
        let input = serde_json::json!({
            "file_path": "src/generated.rs",
            "content": "x".repeat(600)
        });
        let [invocation] = extract_exact_file_invocations(0, Some("Write"), &input)
            .into_invocations()
            .try_into()
            .unwrap();
        let body = normalized_tool_call_body(Some("write-call"), Some("Write"), &input).unwrap();
        let evidence = invocation.evidence_in(&input, &body);
        let range = evidence.normalized_text_range.unwrap();
        let selected = &body[range.start as usize..range.end as usize];
        assert_eq!(selected, serde_json::to_string(&input).unwrap());
        assert!(selected.len() > 512);
    }
}
