use crate::protocol::CrpCommandInfo;
use std::collections::HashSet;
use std::path::Path;

const PROMPTS_CMD_PREFIX: &str = "prompts";

#[derive(Debug, Clone)]
pub(super) struct CustomPrompt {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) argument_hint: Option<String>,
}

pub(super) fn build_prompt_command_infos(
    dir: &Path,
    exclude: &mut HashSet<String>,
) -> Vec<CrpCommandInfo> {
    discover_prompts_in_excluding(dir, exclude)
        .into_iter()
        .filter_map(|prompt| {
            let name = format!("{PROMPTS_CMD_PREFIX}:{}", prompt.name);
            if !exclude.insert(name.clone()) {
                return None;
            }
            Some(CrpCommandInfo {
                name,
                description: prompt
                    .description
                    .or_else(|| Some("send saved prompt".to_string())),
                argument_hint: prompt.argument_hint,
            })
        })
        .collect()
}

pub(super) fn discover_prompts_in_excluding(
    dir: &Path,
    exclude: &HashSet<String>,
) -> Vec<CustomPrompt> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_md = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
        if !is_md {
            continue;
        }
        let Some(name) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if exclude.contains(&name) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (description, argument_hint, _body) = parse_frontmatter(&content);
        out.push(CustomPrompt {
            name,
            description,
            argument_hint,
        });
    }
    out.sort_by(|left, right| left.name.cmp(&right.name));
    out
}

pub(super) fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>, String) {
    let mut segments = content.split_inclusive('\n');
    let Some(first_segment) = segments.next() else {
        return (None, None, String::new());
    };
    let first_line = first_segment.trim_end_matches(['\r', '\n']);
    if first_line.trim() != "---" {
        return (None, None, content.to_string());
    }

    let mut description = None;
    let mut argument_hint = None;
    let mut consumed = first_segment.len();
    let mut frontmatter_closed = false;

    for segment in segments {
        let line = segment.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed == "---" {
            consumed += segment.len();
            frontmatter_closed = true;
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            consumed += segment.len();
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let mut value = value.trim().to_string();
            if value.len() >= 2 {
                let first = value.as_bytes()[0];
                let last = value.as_bytes()[value.len() - 1];
                if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
                    value = value[1..value.len().saturating_sub(1)].to_string();
                }
            }
            match key.trim().to_ascii_lowercase().as_str() {
                "description" => description = Some(value),
                "argument-hint" | "argument_hint" => argument_hint = Some(value),
                _ => {}
            }
        }
        consumed += segment.len();
    }

    if !frontmatter_closed {
        return (None, None, content.to_string());
    }

    let body = if consumed >= content.len() {
        String::new()
    } else {
        content[consumed..].to_string()
    };

    (description, argument_hint, body)
}
