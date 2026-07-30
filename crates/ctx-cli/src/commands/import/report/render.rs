use serde_json::Value;

use crate::commands::import::{import_report_analytics_outcome, ImportReport, ImportTotals};
use crate::progress::{format_bytes, format_count};
use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, RenderContext, Span,
    Token,
};

const MAX_FAILURE_DETAILS: usize = 3;

#[derive(Debug, Clone)]
pub(crate) struct ImportCompletion {
    state: CompletionState,
    totals: ImportTotals,
    source_failures: Vec<SourceFailure>,
}

impl ImportCompletion {
    pub(crate) fn from_report(report: &ImportReport) -> Self {
        let outcome = import_report_analytics_outcome(&report.totals).0;
        let state = match outcome {
            "failure" => CompletionState::Failure,
            "success"
                if report.totals.work_result.as_str() == "no_op"
                    && !has_visible_history(&report.totals) =>
            {
                CompletionState::Empty
            }
            "success" => CompletionState::Success,
            _ => CompletionState::Partial,
        };
        let source_failures = report.sources.iter().filter_map(source_failure).collect();

        Self {
            state,
            totals: report.totals.clone(),
            source_failures,
        }
    }

    pub(crate) fn render(&self, context: &RenderContext) -> Document {
        let (outcome_state, title, detail) = match self.state {
            CompletionState::Empty => (
                OutcomeState::Neutral,
                "No history changes found",
                Some("The configured sources did not add searchable history."),
            ),
            CompletionState::Success if self.totals.work_result.as_str() == "no_op" => {
                (OutcomeState::Success, "History is already up to date", None)
            }
            CompletionState::Success => (OutcomeState::Success, "History import completed", None),
            CompletionState::Partial => (
                OutcomeState::Warning,
                "History import completed with issues",
                Some("Successful imports were retained."),
            ),
            CompletionState::Failure => (
                OutcomeState::Error,
                "History import failed",
                Some("No source completed successfully."),
            ),
        };
        let mut document = outcome(
            context,
            Outcome {
                state: outcome_state,
                title,
                detail,
            },
        );

        let (summary_title, summary) = self.summary_fields();
        append_fields(&mut document, context, summary_title, &summary);
        self.append_failures(&mut document, context);
        self.append_rejections(&mut document, context);
        append_next(&mut document, self.next_command());
        document
    }

    fn summary_fields(&self) -> (&'static str, Vec<(String, String)>) {
        if self.totals.per_run_counts_available {
            let mut values = Vec::new();
            push_nonzero(&mut values, "Sources", self.totals.imported_sources);
            push_nonzero(&mut values, "Sessions", self.totals.imported_sessions);
            push_nonzero(&mut values, "Events", self.totals.imported_events);
            if self.totals.source_bytes > 0 {
                let processed = if self.totals.source_files > 0 {
                    format!(
                        "{} from {}",
                        format_bytes(self.totals.source_bytes),
                        counted(self.totals.source_files, "file", "files")
                    )
                } else {
                    format_bytes(self.totals.source_bytes)
                };
                values.push(("Processed".to_owned(), processed));
            }
            push_nonzero(
                &mut values,
                "Skipped sessions",
                self.totals.skipped_sessions,
            );
            push_nonzero(&mut values, "Skipped events", self.totals.skipped_events);
            if values.is_empty() && self.totals.source_files > 0 {
                values.push((
                    "Source files".to_owned(),
                    format_count(self.totals.source_files),
                ));
            }
            return ("Import", values);
        }

        let mut values = Vec::new();
        push_optional_nonzero(&mut values, "Sources", self.totals.current_source_count);
        if let Some(records) = self
            .totals
            .current_indexed_documents
            .filter(|records| *records > 0)
        {
            values.push(("Records".to_owned(), format_count_u64(records)));
        }
        if let Some(bytes) = self
            .totals
            .current_certified_source_bytes
            .filter(|bytes| *bytes > 0)
        {
            values.push(("Stored".to_owned(), format_bytes(bytes)));
        }
        push_optional_nonzero(
            &mut values,
            "Removed sources",
            self.totals.removed_source_count,
        );
        ("History", values)
    }

