use ctx_pro_host_protocol::{BlameResult, ResolvedBlameTarget, WorktreeStatus};

use crate::ui::{Document, RenderContext, Token};

use super::layout::{line_range_text, push_authored, push_field, push_target_resource};

pub(super) fn render(document: &mut Document, context: &RenderContext, result: &BlameResult) {
    match &result.target {
        ResolvedBlameTarget::File {
            path,
            repository,
            requested_lines,
        } => {
            let label_width = "Repository".len();
            push_field(
                document,
                context,
                0,
                "Path",
                label_width,
                path,
                Token::Text,
                true,
            );
            let lines = requested_lines
                .as_ref()
                .map(line_range_text)
                .unwrap_or_else(|| "all committed lines".to_owned());
            push_field(
                document,
                context,
                0,
                "Lines",
                label_width,
                &lines,
                Token::Text,
                false,
            );
            push_target_resource(document, context, "Repository", label_width, repository);
            if let Some(snapshot) = &result.git_snapshot {
                push_field(
                    document,
                    context,
                    0,
                    "Snapshot",
                    label_width,
                    &format!("HEAD {}", snapshot.head_oid),
                    Token::Text,
                    true,
                );
                match snapshot.worktree_status {
                    WorktreeStatus::Clean => push_field(
                        document,
                        context,
                        0,
                        "Worktree",
                        label_width,
                        "clean",
                        Token::Success,
                        false,
                    ),
                    WorktreeStatus::Differs => {
                        push_field(
                            document,
                            context,
                            0,
                            "Worktree",
                            label_width,
                            "differs",
                            Token::Warning,
                            false,
                        );
                        push_authored(
                            document,
                            context,
                            2,
                            "Ranges refer to committed HEAD lines.",
                            Token::Text,
                        );
                    }
                }
            }
        }
        ResolvedBlameTarget::Commit { commit, repository } => {
            let label_width = "Repository".len();
            push_target_resource(document, context, "Commit", label_width, commit);
            push_target_resource(document, context, "Repository", label_width, repository);
        }
        ResolvedBlameTarget::PullRequest {
            selector,
            pull_request,
            repository,
        } => {
            let label_width = "Pull request".len();
            push_target_resource(document, context, "Pull request", label_width, pull_request);
            push_target_resource(document, context, "Repository", label_width, repository);
            push_field(
                document,
                context,
                0,
                "Selector",
                label_width,
                selector,
                Token::Text,
                true,
            );
        }
    }
}
