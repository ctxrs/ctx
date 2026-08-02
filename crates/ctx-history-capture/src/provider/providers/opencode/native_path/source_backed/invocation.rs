use ctx_history_core::{RepositoryFileInvocationKind, RepositoryFileInvocationTextRange};
use serde_json::{Map, Value};

use crate::repository_attribution::UnscopedRepositoryFileInvocationEvidence;

pub(super) const MAX_STRICT_FILE_INVOCATIONS: usize = 64;
const MAX_STRICT_PATH_BYTES: usize = 16 * 1024;
const MAX_STRICT_TARGET_BYTES: usize = 64 * 1024;
const MAX_STRICT_NATIVE_UNIT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StrictInvocationAbstention {
    Capacity,
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StrictToolCallProjection {
    pub(super) normalized_body: String,
    pub(super) file_invocations: Vec<UnscopedRepositoryFileInvocationEvidence>,
    pub(super) abstention: Option<StrictInvocationAbstention>,
}

pub(super) fn strict_tool_call_projection(
    body: &Value,
    lexical_prefix: &str,
) -> Result<StrictToolCallProjection, serde_json::Error> {
    let mut projected = StrictToolCallProjection {
        normalized_body: lexical_prefix.to_owned(),
        file_invocations: Vec::new(),
        abstention: None,
    };
    let extraction = strict_native_invocation(body);
    let (tool_name, kind, targets) = match extraction {
        StrictExtraction::Exact {
            tool_name,
            kind,
            targets,
        } => (tool_name, kind, targets),
        StrictExtraction::NoTarget => return Ok(projected),
        StrictExtraction::Abstained(reason) => {
            projected.abstention = Some(reason);
            return Ok(projected);
        }
    };
    let exact_unit = serde_json::to_string(body)?;
    if exact_unit.len() > MAX_STRICT_NATIVE_UNIT_BYTES {
        projected.abstention = Some(StrictInvocationAbstention::Capacity);
        return Ok(projected);
    }
    let separator = usize::from(!projected.normalized_body.is_empty());
    let Some(start) = projected.normalized_body.len().checked_add(separator) else {
        projected.abstention = Some(StrictInvocationAbstention::Capacity);
        return Ok(projected);
    };
    let Some(end) = start.checked_add(exact_unit.len()) else {
        projected.abstention = Some(StrictInvocationAbstention::Capacity);
        return Ok(projected);
    };
    let Some(range) = strict_text_range(start, end) else {
        projected.abstention = Some(StrictInvocationAbstention::Capacity);
        return Ok(projected);
    };
    if separator != 0 {
        projected.normalized_body.push('\n');
    }
    projected.normalized_body.push_str(&exact_unit);
    projected.file_invocations = targets
        .into_iter()
        .map(
            |(path, prior_path)| UnscopedRepositoryFileInvocationEvidence {
                operation_ordinal: 0,
                path,
                prior_path,
                kind,
                tool_name: Some(tool_name.to_owned()),
                normalized_text_range: Some(range),
            },
        )
        .collect();
    Ok(projected)
}

enum StrictExtraction<'a> {
    Exact {
        tool_name: &'a str,
        kind: RepositoryFileInvocationKind,
        targets: Vec<(String, Option<String>)>,
    },
    NoTarget,
    Abstained(StrictInvocationAbstention),
}

fn strict_native_invocation(body: &Value) -> StrictExtraction<'_> {
    let Some(object) = body.as_object() else {
        return StrictExtraction::NoTarget;
    };
    if object.get("type").and_then(Value::as_str) != Some("tool") {
        return if body_has_target(body) {
            StrictExtraction::Abstained(StrictInvocationAbstention::Opaque)
        } else {
            StrictExtraction::NoTarget
        };
    }
    if ["name", "tool_name"]
        .into_iter()
        .any(|key| object.contains_key(key))
    {
        return StrictExtraction::Abstained(StrictInvocationAbstention::Opaque);
    }
    let Some(tool_name) = object.get("tool").and_then(exact_tool_name) else {
        return if body_has_target(body) {
            StrictExtraction::Abstained(StrictInvocationAbstention::Opaque)
        } else {
            StrictExtraction::NoTarget
        };
    };
    let Some(kind) = strict_file_action(tool_name) else {
        return if body_has_target(body) {
            StrictExtraction::Abstained(StrictInvocationAbstention::Opaque)
        } else {
            StrictExtraction::NoTarget
        };
    };
    if ["input", "arguments"]
        .into_iter()
        .any(|key| object.contains_key(key))
    {
        return StrictExtraction::Abstained(StrictInvocationAbstention::Opaque);
    }
    let Some(arguments) = body.pointer("/state/input").and_then(Value::as_object) else {
        return StrictExtraction::Abstained(StrictInvocationAbstention::Opaque);
    };
    let targets = match kind {
        RepositoryFileInvocationKind::Rename => strict_rename_target(arguments),
        _ => strict_targets(arguments),
    };
    match targets {
        StrictTargets::Exact(targets) => StrictExtraction::Exact {
            tool_name,
            kind,
            targets,
        },
        StrictTargets::Opaque => StrictExtraction::Abstained(StrictInvocationAbstention::Opaque),
        StrictTargets::Capacity => {
            StrictExtraction::Abstained(StrictInvocationAbstention::Capacity)
        }
    }
}

fn strict_file_action(tool_name: &str) -> Option<RepositoryFileInvocationKind> {
    match tool_name {
        "read" | "read_file" => Some(RepositoryFileInvocationKind::Read),
        "edit" | "edit_file" => Some(RepositoryFileInvocationKind::Modify),
        "write" | "write_file" => Some(RepositoryFileInvocationKind::Write),
        "create" | "create_file" => Some(RepositoryFileInvocationKind::Create),
        "delete" | "delete_file" => Some(RepositoryFileInvocationKind::Delete),
        "rename" | "rename_file" => Some(RepositoryFileInvocationKind::Rename),
        _ => None,
    }
}

