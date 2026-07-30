use std::time::Duration;

use crate::progress::{format_byte_progress, format_bytes, format_count};
use crate::ui::{
    fields, outcome, progress, Document, Field, Outcome, OutcomeState, Progress, RenderContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressSnapshot<'a> {
    operation: &'a str,
    phase: &'a str,
    message: &'a str,
    completed_bytes: u64,
    total_bytes: Option<u64>,
    completed_files: Option<usize>,
    total_files: Option<usize>,
    imported_events: Option<usize>,
    elapsed: Duration,
    done: bool,
}

impl<'a> ProgressSnapshot<'a> {
    pub(crate) const fn new(
        operation: &'a str,
        phase: &'a str,
        message: &'a str,
        elapsed: Duration,
    ) -> Self {
        Self {
            operation,
            phase,
            message,
            completed_bytes: 0,
            total_bytes: None,
            completed_files: None,
            total_files: None,
            imported_events: None,
            elapsed,
            done: false,
        }
    }

    pub(crate) const fn with_bytes(mut self, completed: u64, total: Option<u64>) -> Self {
        self.completed_bytes = completed;
        self.total_bytes = total;
        self
    }

    pub(crate) const fn with_files(mut self, completed: usize, total: Option<usize>) -> Self {
        self.completed_files = Some(completed);
        self.total_files = total;
        self
    }

    pub(crate) const fn with_imported_events(mut self, imported: usize) -> Self {
        self.imported_events = Some(imported);
        self
    }

    pub(crate) const fn finished(mut self) -> Self {
        self.done = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressFrameKind {
    Snapshot,
    Transient,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgressFrame {
    document: Document,
    kind: ProgressFrameKind,
}

impl ProgressFrame {
    pub(crate) const fn document(&self) -> &Document {
        &self.document
    }

    pub(crate) const fn kind(&self) -> ProgressFrameKind {
        self.kind
    }

    pub(crate) fn into_document(self) -> Document {
        self.document
    }
}

pub(crate) fn render_progress_snapshot(
    snapshot: &ProgressSnapshot<'_>,
    context: &RenderContext,
) -> ProgressFrame {
    let title = progress_title(snapshot.operation, snapshot.done);
    let detail = (!snapshot.message.trim().is_empty()).then_some(snapshot.message);
    let total_bytes = snapshot
        .total_bytes
        .filter(|total| *total > 0)
        .map(|total| total.max(snapshot.completed_bytes));
    let mut document = if snapshot.done {
        outcome(
            context,
            Outcome {
                state: OutcomeState::Success,
                title: &title,
                detail,
            },
        )
    } else {
        progress(
            context,
            Progress {
                label: &title,
                current: snapshot.completed_bytes,
                total: total_bytes,
                detail,
            },
        )
    };

    let values = snapshot_fields(snapshot, context, total_bytes);
    if !values.is_empty() {
        let rendered = values
            .iter()
            .map(|(label, value)| Field::new(label, value))
            .collect::<Vec<_>>();
        document.push_blank();
        document.append(fields(context, &rendered));
    }

    ProgressFrame {
        document,
        kind: if context.is_terminal() && !snapshot.done {
            ProgressFrameKind::Transient
        } else {
            ProgressFrameKind::Snapshot
        },
    }
}

fn snapshot_fields(
    snapshot: &ProgressSnapshot<'_>,
    context: &RenderContext,
    total_bytes: Option<u64>,
) -> Vec<(String, String)> {
    let mut values = Vec::new();
    if !snapshot.phase.trim().is_empty() {
        values.push(("Phase".to_owned(), humanize(snapshot.phase)));
    }

    if let Some(total) = total_bytes {
        values.push((
            "Processed".to_owned(),
            format_byte_progress(snapshot.completed_bytes, total),
        ));
    } else if snapshot.completed_bytes > 0 {
        values.push((
            "Processed".to_owned(),
            format!(
                "{}; total {}",
                format_bytes(snapshot.completed_bytes),
                indeterminate(context, "measuring")
            ),
        ));
    } else if !snapshot.done {
        values.push(("Processed".to_owned(), indeterminate(context, "measuring")));
    }

    if let Some(completed) = snapshot.completed_files {
        let value = match snapshot.total_files.filter(|total| *total > 0) {
            Some(total) => {
                let total = total.max(completed);
                format!("{} / {}", format_count(completed), format_count(total))
            }
            None => format_count(completed),
        };
        values.push(("Files".to_owned(), value));
    } else if let Some(total) = snapshot.total_files.filter(|total| *total > 0) {
        values.push(("Files".to_owned(), format!("0 / {}", format_count(total))));
    }

    if let Some(events) = snapshot.imported_events.filter(|events| *events > 0) {
        values.push(("Events".to_owned(), format_count(events)));
    }
    values.push(("Elapsed".to_owned(), format_duration(snapshot.elapsed)));
    values
}

fn progress_title(operation: &str, done: bool) -> String {
    match (operation, done) {
        ("import", false) => "Importing history".to_owned(),
        ("import", true) => "History import complete".to_owned(),
        (operation, false) => format!("{} in progress", humanize(operation)),
        (operation, true) => format!("{} complete", humanize(operation)),
    }
}

fn humanize(value: &str) -> String {
    let mut text = value.replace(['_', '-'], " ");
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    text
}

fn indeterminate(context: &RenderContext, text: &str) -> String {
    if context.unicode() {
        format!("{text}…")
    } else {
        format!("{text}...")
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds == 0 {
        return "under 1 second".to_owned();
    }
    if seconds < 60 {
        return format!(
            "{seconds} {}",
            if seconds == 1 { "second" } else { "seconds" }
        );
    }

    let minutes = seconds / 60;
    let remaining_seconds = seconds % 60;
    if seconds < 3_600 {
        if remaining_seconds == 0 {
            return format!(
                "{minutes} {}",
                if minutes == 1 { "minute" } else { "minutes" }
            );
        }
        return format!(
            "{minutes} {}, {remaining_seconds} {}",
            if minutes == 1 { "minute" } else { "minutes" },
            if remaining_seconds == 1 {
                "second"
            } else {
                "seconds"
            }
        );
    }

    let hours = seconds / 3_600;
    let remaining_minutes = (seconds % 3_600) / 60;
    if remaining_minutes == 0 {
        format!("{hours} {}", if hours == 1 { "hour" } else { "hours" })
    } else {
        format!(
            "{hours} {}, {remaining_minutes} {}",
            if hours == 1 { "hour" } else { "hours" },
            if remaining_minutes == 1 {
                "minute"
            } else {
                "minutes"
            }
        )
    }
}

#[cfg(test)]
mod tests;
