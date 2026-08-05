use ctx_pro_host_protocol::{BlameResult, ResolvedBlameTarget};
use serde_json::Value;

use crate::ui::{Document, RenderContext, Token};

use super::{
    commit, evidence, file,
    layout::{push_authored, push_heading, push_notice},
    pull_request, target, BlameEvidenceContext,
};
use crate::pro::BlameResultFreshness;

pub(super) fn render(
    result: &BlameResult,
    freshness: Option<BlameResultFreshness>,
    context: &RenderContext,
    evidence_context: &BlameEvidenceContext,
) -> Document {
    let mut document = Document::new();
    let serialized = serde_json::to_value(result).unwrap_or(Value::Null);
    if render_contract_summary(&mut document, context, &serialized, freshness) {
        document.push_blank();
    }
    target::render(&mut document, context, result);
    document.push_blank();
    match &result.target {
        ResolvedBlameTarget::File { .. } => file::render(&mut document, context, &result.matches),
        ResolvedBlameTarget::Commit { commit, .. } => {
            commit::render(&mut document, context, commit, &result.matches)
        }
        ResolvedBlameTarget::PullRequest { .. } => {
            pull_request::render(&mut document, context, &result.matches)
        }
    }
    evidence::render_continuation(&mut document, context, result);
    evidence::render_list(&mut document, context, result);
    if evidence_context.is_available() {
        evidence::render_previews(&mut document, context, evidence_context.model());
    }
    document
}

/// Renders only host-integrated outcome fields. The protocol result remains the
/// semantic authority; this presentation seam deliberately does not infer an
/// attribution from matches when an older helper omits `outcome`.
fn render_contract_summary(
    document: &mut Document,
    context: &RenderContext,
    value: &Value,
    freshness: Option<BlameResultFreshness>,
) -> bool {
    let Some(attribution) = value
        .pointer("/outcome/attribution")
        .and_then(Value::as_str)
    else {
        return false;
    };
    let Some(heading) = outcome_heading(attribution) else {
        return false;
    };
    push_heading(document, 0, heading);

    if matches!(
        value.pointer("/target/kind").and_then(Value::as_str),
        Some("file" | "pull_request")
    ) {
        if let Some(coverage) = coverage_text(value) {
            push_authored(document, context, 2, &coverage, Token::Text);
        }
    }

    let stale = freshness == Some(BlameResultFreshness::StaleCommitted)
        || value.pointer("/freshness/state").and_then(Value::as_str) == Some("stale_committed");
    if stale {
        push_notice(
            document,
            context,
            0,
            "Result is from stale committed history; newer Core history may still be materializing.",
        );
    }
    true
}

fn outcome_heading(attribution: &str) -> Option<&'static str> {
    match attribution {
        "proven" => Some("Producer proven"),
        "possible" => Some("Possible producer found"),
        "conflicting" => Some("Producer evidence conflicts"),
        "none" => Some("No producer proven"),
        _ => None,
    }
}

fn coverage_text(value: &Value) -> Option<String> {
    let coverage = value.pointer("/outcome/coverage")?;
    let evaluated = coverage.get("evaluated")?.as_u64()?;
    let unit = coverage.get("unit")?.as_str()?;
    let units = coverage_units(unit, evaluated);
    let proven = coverage.get("proven")?.as_u64()?;
    let possible = coverage.get("possible")?.as_u64()?;
    let conflicting = coverage.get("conflicting")?.as_u64()?;
    let none = coverage.get("none")?.as_u64()?;
    Some(format!(
        "{evaluated} {units} evaluated on this page · {proven} proven · {possible} possible · {conflicting} conflicting · {none} none"
    ))
}

fn coverage_units(unit: &str, evaluated: u64) -> String {
    let singular = evaluated == 1;
    match unit {
        "committed_line" => if singular {
            "committed line"
        } else {
            "committed lines"
        }
        .to_owned(),
        "commit_fact" => if singular {
            "commit fact"
        } else {
            "commit facts"
        }
        .to_owned(),
        "pull_request_relationship" => if singular {
            "pull request relationship"
        } else {
            "pull request relationships"
        }
        .to_owned(),
        _ => unit.replace('_', " "),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context() -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 88).color(ColorMode::Never))
    }

    #[test]
    fn integrated_outcome_renders_page_coverage_and_only_stale_freshness() {
        let value = json!({
            "target": {"kind": "file"},
            "outcome": {
                "attribution": "conflicting",
                "coverage": {
                    "unit": "committed_line",
                    "evaluated": 10,
                    "proven": 7,
                    "possible": 1,
                    "conflicting": 1,
                    "none": 1
                }
            },
            "freshness": {"state": "stale_committed"}
        });
        let mut document = Document::new();
        assert!(render_contract_summary(
            &mut document,
            &context(),
            &value,
            None
        ));
        assert_eq!(
            document.render_plain(),
            include_str!("../../../testdata/pro/blame_outcome_contract.golden.txt")
        );

        let mut current = value;
        current["freshness"]["state"] = json!("current");
        let mut document = Document::new();
        assert!(render_contract_summary(
            &mut document,
            &context(),
            &current,
            None
        ));
        assert!(!document.render_plain().contains("fresh"));
    }

    #[test]
    fn absent_or_unknown_outcome_is_not_inferred_from_matches() {
        for value in [
            json!({"matches": []}),
            json!({"outcome": {"attribution": "future_state"}, "matches": []}),
        ] {
            let mut document = Document::new();
            assert!(!render_contract_summary(
                &mut document,
                &context(),
                &value,
                None
            ));
            assert!(document.render_plain().is_empty());
        }
    }
}
