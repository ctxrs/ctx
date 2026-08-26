use ctx_history_cli::{format_bytes, format_count, provider_display_name};
use ctx_history_read_application::{HistoryHealthReport, HistoryRootCoverage};

pub(super) fn history_health_fields(
    health: Option<&HistoryHealthReport>,
) -> Vec<(&'static str, String)> {
    let Some(health) = health else {
        return Vec::new();
    };
    let mut values = Vec::new();
    if !health.contributing_agent_histories.is_empty() {
        values.push((
            "Agent histories",
            health
                .contributing_agent_histories
                .iter()
                .map(|provider| provider_display_name(provider))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(roots) = nonempty_root_coverage(health) {
        let mut coverage = counted(roots.included, "included root", "included roots");
        if roots.partial > 0 {
            coverage.push_str(", ");
            coverage.push_str(&counted(roots.partial, "partial root", "partial roots"));
        }
        if roots.excluded > 0 {
            coverage.push_str(", ");
            coverage.push_str(&counted(roots.excluded, "excluded root", "excluded roots"));
        }
        if roots.unknown > 0 {
            coverage.push_str(", ");
            coverage.push_str(&counted(roots.unknown, "unknown root", "unknown roots"));
        }
        values.push(("Roots", coverage));
    }
    values.extend([
        ("Sessions", format_count(health.sessions)),
        ("Messages", format_count(health.messages)),
        ("Tool calls", format_count(health.tool_calls)),
    ]);
    let mut data = format!("{} processed", format_bytes(health.data.processed));
    if let Some(excluded) = health.data.excluded.filter(|excluded| *excluded > 0) {
        data.push_str(&format!(", {} excluded", format_bytes(excluded)));
    } else if health.is_partial() && health.data.excluded.is_none() {
        data.push_str(", excluded size unknown");
    }
    values.push(("Data", data));
    values
}

pub(super) fn setup_history_fields(
    health: Option<&HistoryHealthReport>,
) -> Vec<(&'static str, String)> {
    let Some(health) = health else {
        return Vec::new();
    };
    let mut values = Vec::new();
    if !health.contributing_agent_histories.is_empty() {
        values.push((
            "Agent histories",
            health
                .contributing_agent_histories
                .iter()
                .map(|provider| provider_display_name(provider))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(roots) = nonempty_root_coverage(health) {
        let mut coverage = counted(roots.included, "included root", "included roots");
        if roots.partial > 0 {
            coverage.push_str(&format!(
                ", {}",
                counted(roots.partial, "partial root", "partial roots")
            ));
        }
        if roots.excluded > 0 {
            coverage.push_str(&format!(
                ", {}",
                counted(roots.excluded, "excluded root", "excluded roots")
            ));
        }
        if roots.unknown > 0 {
            coverage.push_str(&format!(
                ", {}",
                counted(roots.unknown, "unknown root", "unknown roots")
            ));
        }
        values.push(("Roots", coverage));
    }
    values.push((
        "Indexed",
        format!(
            "{}; {}; {}; {} processed",
            counted(health.sessions, "session", "sessions"),
            counted(health.messages, "message", "messages"),
            counted(health.tool_calls, "tool call", "tool calls"),
            format_bytes(health.data.processed),
        ),
    ));
    if let Some(excluded) = health.data.excluded.filter(|excluded| *excluded > 0) {
        values.push(("Excluded data", format_bytes(excluded)));
    } else if health.is_partial() && health.data.excluded.is_none() {
        values.push(("Excluded data", "size unknown".to_owned()));
    }
    values
}

pub(super) fn history_partial_cause(health: Option<&HistoryHealthReport>) -> Option<String> {
    let health = health.filter(|health| health.is_partial())?;
    let roots = health.provider_roots.unwrap_or_default();
    let mut causes = Vec::new();
    if roots.partial > 0 {
        causes.push(format!(
            "{} only partially indexed",
            counted(roots.partial, "provider root was", "provider roots were")
        ));
    }
    if roots.excluded > 0 {
        causes.push(format!(
            "{} excluded",
            counted(roots.excluded, "provider root was", "provider roots were")
        ));
    }
    if roots.unknown > 0 {
        causes.push(format!(
            "{} could not be assessed",
            counted(roots.unknown, "provider root", "provider roots")
        ));
    }
    if health.source_failures > 0 {
        causes.push(format!(
            "{} could not be read",
            counted(health.source_failures, "history file", "history files")
        ));
    }
    if health.rejected_records > 0 {
        causes.push(format!(
            "{} excluded",
            counted(
                health.rejected_records,
                "history record was",
                "history records were"
            )
        ));
    }
    Some(causes.join("; "))
}

fn nonempty_root_coverage(health: &HistoryHealthReport) -> Option<HistoryRootCoverage> {
    health.provider_roots.filter(|roots| {
        roots.included > 0 || roots.partial > 0 || roots.excluded > 0 || roots.unknown > 0
    })
}

pub(super) fn counted(count: u64, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", format_count(count))
}

#[cfg(test)]
mod tests {
    use ctx_history_read_application::{
        HistoryDataCoverage, HistoryHealthReport, HistoryRootCoverage,
    };

    use super::*;

    #[test]
    fn fields_omit_empty_agent_histories_and_keep_processed_data_distinct() {
        let health = HistoryHealthReport {
            provider_roots: Some(HistoryRootCoverage {
                included: 2,
                partial: 0,
                excluded: 0,
                unknown: 0,
            }),
            sessions: 3,
            messages: 1_000,
            tool_calls: 40,
            data: HistoryDataCoverage {
                processed: 4 * 1024 * 1024,
                excluded: Some(0),
            },
            ..HistoryHealthReport::default()
        };

        let fields = history_health_fields(Some(&health));
        assert_eq!(
            fields,
            vec![
                ("Roots", "2 included roots".to_owned()),
                ("Sessions", "3".to_owned()),
                ("Messages", "1,000".to_owned()),
                ("Tool calls", "40".to_owned()),
                ("Data", "4.0 MiB processed".to_owned()),
            ]
        );
    }

    #[test]
    fn setup_summary_does_not_duplicate_the_status_history_table() {
        let health = HistoryHealthReport {
            contributing_agent_histories: vec!["codex".to_owned()],
            sessions: 3,
            messages: 1_000,
            tool_calls: 40,
            data: HistoryDataCoverage {
                processed: 4 * 1024 * 1024,
                excluded: Some(0),
            },
            ..HistoryHealthReport::default()
        };

        assert_eq!(
            setup_history_fields(Some(&health)),
            vec![
                ("Agent histories", "Codex".to_owned()),
                (
                    "Indexed",
                    "3 sessions; 1,000 messages; 40 tool calls; 4.0 MiB processed".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn partial_cause_and_unknown_excluded_size_are_explicit() {
        let health = HistoryHealthReport {
            provider_roots: Some(HistoryRootCoverage {
                included: 2,
                partial: 1,
                excluded: 1,
                unknown: 1,
            }),
            data: HistoryDataCoverage {
                processed: 1024,
                excluded: None,
            },
            source_failures: 2,
            rejected_records: 3,
            ..HistoryHealthReport::default()
        };

        assert_eq!(
            history_partial_cause(Some(&health)).as_deref(),
            Some(
                "1 provider root was only partially indexed; 1 provider root was excluded; 1 provider root could not be assessed; 2 history files could not be read; 3 history records were excluded"
            )
        );
        assert_eq!(
            history_health_fields(Some(&health)).last(),
            Some(&(
                "Data",
                "1.0 KiB processed, excluded size unknown".to_owned()
            ))
        );
    }
}
