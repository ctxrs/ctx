use std::collections::HashSet;

use ctx_session_tools::model_resolution::normalize_effort_id;

pub const DEFAULT_MAX_SUBAGENTS_PER_CALL: usize = 10;
pub const DEFAULT_MAX_ACTIVE_SUBAGENTS_PER_PARENT: usize = 12;
pub const DEFAULT_MAX_SUBAGENT_DEPTH: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentWorktreeSelection {
    Inherit,
    New,
}

pub fn resolve_max_subagents_per_call(configured: Option<u32>) -> usize {
    configured
        .filter(|value| *value > 0)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_MAX_SUBAGENTS_PER_CALL)
}

pub fn parse_subagent_worktree(value: Option<&str>) -> Result<SubagentWorktreeSelection, String> {
    let trimmed = value.map(str::trim).filter(|raw| !raw.is_empty());
    match trimmed {
        Some("inherit") => Ok(SubagentWorktreeSelection::Inherit),
        Some("new") => Ok(SubagentWorktreeSelection::New),
        Some(_) => Err("worktree must be 'inherit' or 'new'".to_string()),
        None => Err("worktree is required".to_string()),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SubagentRequestAgent<'a> {
    pub prompt: &'a str,
    pub label: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub model: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_reasoning_effort(value: Option<&str>) -> Option<String> {
    normalize_optional(value)
        .map(normalize_effort_id)
        .filter(|value| !value.is_empty())
}

pub fn build_subagent_request_json(agents: &[SubagentRequestAgent<'_>]) -> serde_json::Value {
    let mut items = Vec::with_capacity(agents.len());
    for (idx, agent) in agents.iter().enumerate() {
        let prompt = agent.prompt.trim();
        let mut obj = serde_json::Map::new();
        obj.insert(
            "position".to_string(),
            serde_json::Value::Number(serde_json::Number::from(idx as u64)),
        );
        obj.insert(
            "prompt_length".to_string(),
            serde_json::Value::Number(serde_json::Number::from(prompt.chars().count() as u64)),
        );
        if let Some(label) = normalize_optional(agent.label) {
            obj.insert(
                "label".to_string(),
                serde_json::Value::String(label.to_string()),
            );
        }
        if let Some(harness) = normalize_optional(agent.harness) {
            obj.insert(
                "harness".to_string(),
                serde_json::Value::String(harness.to_string()),
            );
        }
        if let Some(model) = normalize_optional(agent.model) {
            obj.insert(
                "model".to_string(),
                serde_json::Value::String(model.to_string()),
            );
        }
        if let Some(reasoning_effort) = normalize_reasoning_effort(agent.reasoning_effort) {
            obj.insert(
                "reasoning_effort".to_string(),
                serde_json::Value::String(reasoning_effort),
            );
        }
        items.push(serde_json::Value::Object(obj));
    }

    serde_json::json!({
        "agents_total": agents.len(),
        "agents": items,
    })
}

pub fn collect_provider_ids(
    agents: &[SubagentRequestAgent<'_>],
    parent_provider_id: &str,
) -> Result<HashSet<String>, String> {
    let mut provider_ids = HashSet::new();
    for agent in agents {
        let provider_id = agent.harness.unwrap_or(parent_provider_id).trim();
        if provider_id.is_empty() {
            return Err("harness is required".to_string());
        }
        provider_ids.insert(provider_id.to_string());
    }
    Ok(provider_ids)
}

pub fn normalize_subagent_labels(
    agents: &[SubagentRequestAgent<'_>],
) -> Result<Vec<String>, String> {
    let mut labels = Vec::with_capacity(agents.len());
    let mut seen_labels = HashSet::new();
    for (idx, agent) in agents.iter().enumerate() {
        let label = normalize_optional(agent.label)
            .ok_or_else(|| format!("agent {} label is required", idx + 1))?;
        if !seen_labels.insert(label.to_string()) {
            return Err(format!("duplicate subagent label '{label}'"));
        }
        labels.push(label.to_string());
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_max_subagents_per_call_from_positive_config() {
        assert_eq!(resolve_max_subagents_per_call(Some(3)), 3);
        assert_eq!(
            resolve_max_subagents_per_call(Some(0)),
            DEFAULT_MAX_SUBAGENTS_PER_CALL
        );
        assert_eq!(
            resolve_max_subagents_per_call(None),
            DEFAULT_MAX_SUBAGENTS_PER_CALL
        );
    }

    #[test]
    fn parses_subagent_worktree_selection_strictly() {
        assert_eq!(
            parse_subagent_worktree(Some(" inherit ")),
            Ok(SubagentWorktreeSelection::Inherit)
        );
        assert_eq!(
            parse_subagent_worktree(Some("new")),
            Ok(SubagentWorktreeSelection::New)
        );
        assert_eq!(
            parse_subagent_worktree(Some("reuse"))
                .as_ref()
                .map_err(String::as_str),
            Err("worktree must be 'inherit' or 'new'")
        );
        assert_eq!(
            parse_subagent_worktree(None)
                .as_ref()
                .map_err(String::as_str),
            Err("worktree is required")
        );
    }

    #[test]
    fn builds_subagent_request_json_with_normalized_optional_fields() {
        let request = build_subagent_request_json(&[
            SubagentRequestAgent {
                prompt: " hello ",
                label: Some(" alpha "),
                harness: Some(" codex "),
                model: Some(" gpt-5.4 "),
                reasoning_effort: Some("extra high"),
            },
            SubagentRequestAgent {
                prompt: "second",
                label: Some(""),
                harness: None,
                model: None,
                reasoning_effort: None,
            },
        ]);

        assert_eq!(
            request,
            serde_json::json!({
                "agents_total": 2,
                "agents": [
                    {
                        "position": 0,
                        "prompt_length": 5,
                        "label": "alpha",
                        "harness": "codex",
                        "model": "gpt-5.4",
                        "reasoning_effort": "xhigh",
                    },
                    {
                        "position": 1,
                        "prompt_length": 6,
                    }
                ],
            })
        );
    }

    #[test]
    fn collects_provider_ids_with_parent_default_and_empty_rejection() {
        let agents = [
            SubagentRequestAgent {
                prompt: "one",
                label: None,
                harness: None,
                model: None,
                reasoning_effort: None,
            },
            SubagentRequestAgent {
                prompt: "two",
                label: None,
                harness: Some(" gemini "),
                model: None,
                reasoning_effort: None,
            },
        ];
        let ids = collect_provider_ids(&agents, "codex").expect("provider ids should resolve");
        assert!(ids.contains("codex"));
        assert!(ids.contains("gemini"));

        let agents = [SubagentRequestAgent {
            prompt: "one",
            label: None,
            harness: Some(" "),
            model: None,
            reasoning_effort: None,
        }];
        assert_eq!(
            collect_provider_ids(&agents, "codex")
                .as_ref()
                .map_err(String::as_str),
            Err("harness is required")
        );
    }

    #[test]
    fn normalizes_subagent_labels_strictly() {
        let agents = [
            SubagentRequestAgent {
                prompt: "one",
                label: Some(" alpha "),
                harness: None,
                model: None,
                reasoning_effort: None,
            },
            SubagentRequestAgent {
                prompt: "two",
                label: Some("beta"),
                harness: None,
                model: None,
                reasoning_effort: None,
            },
        ];
        assert_eq!(
            normalize_subagent_labels(&agents),
            Ok(vec!["alpha".to_string(), "beta".to_string()])
        );

        let agents = [SubagentRequestAgent {
            prompt: "one",
            label: Some(" "),
            harness: None,
            model: None,
            reasoning_effort: None,
        }];
        assert_eq!(
            normalize_subagent_labels(&agents)
                .as_ref()
                .map_err(String::as_str),
            Err("agent 1 label is required")
        );

        let agents = [
            SubagentRequestAgent {
                prompt: "one",
                label: Some("alpha"),
                harness: None,
                model: None,
                reasoning_effort: None,
            },
            SubagentRequestAgent {
                prompt: "two",
                label: Some(" alpha "),
                harness: None,
                model: None,
                reasoning_effort: None,
            },
        ];
        assert_eq!(
            normalize_subagent_labels(&agents)
                .as_ref()
                .map_err(String::as_str),
            Err("duplicate subagent label 'alpha'")
        );
    }
}