    fn append_failures(&self, document: &mut Document, context: &RenderContext) {
        if self.totals.failed_sources == 0 {
            return;
        }

        let mut values = vec![(
            "Sources".to_owned(),
            format_count(self.totals.failed_sources),
        )];
        for (index, failure) in self
            .source_failures
            .iter()
            .take(MAX_FAILURE_DETAILS)
            .enumerate()
        {
            values.push((format!("Failure {}", index + 1), failure.summary()));
        }
        if self.source_failures.len() > MAX_FAILURE_DETAILS {
            values.push((
                "More".to_owned(),
                counted(
                    self.source_failures.len() - MAX_FAILURE_DETAILS,
                    "source failure",
                    "source failures",
                ),
            ));
        }
        append_fields(document, context, "Failures", &values);
    }

    fn append_rejections(&self, document: &mut Document, context: &RenderContext) {
        if self.totals.sources_completed_with_rejections == 0 && self.totals.failed == 0 {
            return;
        }

        let mut values = Vec::new();
        push_nonzero(
            &mut values,
            "Sources",
            self.totals.sources_completed_with_rejections,
        );
        push_nonzero(&mut values, "Records", self.totals.failed);
        append_fields(document, context, "Rejections", &values);
    }

    fn next_command(&self) -> &'static str {
        if matches!(
            self.state,
            CompletionState::Partial | CompletionState::Failure | CompletionState::Empty
        ) {
            "ctx sources"
        } else if has_searchable_history(&self.totals) {
            "ctx search \"your query\""
        } else {
            "ctx status"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionState {
    Empty,
    Success,
    Partial,
    Failure,
}

#[derive(Debug, Clone)]
struct SourceFailure {
    provider: Option<String>,
    path: Option<String>,
    error: String,
}

impl SourceFailure {
    fn summary(&self) -> String {
        let mut summary = match self.provider.as_deref() {
            Some(provider) => format!("{provider}: {}", self.error),
            None => self.error.clone(),
        };
        if let Some(path) = self
            .path
            .as_deref()
            .filter(|path| !path.is_empty() && !self.error.contains(path))
        {
            summary.push_str("; source ");
            summary.push_str(path);
        }
        summary
    }
}

pub(crate) fn render_import_completion(report: &ImportReport, context: &RenderContext) -> Document {
    ImportCompletion::from_report(report).render(context)
}

fn source_failure(source: &Value) -> Option<SourceFailure> {
    let error = source.get("error").and_then(Value::as_str);
    let failed_status = source
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "error" | "failed"));
    if error.is_none() && !failed_status {
        return None;
    }

    let provider = source
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| source.get("history_source").and_then(Value::as_str))
        .map(str::to_owned);
    let path = source
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(SourceFailure {
        provider,
        path,
        error: error.unwrap_or("source could not be imported").to_owned(),
    })
}

fn has_visible_history(totals: &ImportTotals) -> bool {
    totals.imported_sources > 0
        || totals.imported_sessions > 0
        || totals.imported_events > 0
        || totals.source_bytes > 0
        || totals.current_source_count.is_some_and(|count| count > 0)
        || totals
            .current_indexed_documents
            .is_some_and(|count| count > 0)
        || totals
            .current_certified_source_bytes
            .is_some_and(|bytes| bytes > 0)
}

fn has_searchable_history(totals: &ImportTotals) -> bool {
    totals.imported_sessions > 0
        || totals.imported_events > 0
        || totals
            .current_indexed_documents
            .is_some_and(|count| count > 0)
}

fn append_fields(
    document: &mut Document,
    context: &RenderContext,
    title: &str,
    values: &[(String, String)],
) {
    if values.is_empty() {
        return;
    }
    let rendered = values
        .iter()
        .map(|(label, value)| Field::new(label, value))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section(title, fields(context, &rendered)));
}

fn append_next(document: &mut Document, command: &str) {
    document.push_blank();
    document.append(section(
        "Next",
        Document::from_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(command, Token::Command)),
        ),
    ));
}

fn push_nonzero(values: &mut Vec<(String, String)>, label: &str, value: usize) {
    if value > 0 {
        values.push((label.to_owned(), format_count(value)));
    }
}

fn push_optional_nonzero(values: &mut Vec<(String, String)>, label: &str, value: Option<usize>) {
    if let Some(value) = value.filter(|value| *value > 0) {
        values.push((label.to_owned(), format_count(value)));
    }
}

fn counted(value: usize, singular: &str, plural: &str) -> String {
    format!(
        "{} {}",
        format_count(value),
        if value == 1 { singular } else { plural }
    )
}

fn format_count_u64(value: u64) -> String {
    usize::try_from(value)
        .map(format_count)
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests;
