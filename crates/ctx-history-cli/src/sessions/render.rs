use serde_json::Value;

use crate::{
    source_index::{render_archive, render_human},
    ui::{
        diagnostic, Action, Diagnostic, DiagnosticLevel, Document, Line, RenderContext, Span, Token,
    },
};

const CARD_INDENT: usize = 3;
const CARD_LABEL_WIDTH: usize = 8;

pub(super) fn render_sessions_not_ready_document(context: &RenderContext) -> Document {
    let mut document = diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: "History sessions are not ready",
            detail: Some(
                "There is no current searchable generation. Set up ctx to discover agent history, or import history if setup is already complete.",
            ),
            fields: &[],
            action: Some(Action {
                command: "ctx setup",
            }),
        },
    );
    document.push_blank();
    render_human::push_action(
        &mut document,
        context,
        0,
        "Already set up?",
        "ctx import --all",
    );
    document
}

pub(super) fn render_sessions_document(value: &Value, context: &RenderContext) -> Document {
    let sessions = value["sessions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if sessions.is_empty() {
        let mut document = Document::new();
        render_human::push_wrapped(
            &mut document,
            context,
            0,
            "No sessions found.",
            Token::Warning,
        );
        return document;
    }

    let mut document = Document::new();
    let heading = format!(
        "{} {}",
        sessions.len(),
        if sessions.len() == 1 {
            "session"
        } else {
            "sessions"
        }
    );
    render_human::push_heading(&mut document, &heading, Token::Heading);

    for (index, session) in sessions.iter().enumerate() {
        document.push_blank();
        render_session_card(&mut document, context, index + 1, session);
    }

    if value["truncated"] == true {
        document.push_blank();
        render_human::push_heading(&mut document, "More sessions available.", Token::Warning);
    }
    document
}

fn render_session_card(
    document: &mut Document,
    context: &RenderContext,
    position: usize,
    session: &Value,
) {
    let session_id = session["ctx_session_id"].as_str().unwrap_or("unknown");
    let provider_label = session_provider_label(session);
    let (activity, activity_token) =
        format_session_timestamp(context, session["last_activity_at"].as_str());
    let events = session["events"].as_u64().unwrap_or(0);
    let events_label = if events == 1 {
        "1 event".to_owned()
    } else {
        format!("{events} events")
    };

    document.push_line(
        Line::new()
            .with(Span::new(format!("{position}. "), Token::Accent))
            .with(Span::new(session_id, Token::Reference))
            .with(Span::text(" · "))
            .with(Span::new(provider_label, Token::Accent))
            .with(Span::text(" · "))
            .with(Span::new(activity, activity_token))
            .with(Span::text(" · "))
            .with(Span::new(events_label, Token::Label)),
    );

    render_archive::fields(document, context, session, CARD_INDENT);

    if let Some(source) = session_source_identity(session) {
        let safe = ctx_terminal::sanitize_untrusted_history_body_for_terminal(&source);
        render_human::push_field(
            document,
            context,
            CARD_INDENT,
            "Source",
            CARD_LABEL_WIDTH,
            &safe,
            Token::Text,
        );
    }
    for (label, key) in [("CWD", "cwd"), ("Project", "project"), ("Branch", "branch")] {
        if let Some(raw) = session[key].as_str().filter(|value| !value.is_empty()) {
            let safe = ctx_terminal::sanitize_untrusted_history_body_for_terminal(raw);
            render_human::push_field(
                document,
                context,
                CARD_INDENT,
                label,
                CARD_LABEL_WIDTH,
                &safe,
                Token::Text,
            );
        }
    }

    let parent = session["parent_ctx_session_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    if let Some(parent_id) = parent {
        render_human::push_field(
            document,
            context,
            CARD_INDENT,
            "Parent",
            CARD_LABEL_WIDTH,
            parent_id,
            Token::Reference,
        );
    }
    if let Some(root_id) = session["root_ctx_session_id"]
        .as_str()
        .filter(|value| !value.is_empty() && Some(*value) != parent && *value != session_id)
    {
        render_human::push_field(
            document,
            context,
            CARD_INDENT,
            "Root",
            CARD_LABEL_WIDTH,
            root_id,
            Token::Reference,
        );
    }

    let started = session["started_at"]
        .as_str()
        .filter(|value| !value.is_empty());
    let last_activity = session["last_activity_at"]
        .as_str()
        .filter(|value| !value.is_empty());
    if let Some(started_raw) = started.filter(|value| Some(*value) != last_activity) {
        let formatted = context.human_timestamp(started_raw);
        render_human::push_field(
            document,
            context,
            CARD_INDENT,
            "Started",
            CARD_LABEL_WIDTH,
            &formatted,
            Token::Text,
        );
    }
}

