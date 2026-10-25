use clap::Args;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_history_archive::{
    ArchiveIdentity, CoreEventRangeFilters, ExportPreview, ScopeCounts, Selection,
};
use serde_json::json;

use crate::ui::{fields, Document, Field, RenderContext, Token};

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// New output directory; optional when previewing.
    #[arg(long, required_unless_present = "preview")]
    output: Option<PathBuf>,
    /// Stable origin for native history; selects the original origin in a restored root.
    #[arg(long)]
    origin: String,
    /// Stable view for native history; selects the original view in a restored root.
    #[arg(long, default_value = "personal")]
    view: String,
    /// Preview matching whole conversations and scope without creating an archive.
    #[arg(long)]
    preview: bool,
    /// Substring of stored workspace/CWD/project metadata, not a semantic topic.
    #[arg(long)]
    workspace: Option<String>,
    /// Inclusive RFC3339 timestamp; requires --until. Untimed events cannot match.
    #[arg(long)]
    since: Option<String>,
    /// Exclusive RFC3339 timestamp; requires --since. Whole conversations are retained.
    #[arg(long)]
    until: Option<String>,
    /// Include this full Core source digest (repeat to select more).
    #[arg(long, value_parser = digest)]
    source: Vec<String>,
    /// Include this full Core session digest (repeat to select more).
    #[arg(long, value_parser = digest)]
    session: Vec<String>,
}

pub fn run(
    args: &ExportArgs,
    root: &Path,
    context: &RenderContext,
) -> Result<(serde_json::Value, String)> {
    let identity = ArchiveIdentity {
        origin: args.origin.clone(),
        view: args.view.clone(),
    };
    let explicit = Selection {
        sources: args.source.iter().cloned().collect(),
        sessions: args.session.iter().cloned().collect(),
    };
    let filtered = args.workspace.is_some() || args.since.is_some() || args.until.is_some();
    let plan = if args.preview || filtered {
        let query = ctx_history_read_application::event_range_selection(
            args.since.as_deref(),
            args.until.as_deref(),
            CoreEventRangeFilters {
                workspace: args.workspace.clone(),
                ..CoreEventRangeFilters::default()
            },
        )
        .map_err(|error| ctx_history_archive::ArchiveError::Invalid(error.to_string()))?;
        Some(ctx_history_archive::plan_export(
            root,
            identity.clone(),
            &explicit,
            &query,
        )?)
    } else {
        None
    };
    if let Some(plan) = &plan {
        let selection = plan.preview();
        if args.preview || selection.selected_conversations == 0 {
            let status = if selection.selected_conversations == 0 {
                "empty"
            } else {
                "preview"
            };
            return Ok((
                json!({
                    "schema_version": 1, "operation": if args.preview { "archive_export_preview" } else { "archive_export" },
                    "status": status, "archive": null, "selection": selection_value(selection)?,
                }),
                preview_text(selection, context),
            ));
        }
    }
    let output = args
        .output
        .as_deref()
        .context("--output is required without --preview")?;
    let exported = match &plan {
        Some(plan) => plan.export(output)?,
        None => ctx_history_archive::export_data_root(root, output, identity, &explicit)?,
    };
    let mut text = format!(
        "Exported {} sessions and {} records to {}\nOrigin: {}. View: {}. This archive contains one identity.\nRetained normalized history only; native agent resumption is not included.",
        exported.manifest.members, exported.manifest.records, output.display(),
        exported.manifest.identity.origin, exported.manifest.identity.view,
    );
    for identity in &exported.excluded_identities {
        text.push_str(&format!(
            "\nNot included: origin {}, view {}. Export it separately with --origin and --view.",
            identity.origin, identity.view
        ));
    }
    let mut value = json!({
        "schema_version": 1, "operation": "archive_export", "status": "exported", "archive": output,
        "manifest": exported.manifest, "excluded_identities": exported.excluded_identities,
    });
    if let Some(plan) = &plan {
        value["selection"] = selection_value(plan.preview())?;
        text.push('\n');
        text.push_str(&preview_text(plan.preview(), context));
    }
    Ok((value, text))
}

fn selection_value(preview: &ExportPreview) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(preview)?;
    value["label_authority"] =
        json!(ctx_history_read_application::ArchiveLabelAuthority::ArchiveCreator);
    Ok(value)
}

