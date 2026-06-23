//! Finished-product PR comment rendering and publishing primitives.
//!
//! This crate intentionally exposes GitHub-only publishing in this pass.
//! GitLab PR/MR parsing is recognized through `work-record-vcs`, but publishing
//! returns [`PublishError::GitlabUnsupported`] until a GitLab client is designed.

use work_record_core::{redact_share_safe_markers, Evidence, PullRequestProvider, WorkRecord};
use work_record_vcs::{parse_pull_request_url, VcsError};

pub const COMMENT_MARKER_START: &str = "<!-- ctx-work-record:finished-product:start -->";
pub const COMMENT_MARKER_END: &str = "<!-- ctx-work-record:finished-product:end -->";

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("invalid pull request URL: {0}")]
    InvalidPullRequestUrl(String),
    #[error("GitLab publishing is not supported yet")]
    GitlabUnsupported,
    #[error("only GitHub pull request publishing is supported, got {0}")]
    UnsupportedProvider(String),
    #[error("GitHub authentication is required")]
    AuthRequired,
    #[error("GitHub token does not have permission to publish PR comments")]
    PermissionDenied,
    #[error("pull request was not found")]
    PullRequestNotFound,
    #[error("GitHub API rate limit was exceeded")]
    RateLimited,
    #[error("GitHub client error: {0}")]
    Client(String),
}

pub type Result<T> = std::result::Result<T, PublishError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestTarget {
    pub provider: PullRequestProvider,
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub normalized_url: String,
}

