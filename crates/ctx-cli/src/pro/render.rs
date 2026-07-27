use std::io::{self, Write};

use anyhow::Result;
use ctx_pro_host_protocol::{
    EvidenceCitation, FactValue, QueryResult, ResourceKind, ResourceSelector,
};
use serde_json::{json, Value};

use super::selector_json;

pub(crate) fn query_result_json(
    payload_type: &str,
    target: &ResourceSelector,
    result: &QueryResult,
) -> Value {
    let citations: Vec<&EvidenceCitation> = result
        .records
        .iter()
        .flat_map(|record| {
            record
                .citations
                .iter()
                .chain(record.facts.iter().flat_map(|fact| fact.citations.iter()))
        })
        .collect();
    json!({
        "schema_version": 1,
        "payload_type": payload_type,
        "target": selector_json(target),
        "results": result.records,
        "citations": citations,
        "pagination": {
            "next_cursor": result.next_cursor,
            "truncated": result.truncated
        },
        "stale": result.stale,
        "suggested_next_commands": suggested_commands(target),
    })
}

pub(crate) fn print_query_result(
    payload_type: &str,
    target: &ResourceSelector,
    result: &QueryResult,
    json_output: bool,
) -> Result<()> {
    let value = query_result_json(payload_type, target, result);
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json_output {
        serde_json::to_writer_pretty(&mut output, &value)?;
        writeln!(output)?;
        return Ok(());
    }
    writeln!(
        output,
        "{}: {}",
        payload_type.replace('_', " "),
        target.value
    )?;
    writeln!(output, "results: {}", result.records.len())?;
    if result.stale {
        writeln!(output, "status: stale")?;
    }
    for record in &result.records {
        writeln!(output, "\n{}", record.resource.display)?;
        if let Some(summary) = &record.summary {
            writeln!(output, "  {summary}")?;
        }
        for fact in &record.facts {
            writeln!(
                output,
                "  {} = {} [{} / {:?}]",
                fact.predicate,
                fact_value_text(&fact.object),
                format!("{:?}", fact.confidence).to_ascii_lowercase(),
                fact.state
            )?;
            for citation in &fact.citations {
                writeln!(output, "    citation: {}", citation_text(citation))?;
            }
        }
        for citation in &record.citations {
            writeln!(output, "  citation: {}", citation_text(citation))?;
        }
    }
    if result.truncated {
        if let Some(cursor) = &result.next_cursor {
            writeln!(output, "\nnext cursor: {cursor}")?;
        } else {
            writeln!(output, "\nstatus: truncated; narrow the target")?;
        }
    }
    Ok(())
}

fn fact_value_text(value: &FactValue) -> String {
    match value {
        FactValue::Resource(resource) => resource.display.clone(),
        FactValue::Text(value) => value.clone(),
        FactValue::Integer(value) => value.to_string(),
        FactValue::Boolean(value) => value.to_string(),
        FactValue::Json(value) => value.to_string(),
    }
}

fn citation_text(citation: &EvidenceCitation) -> String {
    if let Some(event_id) = citation.event_id {
        return match citation.session_id {
            Some(session_id) => format!("ctx show event {event_id} # session {session_id}"),
            None => format!("ctx show event {event_id}"),
        };
    }
    if let Some(path) = &citation.source_path {
        return match &citation.byte_range {
            Some(range) => format!("{path}:{}-{}", range.start, range.end_exclusive),
            None => path.clone(),
        };
    }
    "canonical evidence".to_owned()
}

fn suggested_commands(target: &ResourceSelector) -> Vec<String> {
    ["facts", "timeline"]
        .into_iter()
        .map(|operation| suggested_command(operation, target))
        .collect()
}

fn suggested_command(operation: &str, target: &ResourceSelector) -> String {
    let mut command = format!(
        "ctx {operation} {} {}",
        resource_kind_cli(target.kind),
        shell_display(&target.value)
    );
    if let Some(repository) = &target.repository {
        command.push_str(" --repository ");
        command.push_str(&shell_display(repository));
    }
    if let Some(line) = target.line {
        command.push_str(" --line ");
        command.push_str(&line.to_string());
    }
    command
}

const fn resource_kind_cli(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Repository => "repository",
        ResourceKind::Checkout => "checkout",
        ResourceKind::Worktree => "worktree",
        ResourceKind::Branch => "branch",
        ResourceKind::Commit => "commit",
        ResourceKind::File => "file",
        ResourceKind::PullRequest => "pr",
        ResourceKind::Issue => "issue",
        ResourceKind::Remote => "remote",
        ResourceKind::Release => "release",
        ResourceKind::Command => "command",
        ResourceKind::Check => "check",
        ResourceKind::Session => "session",
        ResourceKind::Agent => "agent",
        ResourceKind::Run => "run",
    }
}

fn shell_display(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:@".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use ctx_pro_host_protocol::{QueryResult, ResourceKind};

    use super::*;

    #[test]
    fn json_shape_is_stable_when_empty() {
        let target = ResourceSelector {
            kind: ResourceKind::Commit,
            value: "abc".to_owned(),
            repository: None,
            line: None,
        };
        let value = query_result_json(
            "pro_facts",
            &target,
            &QueryResult {
                records: Vec::new(),
                next_cursor: None,
                truncated: false,
                stale: false,
            },
        );
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["payload_type"], "pro_facts");
        assert_eq!(value["results"], json!([]));
    }

    #[test]
    fn suggested_commands_use_cli_tokens_and_preserve_disambiguators() {
        let target = ResourceSelector {
            kind: ResourceKind::PullRequest,
            value: "owner/repo #42".to_owned(),
            repository: Some("owner/repo mirror".to_owned()),
            line: Some(17),
        };
        assert_eq!(
            suggested_commands(&target),
            vec![
                "ctx facts pr 'owner/repo #42' --repository 'owner/repo mirror' --line 17",
                "ctx timeline pr 'owner/repo #42' --repository 'owner/repo mirror' --line 17",
            ]
        );
    }

    #[test]
    fn every_suggested_resource_kind_uses_an_accepted_cli_token() {
        for (kind, token) in [
            (ResourceKind::Repository, "repository"),
            (ResourceKind::Checkout, "checkout"),
            (ResourceKind::Worktree, "worktree"),
            (ResourceKind::Branch, "branch"),
            (ResourceKind::Commit, "commit"),
            (ResourceKind::File, "file"),
            (ResourceKind::PullRequest, "pr"),
            (ResourceKind::Issue, "issue"),
            (ResourceKind::Remote, "remote"),
            (ResourceKind::Release, "release"),
            (ResourceKind::Command, "command"),
            (ResourceKind::Check, "check"),
            (ResourceKind::Session, "session"),
            (ResourceKind::Agent, "agent"),
            (ResourceKind::Run, "run"),
        ] {
            let target = ResourceSelector {
                kind,
                value: "value".to_owned(),
                repository: None,
                line: None,
            };
            assert_eq!(
                suggested_commands(&target)[0],
                format!("ctx facts {token} value")
            );
        }
    }
}