fn format_session_timestamp(context: &RenderContext, value: Option<&str>) -> (String, Token) {
    match value.filter(|value| !value.is_empty()) {
        Some(raw) => (context.human_timestamp(raw).into_owned(), Token::Text),
        None => ("time unavailable".to_owned(), Token::Label),
    }
}

fn session_provider_label(session: &Value) -> String {
    crate::provider_display_name(session["provider"].as_str().unwrap_or("unknown"))
}

fn session_source_identity(session: &Value) -> Option<String> {
    let provider = session["provider"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let provider_key = session["provider_key"]
        .as_str()
        .filter(|value| !value.is_empty());
    let source_id = session["source_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    let identity = match (provider_key, source_id) {
        (Some(provider_key), Some(source_id)) if provider_key == source_id => {
            provider_key.to_owned()
        }
        (Some(provider_key), Some(source_id)) => format!("{provider_key}/{source_id}"),
        (Some(provider_key), None) => provider_key.to_owned(),
        (None, Some(source_id)) => source_id.to_owned(),
        (None, None) => return None,
    };
    let display_provider = crate::provider_display_name(provider);
    if identity == provider || identity == display_provider {
        None
    } else {
        Some(identity)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{render_sessions_document, render_sessions_not_ready_document};
    use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext};

    fn test_context() -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 100).color(ColorMode::Never))
    }

    #[test]
    fn renders_empty_state_cleanly() {
        let value = json!({
            "schema_version": 1,
            "payload_type": "session_list",
            "generation_id": "gen-1",
            "truncated": false,
            "sessions": [],
        });
        let rendered = render_sessions_document(&value, &test_context()).render_plain();
        assert!(rendered.contains("No sessions found."), "{rendered}");
        assert!(!rendered.contains("More sessions available."), "{rendered}");
    }

    #[test]
    fn renders_session_cards_truncation_and_sanitizes_control_chars() {
        let value = json!({
            "schema_version": 1,
            "payload_type": "session_list",
            "generation_id": "gen-2",
            "truncated": true,
            "sessions": [
                {
                    "ctx_session_id": "a1b2c3d4",
                    "provider": "codex",
                    "cwd": "/work/\x1b[31mdanger\x1b[0m",
                    "project": "ctx",
                    "branch": "main",
                    "started_at": "2026-11-20T10:00:00.000Z",
                    "last_activity_at": "2026-11-20T12:00:00.000Z",
                    "events": 12
                },
                {
                    "ctx_session_id": "e5f60718",
                    "provider": "custom",
                    "provider_key": "amp",
                    "source_id": "threads",
                    "parent_ctx_session_id": "a1b2c3d4",
                    "events": 1
                }
            ]
        });
        let rendered = render_sessions_document(&value, &test_context()).render_plain();
        assert!(rendered.contains("2 sessions"), "{rendered}");
        assert!(rendered.contains("1. a1b2c3d4 · Codex"), "{rendered}");
        assert!(rendered.contains("12 events"), "{rendered}");
        assert!(rendered.contains("CWD"), "{rendered}");
        assert!(rendered.contains("Project"), "{rendered}");
        assert!(rendered.contains("Branch"), "{rendered}");
        assert!(rendered.contains("Started"), "{rendered}");
        assert!(rendered.contains("2. e5f60718 · Custom"), "{rendered}");
        assert!(rendered.contains("time unavailable"), "{rendered}");
        assert!(rendered.contains("1 event"), "{rendered}");
        assert!(rendered.contains("Source"), "{rendered}");
        assert!(rendered.contains("amp/threads"), "{rendered}");
        assert!(rendered.contains("Parent"), "{rendered}");
        assert!(rendered.contains("More sessions available."), "{rendered}");
        assert!(
            !rendered.contains('\x1b'),
            "raw escape leaked: {rendered:?}"
        );
    }

    #[test]
    fn not_ready_document_points_to_setup_and_import() {
        let rendered = render_sessions_not_ready_document(&test_context()).render_plain();
        assert!(
            rendered.contains("History sessions are not ready"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx setup"), "{rendered}");
        assert!(rendered.contains("ctx import --all"), "{rendered}");
    }
}