impl PullRequestTarget {
    pub fn github_from_url(raw: &str) -> Result<Self> {
        let parsed = parse_pull_request_url(raw).map_err(|err| match err {
            VcsError::InvalidPullRequestUrl(value) => PublishError::InvalidPullRequestUrl(value),
            other => PublishError::Client(other.to_string()),
        })?;

        match parsed.provider {
            PullRequestProvider::Github => Ok(Self {
                provider: parsed.provider,
                host: parsed.host,
                owner: parsed.owner,
                repo: parsed.repo,
                number: parsed.number,
                normalized_url: parsed.normalized_url,
            }),
            PullRequestProvider::Gitlab => Err(PublishError::GitlabUnsupported),
            PullRequestProvider::Unknown => Err(PublishError::UnsupportedProvider(
                parsed.provider.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTranscriptOptIn {
    reason: String,
}

impl RawTranscriptOptIn {
    pub fn acknowledge_private_data_risk(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderOptions {
    pub raw_transcript: Option<RawTranscriptOptIn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPrComment {
    pub markdown: String,
    pub raw_transcript_included: bool,
}

pub fn render_pr_comment(
    records: &[WorkRecord],
    evidence: &[Evidence],
    options: &RenderOptions,
) -> RenderedPrComment {
    let raw_transcript_included = options.raw_transcript.is_some();
    let mut records = records.iter().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let mut evidence = evidence.iter().collect::<Vec<_>>();
    evidence.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then(left.id.cmp(&right.id))
    });

    let mut out = String::new();
    out.push_str(COMMENT_MARKER_START);
    out.push_str("\n## Work Recorder Finished Product\n\n");
    out.push_str(&format!("- Records: {}\n", records.len()));
    out.push_str(&format!("- Evidence items: {}\n", evidence.len()));
    out.push_str(&format!(
        "- Linked PRs: {}\n",
        records
            .iter()
            .filter(|record| record.pr_url.is_some())
            .count()
    ));
    out.push_str(&format!(
        "- Transcript mode: {}\n",
        if raw_transcript_included {
            "raw opt-in"
        } else {
            "redacted"
        }
    ));

    if records.is_empty() {
        out.push_str("\n_No work records selected._\n");
    } else {
        out.push_str("\n### Records\n");
        for record in records {
            out.push_str(&format!(
                "\n- **{}** `{}`\n",
                render_text(&record.title, raw_transcript_included),
                record.id
            ));
            if !record.tags.is_empty() {
                out.push_str(&format!(
                    "  - Tags: {}\n",
                    render_text(&record.tags.join(", "), raw_transcript_included)
                ));
            }
            if let Some(pr_url) = &record.pr_url {
                out.push_str(&format!("  - PR: {}\n", render_url(pr_url)));
            }
            if !record.body.trim().is_empty() {
                out.push_str("  - Notes:\n");
                push_indented_block(
                    &mut out,
                    &render_text(&record.body, raw_transcript_included),
                );
            }
        }
    }

    if !evidence.is_empty() {
        out.push_str("\n### Evidence\n");
        for item in evidence {
            out.push_str(&format!(
                "\n- `{}` exited {} in {}ms\n",
                render_text(&item.command, raw_transcript_included),
                item.exit_code,
                item.duration_ms
            ));
            if raw_transcript_included {
                if !item.stdout.is_empty() {
                    out.push_str("  - stdout:\n");
                    push_indented_block(&mut out, &item.stdout);
                }
                if !item.stderr.is_empty() {
                    out.push_str("  - stderr:\n");
                    push_indented_block(&mut out, &item.stderr);
                }
            } else if !item.stdout.is_empty() || !item.stderr.is_empty() {
                out.push_str("  - Transcript redacted by default.\n");
            }
        }
    }

    out.push('\n');
    out.push_str(COMMENT_MARKER_END);
    out.push('\n');

    RenderedPrComment {
        markdown: out,
        raw_transcript_included,
    }
}

pub fn replace_marked_comment_section(
    existing: &str,
    rendered_marked_section: &str,
) -> Option<String> {
    let (start, end) = marked_section_bounds(existing)?;

    let mut out = String::new();
    out.push_str(&existing[..start]);
    out.push_str(rendered_marked_section.trim_end());
    out.push_str(&existing[end..]);
    Some(out)
}

pub fn has_comment_markers(body: &str) -> bool {
    marked_section_bounds(body).is_some()
}

fn marked_section_bounds(body: &str) -> Option<(usize, usize)> {
    let start = body.find(COMMENT_MARKER_START)?;
    let after_start = start + COMMENT_MARKER_START.len();
    let relative_end = body[after_start..].find(COMMENT_MARKER_END)?;
    let end = after_start + relative_end + COMMENT_MARKER_END.len();
    Some((start, end))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestComment {
    pub id: u64,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertPlan {
    Create,
    Update { comment_id: u64 },
    Unchanged { comment_id: u64 },
}

pub fn plan_comment_upsert(
    existing_comments: &[PullRequestComment],
    desired_body: &str,
) -> UpsertPlan {
    let existing = existing_comments
        .iter()
        .filter(|comment| has_comment_markers(&comment.body))
        .min_by_key(|comment| comment.id);

    match existing {
        None => UpsertPlan::Create,
        Some(comment) if comment.body == desired_body => UpsertPlan::Unchanged {
            comment_id: comment.id,
        },
        Some(comment) => UpsertPlan::Update {
            comment_id: comment.id,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    DryRunCreated { markdown: String },
    DryRunUpdated { comment_id: u64, markdown: String },
    DryRunUnchanged { comment_id: u64, markdown: String },
    Created { comment_id: u64 },
    Updated { comment_id: u64 },
    Unchanged { comment_id: u64 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublishOptions {
    pub dry_run: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum GitHubClientError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("rate limited")]
    RateLimited,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("GitHub API error {status}: {message}")]
    Api { status: u16, message: String },
}

impl From<GitHubClientError> for PublishError {
    fn from(value: GitHubClientError) -> Self {
        match value {
            GitHubClientError::Unauthorized => PublishError::AuthRequired,
            GitHubClientError::Forbidden => PublishError::PermissionDenied,
            GitHubClientError::NotFound => PublishError::PullRequestNotFound,
            GitHubClientError::RateLimited => PublishError::RateLimited,
            GitHubClientError::Transport(message) => PublishError::Client(message),
            GitHubClientError::Api { status, message } => match status {
                401 => PublishError::AuthRequired,
                403 => PublishError::PermissionDenied,
                404 => PublishError::PullRequestNotFound,
                429 => PublishError::RateLimited,
                _ => PublishError::Client(format!("GitHub API error {status}: {message}")),
            },
        }
    }
}

pub trait GitHubPrCommentClient {
    fn list_comments(
        &mut self,
        target: &PullRequestTarget,
    ) -> std::result::Result<Vec<PullRequestComment>, GitHubClientError>;

    fn create_comment(
        &mut self,
        target: &PullRequestTarget,
        body: &str,
    ) -> std::result::Result<PullRequestComment, GitHubClientError>;

    fn update_comment(
        &mut self,
        target: &PullRequestTarget,
        comment_id: u64,
        body: &str,
    ) -> std::result::Result<PullRequestComment, GitHubClientError>;
}

pub fn upsert_github_pr_comment<C: GitHubPrCommentClient>(
    client: &mut C,
    target: &PullRequestTarget,
    body: &str,
    options: &PublishOptions,
) -> Result<PublishOutcome> {
    if target.provider != PullRequestProvider::Github {
        return Err(match target.provider {
            PullRequestProvider::Gitlab => PublishError::GitlabUnsupported,
            other => PublishError::UnsupportedProvider(other.to_string()),
        });
    }

    let comments = client.list_comments(target)?;
    let plan = plan_comment_upsert(&comments, body);
    if options.dry_run {
        return Ok(match plan {
            UpsertPlan::Create => PublishOutcome::DryRunCreated {
                markdown: body.to_owned(),
            },
            UpsertPlan::Update { comment_id } => PublishOutcome::DryRunUpdated {
                comment_id,
                markdown: body.to_owned(),
            },
            UpsertPlan::Unchanged { comment_id } => PublishOutcome::DryRunUnchanged {
                comment_id,
                markdown: body.to_owned(),
            },
        });
    }

    match plan {
        UpsertPlan::Create => {
            let comment = client.create_comment(target, body)?;
            Ok(PublishOutcome::Created {
                comment_id: comment.id,
            })
        }
        UpsertPlan::Update { comment_id } => {
            let comment = client.update_comment(target, comment_id, body)?;
            Ok(PublishOutcome::Updated {
                comment_id: comment.id,
            })
        }
        UpsertPlan::Unchanged { comment_id } => Ok(PublishOutcome::Unchanged { comment_id }),
    }
}

fn render_text(value: &str, raw_transcript_included: bool) -> String {
    if raw_transcript_included {
        value.to_owned()
    } else {
        redact_share_safe_markers(value)
    }
}

fn render_url(value: &str) -> String {
    PullRequestTarget::github_from_url(value)
        .map(|target| target.normalized_url)
        .unwrap_or_else(|_| "link withheld".to_owned())
}

fn push_indented_block(out: &mut String, value: &str) {
    for line in value.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn dry_run_markdown_is_marker_bounded_and_deterministic() {
        let mut later = record("Later", "second", vec!["beta"], 20);
        later.pr_url = Some("https://github.com/ctxrs/ctx/pull/5/files".into());
        let earlier = record("Earlier", "first", vec!["alpha"], 10);
        let evidence = evidence("cargo test", 0, "ok", "", 15);

        let first = render_pr_comment(
            &[later.clone(), earlier.clone()],
            &[evidence.clone()],
            &RenderOptions::default(),
        );
        let second = render_pr_comment(&[earlier, later], &[evidence], &RenderOptions::default());

        assert_eq!(first, second);
        assert!(first.markdown.starts_with(COMMENT_MARKER_START));
        assert!(first.markdown.trim_end().ends_with(COMMENT_MARKER_END));
        assert!(first.markdown.contains("## Work Recorder Finished Product"));
        assert!(first.markdown.contains("- Records: 2"));
        assert!(first
            .markdown
            .contains("https://github.com/ctxrs/ctx/pull/5"));
    }

    #[test]
    fn redacts_by_default_and_omits_transcripts() {
        let record = record(
            "Ship token=ghp_1234567890abcdef",
            "password=hunter2 from /home/daddy/code/private",
            vec!["secret=shhh"],
            10,
        );
        let evidence = evidence(
            "deploy --token=secret",
            1,
            "raw stdout password=hunter2",
            "raw stderr token=abc123",
            11,
        );

        let rendered = render_pr_comment(&[record], &[evidence], &RenderOptions::default());

        assert!(!rendered.raw_transcript_included);
        assert!(rendered.markdown.contains("token=[redacted]"));
        assert!(rendered.markdown.contains("password=[redacted]"));
        assert!(rendered.markdown.contains("[local-path]"));
        assert!(rendered.markdown.contains("Transcript redacted by default"));
        assert!(!rendered.markdown.contains("hunter2"));
        assert!(!rendered.markdown.contains("secret=shhh"));
        assert!(!rendered.markdown.contains("raw stdout"));
    }

    #[test]
    fn raw_transcript_requires_explicit_opt_in() {
        let options = RenderOptions {
            raw_transcript: Some(RawTranscriptOptIn::acknowledge_private_data_risk(
                "publishing to a private PR",
            )),
        };
        let record = record("Ship token=secret", "password=hunter2", vec![], 10);
        let evidence = evidence(
            "deploy --token=secret",
            0,
            "raw stdout password=hunter2",
            "",
            11,
        );

        let rendered = render_pr_comment(&[record], &[evidence], &options);

        assert!(rendered.raw_transcript_included);
        assert!(rendered.markdown.contains("Transcript mode: raw opt-in"));
        assert!(rendered.markdown.contains("password=hunter2"));
        assert!(rendered.markdown.contains("raw stdout password=hunter2"));
    }

    #[test]
    fn replaces_existing_marked_section_without_touching_surrounding_text() {
        let existing = "before\n<!-- ctx-work-record:finished-product:start -->\nold\n<!-- ctx-work-record:finished-product:end -->\nafter";
        let replacement = format!("{COMMENT_MARKER_START}\nnew\n{COMMENT_MARKER_END}\n");

        let replaced = replace_marked_comment_section(existing, &replacement).unwrap();

        assert_eq!(
            replaced,
            format!("before\n{COMMENT_MARKER_START}\nnew\n{COMMENT_MARKER_END}\nafter")
        );
    }

    #[test]
    fn marker_detection_requires_ordered_bounds() {
        let reversed = format!("{COMMENT_MARKER_END}\nold\n{COMMENT_MARKER_START}");

        assert!(!has_comment_markers(&reversed));
        assert_eq!(replace_marked_comment_section(&reversed, "new"), None);
    }

    #[test]
    fn parses_github_and_defers_gitlab() {
        let github =
            PullRequestTarget::github_from_url("github.com/ctxrs/ctx/pull/42/files").unwrap();
        assert_eq!(github.provider, PullRequestProvider::Github);
        assert_eq!(github.owner, "ctxrs");
        assert_eq!(github.repo, "ctx");
        assert_eq!(github.number, 42);
        assert_eq!(
            github.normalized_url,
            "https://github.com/ctxrs/ctx/pull/42"
        );

        let gitlab = PullRequestTarget::github_from_url(
            "https://gitlab.example.com/platform/team/ctx/-/merge_requests/7",
        );
        assert!(matches!(gitlab, Err(PublishError::GitlabUnsupported)));
    }

    #[test]
    fn plans_create_update_and_unchanged_idempotently() {
        let desired = format!("{COMMENT_MARKER_START}\ndesired\n{COMMENT_MARKER_END}\n");
        assert_eq!(plan_comment_upsert(&[], &desired), UpsertPlan::Create);
        assert_eq!(
            plan_comment_upsert(
                &[PullRequestComment {
                    id: 9,
                    body: desired.clone(),
                }],
                &desired
            ),
            UpsertPlan::Unchanged { comment_id: 9 }
        );
        assert_eq!(
            plan_comment_upsert(
                &[
                    PullRequestComment {
                        id: 10,
                        body: "unrelated".into(),
                    },
                    PullRequestComment {
                        id: 8,
                        body: format!("{COMMENT_MARKER_START}\nold\n{COMMENT_MARKER_END}\n"),
                    },
                ],
                &desired
            ),
            UpsertPlan::Update { comment_id: 8 }
        );
    }

    #[test]
    fn upsert_uses_mockable_client_for_create_update_and_dry_run() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let body = format!("{COMMENT_MARKER_START}\nnew\n{COMMENT_MARKER_END}\n");
        let mut client = MockClient {
            comments: Vec::new(),
            calls: Vec::new(),
            fail_list: None,
        };

        let dry_run = upsert_github_pr_comment(
            &mut client,
            &target,
            &body,
            &PublishOptions { dry_run: true },
        )
        .unwrap();
        assert!(matches!(dry_run, PublishOutcome::DryRunCreated { .. }));
        assert_eq!(client.calls, vec!["list"]);

        let created =
            upsert_github_pr_comment(&mut client, &target, &body, &PublishOptions::default())
                .unwrap();
        assert_eq!(created, PublishOutcome::Created { comment_id: 100 });
        assert_eq!(client.calls, vec!["list", "list", "create"]);

        client.comments = vec![PullRequestComment {
            id: 7,
            body: format!("{COMMENT_MARKER_START}\nold\n{COMMENT_MARKER_END}\n"),
        }];
        let updated =
            upsert_github_pr_comment(&mut client, &target, &body, &PublishOptions::default())
                .unwrap();
        assert_eq!(updated, PublishOutcome::Updated { comment_id: 7 });
        assert_eq!(client.comments[0].body, body);
    }

    #[test]
    fn maps_auth_and_permission_errors() {
        assert!(matches!(
            PublishError::from(GitHubClientError::Unauthorized),
            PublishError::AuthRequired
        ));
        assert!(matches!(
            PublishError::from(GitHubClientError::Forbidden),
            PublishError::PermissionDenied
        ));
        assert!(matches!(
            PublishError::from(GitHubClientError::Api {
                status: 401,
                message: "bad credentials".into()
            }),
            PublishError::AuthRequired
        ));
        assert!(matches!(
            PublishError::from(GitHubClientError::Api {
                status: 403,
                message: "resource not accessible by integration".into()
            }),
            PublishError::PermissionDenied
        ));
    }

    #[test]
    fn upsert_maps_client_auth_and_permission_errors() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let body = format!("{COMMENT_MARKER_START}\nnew\n{COMMENT_MARKER_END}\n");
        let mut client = MockClient {
            comments: Vec::new(),
            calls: Vec::new(),
            fail_list: Some(GitHubClientError::Unauthorized),
        };

        let error =
            upsert_github_pr_comment(&mut client, &target, &body, &PublishOptions::default())
                .unwrap_err();
        assert!(matches!(error, PublishError::AuthRequired));

        client.fail_list = Some(GitHubClientError::Forbidden);
        let error =
            upsert_github_pr_comment(&mut client, &target, &body, &PublishOptions::default())
                .unwrap_err();
        assert!(matches!(error, PublishError::PermissionDenied));
    }

    #[test]
    fn upsert_rejects_gitlab_targets_as_deferred() {
        let mut client = MockClient {
            comments: Vec::new(),
            calls: Vec::new(),
            fail_list: None,
        };
        let target = PullRequestTarget {
            provider: PullRequestProvider::Gitlab,
            host: "gitlab.example.com".into(),
            owner: "platform/team".into(),
            repo: "ctx".into(),
            number: 7,
            normalized_url: "https://gitlab.example.com/platform/team/ctx/-/merge_requests/7"
                .into(),
        };
        let body = format!("{COMMENT_MARKER_START}\nnew\n{COMMENT_MARKER_END}\n");

        let error =
            upsert_github_pr_comment(&mut client, &target, &body, &PublishOptions::default())
                .unwrap_err();

        assert!(matches!(error, PublishError::GitlabUnsupported));
        assert!(client.calls.is_empty());
    }

    fn record(title: &str, body: &str, tags: Vec<&str>, timestamp: i64) -> WorkRecord {
        let mut record = WorkRecord::new(
            title,
            body,
            tags.into_iter().map(str::to_owned).collect(),
            "task",
            None,
        );
        record.id = Uuid::from_u128(timestamp as u128);
        record.created_at = Utc.timestamp_opt(timestamp, 0).unwrap();
        record.updated_at = record.created_at;
        record
    }

    fn evidence(
        command: &str,
        exit_code: i32,
        stdout: &str,
        stderr: &str,
        timestamp: i64,
    ) -> Evidence {
        Evidence {
            id: Uuid::from_u128((timestamp + 1000) as u128),
            record_id: None,
            command: command.into(),
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            started_at: Utc.timestamp_opt(timestamp, 0).unwrap(),
            duration_ms: 123,
        }
    }

    struct MockClient {
        comments: Vec<PullRequestComment>,
        calls: Vec<&'static str>,
        fail_list: Option<GitHubClientError>,
    }

    impl GitHubPrCommentClient for MockClient {
        fn list_comments(
            &mut self,
            _target: &PullRequestTarget,
        ) -> std::result::Result<Vec<PullRequestComment>, GitHubClientError> {
            self.calls.push("list");
            if let Some(err) = self.fail_list.take() {
                return Err(err);
            }
            Ok(self.comments.clone())
        }

        fn create_comment(
            &mut self,
            _target: &PullRequestTarget,
            body: &str,
        ) -> std::result::Result<PullRequestComment, GitHubClientError> {
            self.calls.push("create");
            let comment = PullRequestComment {
                id: 100,
                body: body.into(),
            };
            self.comments.push(comment.clone());
            Ok(comment)
        }

        fn update_comment(
            &mut self,
            _target: &PullRequestTarget,
            comment_id: u64,
            body: &str,
        ) -> std::result::Result<PullRequestComment, GitHubClientError> {
            self.calls.push("update");
            let comment = PullRequestComment {
                id: comment_id,
                body: body.into(),
            };
            if let Some(existing) = self
                .comments
                .iter_mut()
                .find(|existing| existing.id == comment_id)
            {
                *existing = comment.clone();
            }
            Ok(comment)
        }
    }
}
