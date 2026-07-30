use super::{fields, hint, layout::wrap_text, outcome, Action, Field, Hint, Outcome, OutcomeState};
use crate::ui::{Document, Line, RenderContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Diagnostic<'a> {
    pub(crate) level: DiagnosticLevel,
    pub(crate) summary: &'a str,
    pub(crate) detail: Option<&'a str>,
    pub(crate) fields: &'a [Field<'a>],
    pub(crate) action: Option<Action<'a>>,
}

pub(crate) fn diagnostic(context: &RenderContext, diagnostic: Diagnostic<'_>) -> Document {
    let state = match diagnostic.level {
        DiagnosticLevel::Warning => OutcomeState::Warning,
        DiagnosticLevel::Error => OutcomeState::Error,
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title: diagnostic.summary,
            detail: None,
        },
    );
    if let Some(detail) = diagnostic.detail {
        for line in wrap_text(detail, context.content_width()) {
            document.push_line(Line::text(line));
        }
    }
    if !diagnostic.fields.is_empty() {
        document.push_blank();
        document.append(fields(context, diagnostic.fields));
    }
    if let Some(action) = diagnostic.action {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Resolve the issue and retry",
            },
            Some(action),
        ));
    }
    document
}