fn preview_text(preview: &ExportPreview, context: &RenderContext) -> String {
    let mut document = Document::new();
    let mut field = |label: &str, value: &str| {
        document.append(fields(context, &[Field::new(label, value)]));
    };
    field(
        "Selection",
        if preview.selected_conversations == 0 {
            "Complete: no matching conversations; no archive created."
        } else {
            "Complete: whole conversations are selected."
        },
    );
    field("Archive origin", &preview.identity.origin);
    field("Archive view", &preview.identity.view);
    field(
        "Labels",
        "Creator-supplied; not verified machine identities.",
    );
    if let Some(workspace) = &preview.workspace {
        field("Workspace", workspace);
    }
    if let (Some(since), Some(until)) = (preview.since_unix_ms, preview.until_unix_ms) {
        field("Since (inclusive)", &timestamp(Some(since)));
        field("Until (exclusive)", &timestamp(Some(until)));
    }
    field("Conversations", &preview.selected_conversations.to_string());
    field(
        "Scope",
        "Counts describe local conversations before identical original members coalesce on export.",
    );
    field(
        "Workspace meaning",
        "Stored workspace/CWD/project metadata, not semantic topic detection.",
    );
    field(
        "Time meaning",
        "Untimed events cannot match a time range; whole selected conversations retain them.",
    );
    for excluded in &preview.excluded_identities {
        field(
            "Excluded origin/view",
            &format!("{} / {}", excluded.origin, excluded.view),
        );
    }
    document.push_blank();
    append_counts(&mut document, context, &preview.scope);
    for conversation in &preview.conversations {
        document.push_blank();
        document.append(fields(
            context,
            &[
                Field::new("Conversation", &conversation.ctx_session_id)
                    .with_value_token(Token::Reference),
                Field::new("Provider", &conversation.provider),
                Field::new(
                    "Provider session",
                    conversation
                        .provider_session_id
                        .as_deref()
                        .unwrap_or("unknown"),
                ),
                Field::new(
                    "First timestamp",
                    &timestamp(conversation.first_occurred_at_unix_ms),
                ),
                Field::new(
                    "Last timestamp",
                    &timestamp(conversation.last_occurred_at_unix_ms),
                ),
            ],
        ));
        append_counts(&mut document, context, &conversation.scope);
        for evidence in &conversation.workspace_values {
            document.append(fields(
                context,
                &[Field::new(
                    if evidence.matches_workspace {
                        "Workspace metadata"
                    } else {
                        "Other workspace"
                    },
                    &evidence.value,
                )],
            ));
        }
        if conversation.workspace_values_truncated {
            document.append(fields(
                context,
                &[Field::new(
                    "Workspace values",
                    "First 8 distinct values shown; record counts are complete.",
                )],
            ));
        }
        document.append(fields(
            context,
            &[Field::new("Inspect", &conversation.show_session_command)
                .with_value_token(Token::Command)],
        ));
    }
    if preview.conversations_truncated {
        document.append(fields(context, &[Field::new("Display", "First 100 conversations shown. Selection is complete; export includes every selected conversation.")]));
    }
    document.render_plain()
}

fn append_counts(document: &mut Document, context: &RenderContext, scope: &ScopeCounts) {
    document.append(fields(
        context,
        &[
            Field::new(
                "Records",
                &format!(
                    "{} match; {} retained in whole conversations",
                    scope.matched_records, scope.retained_records
                ),
            ),
            Field::new(
                "Time scope",
                &format!(
                    "{} before; {} at/after end; {} untimed",
                    scope.records_before_window, scope.records_after_window, scope.untimed_records
                ),
            ),
            Field::new(
                "Workspace scope",
                &format!(
                    "{} without metadata; {} without a matching value; {} with other values",
                    scope.records_without_workspace,
                    scope.records_outside_workspace,
                    scope.records_with_other_workspaces
                ),
            ),
        ],
    ));
}

fn timestamp(value: Option<i64>) -> String {
    ctx_history_read_application::timestamp_json(value).unwrap_or_else(|| "unknown".into())
}

pub fn digest(value: &str) -> std::result::Result<String, String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_owned())
    } else {
        Err("expected a full 64-character lowercase Core identity digest".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{display_width, ColorMode, StreamKind, TestContext};
    use ctx_history_archive::{ConversationPreview, WorkspaceValue};

    #[test]
    fn preview_preserves_scope_and_escapes_metadata_at_terminal_widths() {
        let preview = ExportPreview {
            generation_id: "synthetic-generation".into(),
            identity: ArchiveIdentity {
                origin: "café laptop".into(),
                view: "work".into(),
            },
            excluded_identities: Vec::new(),
            workspace: Some("shop".into()),
            since_unix_ms: Some(100),
            until_unix_ms: Some(200),
            selection_complete: true,
            selected_conversations: 1,
            scope: ScopeCounts {
                matched_records: 1,
                retained_records: 3,
                untimed_records: 1,
                records_before_window: 1,
                ..ScopeCounts::default()
            },
            conversations: vec![ConversationPreview {
                ctx_session_id: "synthetic-session".into(),
                provider: "synthetic".into(),
                provider_session_id: Some("provider\u{001b}[31m\nvalue".into()),
                first_occurred_at_unix_ms: Some(99),
                last_occurred_at_unix_ms: Some(100),
                scope: ScopeCounts::default(),
                workspace_values: vec![WorkspaceValue {
                    value: "/work/café\tshop".into(),
                    matches_workspace: true,
                }],
                workspace_values_truncated: true,
                show_session_command: "ctx show session synthetic-session --mode log".into(),
            }],
            conversations_truncated: true,
        };
        for width in [32, 48, 80, 120] {
            let context = RenderContext::for_test(
                TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never),
            );
            let text = preview_text(&preview, &context);
            assert!(text.contains("café laptop"));
            assert!(!text.contains('\u{001b}') && !text.contains('\t'));
            let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("1 before; 0 at/after end; 1 untimed"));
            assert!(words
                .contains("Selection is complete; export includes every selected conversation."));
            for line in text.lines() {
                assert!(
                    display_width(line) <= width
                        || line.trim() == preview.conversations[0].show_session_command,
                    "width {width}: {line}"
                );
            }
        }
    }
}
