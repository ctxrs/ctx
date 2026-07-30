use serde_json::Value;

use crate::{
    transcript::shell_quote_arg,
    ui::{Document, RenderContext, Token},
};

use super::human::{push_action, push_field, push_heading};

const IDENTITY_LABEL_WIDTH: usize = 16;
const SOURCE_LABEL_WIDTH: usize = 7;

pub(in crate::commands::source_index) fn render_locate_document(
    value: &Value,
    context: &RenderContext,
) -> Document {
    let session = value["target"].as_str() == Some("session");
    let mut document = Document::new();
    push_heading(
        &mut document,
        if session { "Session" } else { "Event" },
        Token::Heading,
    );
    if session {
        render_session_identity(&mut document, context, value);
    } else {
        render_event_identity(&mut document, context, value);
    }
    render_source(&mut document, context, value);
    render_actions(&mut document, context, value, session);
    document
}

fn render_session_identity(document: &mut Document, context: &RenderContext, value: &Value) {
    push_optional_field(
        document,
        context,
        "ID",
        value["ctx_session_id"].as_str(),
        Token::Reference,
    );
    push_optional_field(
        document,
        context,
        "Provider",
        value["provider"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        document,
        context,
        "Provider session",
        value["provider_session_id"].as_str(),
        Token::Reference,
    );
}

fn render_event_identity(document: &mut Document, context: &RenderContext, value: &Value) {
    push_optional_field(
        document,
        context,
        "ID",
        value["ctx_event_id"].as_str(),
        Token::Reference,
    );
    push_optional_field(
        document,
        context,
        "Session",
        value["ctx_session_id"].as_str(),
        Token::Reference,
    );
    push_optional_field(
        document,
        context,
        "Provider",
        value["provider"].as_str(),
        Token::Accent,
    );
    push_optional_field(
        document,
        context,
        "Provider session",
        value["provider_session_id"].as_str(),
        Token::Reference,
    );
    let event_type = value["event_type"].as_str();
    let role = value["role"].as_str();
    let kind = match (role, event_type) {
        (Some(role), Some(event_type)) if role != event_type => {
            Some(format!("{role} {event_type}"))
        }
        (Some(role), _) => Some(role.to_owned()),
        (None, Some(event_type)) => Some(event_type.to_owned()),
        (None, None) => None,
    };
    if let Some(kind) = kind {
        push_field(
            document,
            context,
            0,
            "Type",
            IDENTITY_LABEL_WIDTH,
            &kind,
            Token::Text,
        );
    }
    push_optional_field(
        document,
        context,
        "Cursor",
        value["cursor"].as_str(),
        Token::Reference,
    );
}

fn push_optional_field(
    document: &mut Document,
    context: &RenderContext,
    label: &str,
    value: Option<&str>,
    token: Token,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        push_field(
            document,
            context,
            0,
            label,
            IDENTITY_LABEL_WIDTH,
            value,
            token,
        );
    }
}

fn render_source(document: &mut Document, context: &RenderContext, value: &Value) {
    let source = &value["source"];
    let record = &value["source_record"];
    let has_source = source["path"].is_string()
        || source["source_format"].is_string()
        || record.is_object()
        || source["exists"].as_bool() == Some(false);
    if !has_source {
        return;
    }

    document.push_blank();
    push_heading(document, "Source", Token::Heading);
    if let Some(path) = source["path"].as_str() {
        push_field(
            document,
            context,
            0,
            "Path",
            SOURCE_LABEL_WIDTH,
            path,
            Token::Accent,
        );
    }
    if let Some(format) = source["source_format"].as_str() {
        push_field(
            document,
            context,
            0,
            "Format",
            SOURCE_LABEL_WIDTH,
            format,
            Token::Text,
        );
    }
    if let Some(coordinate) = record_coordinate(record) {
        push_field(
            document,
            context,
            0,
            "Record",
            SOURCE_LABEL_WIDTH,
            &coordinate,
            Token::Reference,
        );
    }
    if let (Some(offset), Some(length)) = (
        record["byte_offset"].as_u64(),
        record["byte_length"].as_u64(),
    ) {
        push_field(
            document,
            context,
            0,
            "Bytes",
            SOURCE_LABEL_WIDTH,
            &format!("{offset}-{}", offset.saturating_add(length)),
            Token::Reference,
        );
    }
    for (label, key) in [
        ("Pointer", "json_pointer"),
        ("Relation", "logical_relation"),
        ("Namespace", "namespace"),
    ] {
        if let Some(coordinate) = record[key].as_str() {
            push_field(
                document,
                context,
                0,
                label,
                SOURCE_LABEL_WIDTH,
                coordinate,
                Token::Reference,
            );
        }
    }

    let source_missing = source["exists"].as_bool() == Some(false);
    if source_missing {
        push_field(
            document,
            context,
            0,
            "Status",
            SOURCE_LABEL_WIDTH,
            "missing",
            Token::Warning,
        );
    }
    let content = &value["complete_content"];
    if content["locator_available"].as_bool() == Some(false) {
        push_field(
            document,
            context,
            0,
            "Locator",
            SOURCE_LABEL_WIDTH,
            "unavailable",
            Token::Warning,
        );
    } else if !source_missing && content["available"].as_bool() == Some(false) {
        push_field(
            document,
            context,
            0,
            "Content",
            SOURCE_LABEL_WIDTH,
            "unavailable",
            Token::Warning,
        );
    }
}

fn record_coordinate(record: &Value) -> Option<String> {
    let ordinal = record["ordinal"].as_u64()?;
    Some(match record["subrecord_index"].as_u64() {
        Some(index) => format!("{ordinal}.{index}"),
        None => ordinal.to_string(),
    })
}

fn render_actions(document: &mut Document, context: &RenderContext, value: &Value, session: bool) {
    let id = if session {
        value["ctx_session_id"].as_str()
    } else {
        value["ctx_event_id"].as_str()
    };
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        let command = if session {
            format!("ctx show session {}", shell_quote_arg(id))
        } else {
            format!("ctx show event {} --window 10", shell_quote_arg(id))
        };
        document.push_blank();
        push_action(document, context, 0, "Inspect", &command);
    }
    if let Some(command) = value["resume"]["command"]
        .as_str()
        .filter(|command| !command.is_empty())
    {
        document.push_blank();
        push_action(document, context, 0, "Resume", command);
    }
}