enum StrictTargets {
    Exact(Vec<(String, Option<String>)>),
    Opaque,
    Capacity,
}

fn strict_targets(arguments: &Map<String, Value>) -> StrictTargets {
    if has_any(
        arguments,
        &[
            "old_path",
            "oldPath",
            "source",
            "from",
            "new_path",
            "newPath",
            "destination",
            "to",
        ],
    ) {
        return StrictTargets::Opaque;
    }
    let direct = match exactly_one_alias_string(arguments, &["path", "file_path", "filePath"]) {
        Ok(value) => value,
        Err(reason) => return reason,
    };
    let files = match strict_path_array(arguments.get("files")) {
        Ok(value) => value,
        Err(reason) => return reason,
    };
    let paths = match strict_path_array(arguments.get("paths")) {
        Ok(value) => value,
        Err(reason) => return reason,
    };
    if usize::from(direct.is_some()) + usize::from(files.is_some()) + usize::from(paths.is_some())
        != 1
    {
        return StrictTargets::Opaque;
    }
    let targets = direct
        .map(|path| vec![path])
        .or(files)
        .or(paths)
        .unwrap_or_default();
    checked_targets(targets.into_iter().map(|path| (path, None)).collect())
}

fn strict_rename_target(arguments: &Map<String, Value>) -> StrictTargets {
    if has_any(arguments, &["files", "paths"])
        || alias_count(arguments, &["old_path", "oldPath", "source", "from"]) != 1
        || alias_count(
            arguments,
            &["new_path", "newPath", "destination", "to", "path"],
        ) != 1
    {
        return StrictTargets::Opaque;
    }
    let Ok(Some(prior_path)) =
        exactly_one_alias_string(arguments, &["old_path", "oldPath", "source", "from"])
    else {
        return StrictTargets::Opaque;
    };
    let Ok(Some(path)) = exactly_one_alias_string(
        arguments,
        &["new_path", "newPath", "destination", "to", "path"],
    ) else {
        return StrictTargets::Opaque;
    };
    checked_targets(vec![(path, Some(prior_path))])
}

fn exactly_one_alias_string(
    arguments: &Map<String, Value>,
    keys: &[&str],
) -> Result<Option<String>, StrictTargets> {
    let mut present = keys.iter().filter_map(|key| arguments.get(*key));
    let Some(value) = present.next() else {
        return Ok(None);
    };
    if present.next().is_some() {
        return Err(StrictTargets::Opaque);
    }
    Ok(Some(
        value
            .as_str()
            .and_then(bounded_path)
            .ok_or(StrictTargets::Opaque)?
            .to_owned(),
    ))
}

fn strict_path_array(value: Option<&Value>) -> Result<Option<Vec<String>>, StrictTargets> {
    let Some(values) = value.and_then(Value::as_array) else {
        return if value.is_some() {
            Err(StrictTargets::Opaque)
        } else {
            Ok(None)
        };
    };
    if values.is_empty() {
        return Err(StrictTargets::Opaque);
    }
    if values.len() > MAX_STRICT_FILE_INVOCATIONS {
        return Err(StrictTargets::Capacity);
    }
    let mut paths = Vec::with_capacity(values.len());
    for value in values {
        paths.push(
            value
                .as_str()
                .and_then(bounded_path)
                .ok_or(StrictTargets::Opaque)?
                .to_owned(),
        );
    }
    Ok(Some(paths))
}

fn checked_targets(targets: Vec<(String, Option<String>)>) -> StrictTargets {
    if targets.is_empty() {
        return StrictTargets::Opaque;
    }
    if targets.len() > MAX_STRICT_FILE_INVOCATIONS {
        return StrictTargets::Capacity;
    }
    let bytes = targets.iter().try_fold(0usize, |total, (path, prior)| {
        total
            .checked_add(path.len())?
            .checked_add(prior.as_deref().map_or(0, str::len))
    });
    if bytes.is_none_or(|bytes| bytes > MAX_STRICT_TARGET_BYTES) {
        StrictTargets::Capacity
    } else {
        StrictTargets::Exact(targets)
    }
}

pub(super) fn strict_text_range(
    start: usize,
    end: usize,
) -> Option<RepositoryFileInvocationTextRange> {
    (start < end).then_some(())?;
    Some(RepositoryFileInvocationTextRange {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

fn exact_tool_name(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
}

fn bounded_path(value: &str) -> Option<&str> {
    (!value.trim().is_empty() && value.len() <= MAX_STRICT_PATH_BYTES && !value.contains('\0'))
        .then_some(value)
}

fn alias_count(arguments: &Map<String, Value>, keys: &[&str]) -> usize {
    keys.iter()
        .filter(|key| arguments.contains_key(**key))
        .count()
}

fn has_any(arguments: &Map<String, Value>, keys: &[&str]) -> bool {
    alias_count(arguments, keys) != 0
}

fn target_bearing(arguments: &Map<String, Value>) -> bool {
    has_any(
        arguments,
        &[
            "path",
            "file_path",
            "filePath",
            "files",
            "paths",
            "old_path",
            "oldPath",
            "new_path",
            "newPath",
        ],
    )
}

fn body_has_target(body: &Value) -> bool {
    body.as_object().is_some_and(target_bearing)
        || body
            .pointer("/state/input")
            .and_then(Value::as_object)
            .is_some_and(target_bearing)
        || body
            .get("input")
            .and_then(Value::as_object)
            .is_some_and(target_bearing)
        || body
            .get("arguments")
            .and_then(Value::as_object)
            .is_some_and(target_bearing)
}
