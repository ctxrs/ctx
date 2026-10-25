use serde_json::Value;

use crate::ui::{Document, RenderContext, Token};

pub(super) fn fields(
    document: &mut Document,
    context: &RenderContext,
    value: &Value,
    indent: usize,
) {
    let provenance = &value["archive_provenance"];
    if !provenance.is_object() {
        return;
    }
    for (label, key) in [("Archive origin", "origin"), ("Archive view", "view")] {
        if let Some(value) = provenance[key].as_str() {
            let safe = ctx_terminal::sanitize_untrusted_history_body_for_terminal(value);
            super::human::push_field(document, context, indent, label, 14, &safe, Token::Text);
        }
    }
    super::human::push_wrapped(
        document,
        context,
        indent,
        "Creator-supplied labels; not verified machine identities.",
        Token::Label,
    );
}

pub(in crate::source_index) fn append(output: &mut String, value: &Value, markdown: bool) {
    let provenance = &value["archive_provenance"];
    if !provenance.is_object() {
        return;
    }
    for (label, key) in [("Archive origin", "origin"), ("Archive view", "view")] {
        if let Some(value) = provenance[key].as_str() {
            let safe = ctx_terminal::sanitize_untrusted_history_body_for_terminal(value);
            let safe = if markdown {
                super::markdown_code_span(&safe)
            } else {
                safe
            };
            output.push_str(&format!("{label} (creator-supplied): {safe}\n"));
        }
    }
    output.push_str("Not verified machine identities.\n");
}
