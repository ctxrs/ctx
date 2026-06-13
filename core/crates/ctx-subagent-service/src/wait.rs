use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentWaitMode {
    Any,
    All,
}

impl AgentWaitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentWaitUntil {
    Terminal,
    Update,
}

impl AgentWaitUntil {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Update => "update",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AgentWaitDetail<'a> {
    pub agent_id: &'a str,
    pub has_current_run: bool,
    pub has_latest_result: bool,
    pub last_event_seq: i64,
}

pub fn parse_wait_mode(mode: Option<&str>) -> Result<AgentWaitMode, String> {
    match mode.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("any") => Ok(AgentWaitMode::Any),
        Some("all") => Ok(AgentWaitMode::All),
        Some(other) => Err(format!("unsupported wait mode '{other}'")),
    }
}

pub fn parse_wait_until(until: Option<&str>) -> Result<AgentWaitUntil, String> {
    match until.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("terminal") => Ok(AgentWaitUntil::Terminal),
        Some("update") => Ok(AgentWaitUntil::Update),
        Some(other) => Err(format!("unsupported wait until '{other}'")),
    }
}

pub fn normalize_wait_agent_ids(
    agent_id: Option<&str>,
    agent_ids: Option<&[String]>,
) -> Result<Vec<String>, String> {
    let raw_ids = match (agent_id, agent_ids) {
        (Some(agent_id), None) => vec![agent_id.to_string()],
        (None, Some(agent_ids)) => agent_ids.to_vec(),
        (Some(_), Some(_)) => {
            return Err("provide either agent_id or agent_ids".to_string());
        }
        (None, None) => {
            return Err("agent_id or agent_ids is required".to_string());
        }
    };
    if raw_ids.is_empty() {
        return Err("agent_ids is required".to_string());
    }
    let mut seen = HashSet::new();
    let mut normalized_ids = Vec::with_capacity(raw_ids.len());
    for raw in raw_ids {
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            return Err("agent_id cannot be empty".to_string());
        }
        if !seen.insert(trimmed.clone()) {
            return Err(format!("duplicate agent_id '{trimmed}'"));
        }
        normalized_ids.push(trimmed);
    }
    Ok(normalized_ids)
}

fn detail_satisfies_terminal(detail: &AgentWaitDetail<'_>) -> bool {
    !detail.has_current_run && detail.has_latest_result
}

fn detail_satisfies_update(detail: &AgentWaitDetail<'_>, threshold: i64) -> bool {
    detail.last_event_seq > threshold
}

pub fn wait_predicate_satisfied(
    details: &[AgentWaitDetail<'_>],
    mode: AgentWaitMode,
    until: AgentWaitUntil,
    thresholds: &HashMap<String, i64>,
) -> bool {
    let per_agent = |detail: &AgentWaitDetail<'_>| match until {
        AgentWaitUntil::Terminal => detail_satisfies_terminal(detail),
        AgentWaitUntil::Update => detail_satisfies_update(
            detail,
            thresholds.get(detail.agent_id).copied().unwrap_or_default(),
        ),
    };

    match mode {
        AgentWaitMode::Any => details.iter().any(per_agent),
        AgentWaitMode::All => details.iter().all(per_agent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wait_mode_and_until_strictly() {
        assert_eq!(parse_wait_mode(None), Ok(AgentWaitMode::Any));
        assert_eq!(parse_wait_mode(Some(" all ")), Ok(AgentWaitMode::All));
        assert_eq!(
            parse_wait_mode(Some("first"))
                .as_ref()
                .map_err(String::as_str),
            Err("unsupported wait mode 'first'")
        );

        assert_eq!(parse_wait_until(None), Ok(AgentWaitUntil::Terminal));
        assert_eq!(
            parse_wait_until(Some(" update ")),
            Ok(AgentWaitUntil::Update)
        );
        assert_eq!(
            parse_wait_until(Some("done"))
                .as_ref()
                .map_err(String::as_str),
            Err("unsupported wait until 'done'")
        );
    }

    #[test]
    fn normalizes_wait_agent_ids_strictly() {
        assert_eq!(
            normalize_wait_agent_ids(Some(" agent_1 "), None),
            Ok(vec!["agent_1".to_string()])
        );
        assert_eq!(
            normalize_wait_agent_ids(
                None,
                Some(&[" agent_1 ".to_string(), "agent_2".to_string()])
            ),
            Ok(vec!["agent_1".to_string(), "agent_2".to_string()])
        );
        assert_eq!(
            normalize_wait_agent_ids(Some("agent_1"), Some(&["agent_2".to_string()]))
                .as_ref()
                .map_err(String::as_str),
            Err("provide either agent_id or agent_ids")
        );
        assert_eq!(
            normalize_wait_agent_ids(
                None,
                Some(&["agent_1".to_string(), " agent_1 ".to_string()])
            )
            .as_ref()
            .map_err(String::as_str),
            Err("duplicate agent_id 'agent_1'")
        );
    }

    #[test]
    fn evaluates_wait_predicates() {
        let details = [
            AgentWaitDetail {
                agent_id: "agent_1",
                has_current_run: false,
                has_latest_result: true,
                last_event_seq: 12,
            },
            AgentWaitDetail {
                agent_id: "agent_2",
                has_current_run: true,
                has_latest_result: false,
                last_event_seq: 9,
            },
        ];
        let thresholds = HashMap::from([("agent_1".to_string(), 10), ("agent_2".to_string(), 10)]);

        assert!(wait_predicate_satisfied(
            &details,
            AgentWaitMode::Any,
            AgentWaitUntil::Terminal,
            &thresholds
        ));
        assert!(!wait_predicate_satisfied(
            &details,
            AgentWaitMode::All,
            AgentWaitUntil::Terminal,
            &thresholds
        ));
        assert!(wait_predicate_satisfied(
            &details,
            AgentWaitMode::Any,
            AgentWaitUntil::Update,
            &thresholds
        ));
        assert!(!wait_predicate_satisfied(
            &details,
            AgentWaitMode::All,
            AgentWaitUntil::Update,
            &thresholds
        ));
    }
}
