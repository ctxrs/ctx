//! Finished-product PR comment rendering and publishing primitives.
//!
//! This crate intentionally exposes GitHub-only publishing in this pass.
//! GitLab PR/MR parsing is recognized through `work-record-vcs`, but publishing
//! returns [`PublishError::GitlabUnsupported`] until a GitLab client is designed.

use std::{
    ffi::OsString,
    process::{Command, Stdio},
};

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
    #[error("raw transcript publishing requires a non-empty acknowledgement reason")]
    InvalidRawTranscriptOptIn,
    #[error("PR comment body must contain exactly one ctx marker-bounded section")]
    InvalidMarkedComment,
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
    pub fn acknowledge_private_data_risk(reason: impl Into<String>) -> Result<Self> {
        let reason = reason.into().trim().to_owned();
        if reason.is_empty() {
            return Err(PublishError::InvalidRawTranscriptOptIn);
        }
        Ok(Self { reason })
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
                render_share_text(&record.title),
                record.id
            ));
            if !record.tags.is_empty() {
                out.push_str(&format!(
                    "  - Tags: {}\n",
                    render_share_text(&record.tags.join(", "))
                ));
            }
            if let Some(pr_url) = &record.pr_url {
                out.push_str(&format!("  - PR: {}\n", render_url(pr_url)));
            }
            if !record.body.trim().is_empty() {
                out.push_str("  - Notes:\n");
                push_indented_block(&mut out, &render_share_text(&record.body));
            }
        }
    }

    if !evidence.is_empty() {
        out.push_str("\n### Evidence\n");
        for item in evidence {
            out.push_str(&format!(
                "\n- `{}` exited {} in {}ms\n",
                render_share_text(&item.command),
                item.exit_code,
                item.duration_ms
            ));
            if raw_transcript_included {
                if !item.stdout.is_empty() {
                    out.push_str("  - stdout:\n");
                    push_indented_block(&mut out, &render_raw_transcript(&item.stdout));
                }
                if !item.stderr.is_empty() {
                    out.push_str("  - stderr:\n");
                    push_indented_block(&mut out, &render_raw_transcript(&item.stderr));
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

pub fn has_single_comment_marker_section(body: &str) -> bool {
    let Some((_start, end)) = marked_section_bounds(body) else {
        return false;
    };
    !body[end..].contains(COMMENT_MARKER_START) && !body[end..].contains(COMMENT_MARKER_END)
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
    pub owned_by_ctx: bool,
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
        .filter(|comment| comment.owned_by_ctx && has_comment_markers(&comment.body))
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
    #[error("PR comment body must contain exactly one ctx marker-bounded section")]
    InvalidMarkedComment,
    #[error("invalid GitHub client configuration: {0}")]
    Config(String),
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
            GitHubClientError::InvalidMarkedComment => PublishError::InvalidMarkedComment,
            GitHubClientError::Config(message) => PublishError::Client(message),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCommandOutput {
    pub stdout: String,
    pub stderr: String,
}

pub trait GhCommandRunner {
    fn run_gh(
        &mut self,
        args: &[OsString],
    ) -> std::result::Result<GhCommandOutput, GitHubClientError>;
}

#[derive(Debug, Clone)]
pub struct StdGhCommandRunner {
    program: OsString,
}

impl Default for StdGhCommandRunner {
    fn default() -> Self {
        Self {
            program: OsString::from("gh"),
        }
    }
}

impl StdGhCommandRunner {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl GhCommandRunner for StdGhCommandRunner {
    fn run_gh(
        &mut self,
        args: &[OsString],
    ) -> std::result::Result<GhCommandOutput, GitHubClientError> {
        let output = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| {
                GitHubClientError::Transport(format!(
                    "failed to execute {}: {err}",
                    self.program.to_string_lossy()
                ))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.success() {
            Ok(GhCommandOutput { stdout, stderr })
        } else {
            Err(map_gh_stderr_to_error(
                output.status.code().unwrap_or_default(),
                &stderr,
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCliClientOptions {
    pub expected_author: Option<String>,
}

impl Default for GhCliClientOptions {
    fn default() -> Self {
        Self {
            expected_author: None,
        }
    }
}

pub struct GhCliGitHubPrCommentClient<R = StdGhCommandRunner> {
    runner: R,
    expected_author: Option<String>,
    authenticated_author: Option<String>,
}

impl GhCliGitHubPrCommentClient<StdGhCommandRunner> {
    pub fn new() -> Self {
        Self::with_runner(StdGhCommandRunner::default())
    }
}

impl Default for GhCliGitHubPrCommentClient<StdGhCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> GhCliGitHubPrCommentClient<R> {
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            expected_author: None,
            authenticated_author: None,
        }
    }

    pub fn with_runner_and_options(
        runner: R,
        options: GhCliClientOptions,
    ) -> std::result::Result<Self, GitHubClientError> {
        let expected_author = options
            .expected_author
            .map(normalize_expected_author)
            .transpose()?;
        Ok(Self {
            runner,
            expected_author,
            authenticated_author: None,
        })
    }

    pub fn into_runner(self) -> R {
        self.runner
    }
}

impl<R: GhCommandRunner> GitHubPrCommentClient for GhCliGitHubPrCommentClient<R> {
    fn list_comments(
        &mut self,
        target: &PullRequestTarget,
    ) -> std::result::Result<Vec<PullRequestComment>, GitHubClientError> {
        let author = self.ctx_author_login()?;
        let endpoint = format!(
            "/repos/{}/{}/issues/{}/comments",
            target.owner, target.repo, target.number
        );
        let output = self.runner.run_gh(&[
            OsString::from("api"),
            OsString::from("--paginate"),
            OsString::from(endpoint),
            OsString::from("--jq"),
            OsString::from(COMMENT_JQ_TSV),
        ])?;
        let comments = parse_gh_comment_tsv_list(&output.stdout)?;

        comments
            .into_iter()
            .map(|comment| map_api_comment(comment, &author))
            .collect()
    }

    fn create_comment(
        &mut self,
        target: &PullRequestTarget,
        body: &str,
    ) -> std::result::Result<PullRequestComment, GitHubClientError> {
        validate_publish_body(body)?;
        let author = self.ctx_author_login()?;
        let endpoint = format!(
            "/repos/{}/{}/issues/{}/comments",
            target.owner, target.repo, target.number
        );
        let output = self.runner.run_gh(&[
            OsString::from("api"),
            OsString::from("--method"),
            OsString::from("POST"),
            OsString::from(endpoint),
            OsString::from("--field"),
            OsString::from(format!("body={body}")),
            OsString::from("--jq"),
            OsString::from(COMMENT_JQ_TSV_SINGLE),
        ])?;
        map_api_comment(
            parse_gh_comment_tsv_line(output.stdout.trim_end())?,
            &author,
        )
    }

    fn update_comment(
        &mut self,
        target: &PullRequestTarget,
        comment_id: u64,
        body: &str,
    ) -> std::result::Result<PullRequestComment, GitHubClientError> {
        validate_publish_body(body)?;
        let author = self.ctx_author_login()?;
        let endpoint = format!(
            "/repos/{}/{}/issues/comments/{comment_id}",
            target.owner, target.repo
        );
        let output = self.runner.run_gh(&[
            OsString::from("api"),
            OsString::from("--method"),
            OsString::from("PATCH"),
            OsString::from(endpoint),
            OsString::from("--field"),
            OsString::from(format!("body={body}")),
            OsString::from("--jq"),
            OsString::from(COMMENT_JQ_TSV_SINGLE),
        ])?;
        map_api_comment(
            parse_gh_comment_tsv_line(output.stdout.trim_end())?,
            &author,
        )
    }
}

impl<R: GhCommandRunner> GhCliGitHubPrCommentClient<R> {
    fn ctx_author_login(&mut self) -> std::result::Result<String, GitHubClientError> {
        if let Some(author) = &self.expected_author {
            return Ok(author.clone());
        }
        if let Some(author) = &self.authenticated_author {
            return Ok(author.clone());
        }

        let output = self.runner.run_gh(&[
            OsString::from("api"),
            OsString::from("user"),
            OsString::from("--jq"),
            OsString::from(".login"),
        ])?;
        let author = normalize_expected_author(output.stdout.trim().to_owned())?;
        self.authenticated_author = Some(author.clone());
        Ok(author)
    }
}

const COMMENT_JQ_TSV: &str = r#".[] | [.id, (.body // ""), (.user.login // "")] | @tsv"#;
const COMMENT_JQ_TSV_SINGLE: &str = r#"[.id, (.body // ""), (.user.login // "")] | @tsv"#;

#[derive(Debug)]
struct ApiComment {
    id: u64,
    body: String,
    login: String,
}

fn normalize_expected_author(author: String) -> std::result::Result<String, GitHubClientError> {
    let author = author.trim().trim_start_matches('@').to_owned();
    if author.is_empty() || author.chars().any(char::is_whitespace) {
        return Err(GitHubClientError::Config(
            "expected GitHub author must be a non-empty login".into(),
        ));
    }
    Ok(author)
}

fn validate_publish_body(body: &str) -> std::result::Result<(), GitHubClientError> {
    if has_single_comment_marker_section(body) {
        Ok(())
    } else {
        Err(GitHubClientError::InvalidMarkedComment)
    }
}

fn parse_gh_comment_tsv_list(
    stdout: &str,
) -> std::result::Result<Vec<ApiComment>, GitHubClientError> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_gh_comment_tsv_line)
        .collect()
}

fn parse_gh_comment_tsv_line(line: &str) -> std::result::Result<ApiComment, GitHubClientError> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 3 {
        return Err(GitHubClientError::Transport(
            "failed to parse gh API response: expected id/body/login TSV fields".into(),
        ));
    }
    let id = fields[0].parse::<u64>().map_err(|err| {
        GitHubClientError::Transport(format!("failed to parse gh API comment id: {err}"))
    })?;
    Ok(ApiComment {
        id,
        body: unescape_jq_tsv_field(fields[1])?,
        login: unescape_jq_tsv_field(fields[2])?,
    })
}

fn unescape_jq_tsv_field(value: &str) -> std::result::Result<String, GitHubClientError> {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => {
                return Err(GitHubClientError::Transport(
                    "failed to parse gh API response: dangling TSV escape".into(),
                ));
            }
        }
    }
    Ok(out)
}

fn map_api_comment(
    comment: ApiComment,
    ctx_author: &str,
) -> std::result::Result<PullRequestComment, GitHubClientError> {
    let body = comment.body;
    let owned_by_ctx = comment.login.eq_ignore_ascii_case(ctx_author);
    if owned_by_ctx
        && (body.contains(COMMENT_MARKER_START) || body.contains(COMMENT_MARKER_END))
        && !has_single_comment_marker_section(&body)
    {
        return Err(GitHubClientError::InvalidMarkedComment);
    }
    Ok(PullRequestComment {
        id: comment.id,
        body,
        owned_by_ctx,
    })
}

fn map_gh_stderr_to_error(_status_code: i32, stderr: &str) -> GitHubClientError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("http 401")
        || lower.contains("bad credentials")
        || lower.contains("authentication required")
        || lower.contains("not logged into")
    {
        GitHubClientError::Unauthorized
    } else if lower.contains("rate limit")
        || lower.contains("secondary rate limit")
        || lower.contains("http 429")
    {
        GitHubClientError::RateLimited
    } else if lower.contains("http 403")
        || lower.contains("resource not accessible")
        || lower.contains("permission")
        || lower.contains("forbidden")
    {
        GitHubClientError::Forbidden
    } else if lower.contains("http 404") || lower.contains("not found") {
        GitHubClientError::NotFound
    } else {
        GitHubClientError::Transport(stderr.trim().to_owned())
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
    if !has_single_comment_marker_section(body) {
        return Err(PublishError::InvalidMarkedComment);
    }
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

fn render_share_text(value: &str) -> String {
    sanitize_comment_markers(&redact_share_safe_markers(value))
}

fn render_raw_transcript(value: &str) -> String {
    sanitize_comment_markers(value)
}

fn sanitize_comment_markers(value: &str) -> String {
    value
        .replace(COMMENT_MARKER_START, "[ctx-comment-marker-start-redacted]")
        .replace(COMMENT_MARKER_END, "[ctx-comment-marker-end-redacted]")
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
        assert!(rendered.markdown.contains("token=[REDACTED_SECRET]"));
        assert!(rendered.markdown.contains("password=[REDACTED_SECRET]"));
        assert!(rendered.markdown.contains("[REDACTED_PATH]"));
        assert!(rendered.markdown.contains("Transcript redacted by default"));
        assert!(!rendered.markdown.contains("hunter2"));
        assert!(!rendered.markdown.contains("secret=shhh"));
        assert!(!rendered.markdown.contains("raw stdout"));
    }

    #[test]
    fn raw_transcript_requires_explicit_opt_in() {
        let options = RenderOptions {
            raw_transcript: Some(
                RawTranscriptOptIn::acknowledge_private_data_risk("publishing to a private PR")
                    .unwrap(),
            ),
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
        assert!(rendered.markdown.contains("password=[REDACTED_SECRET]"));
        assert!(rendered.markdown.contains("raw stdout password=hunter2"));
    }

    #[test]
    fn raw_transcript_opt_in_requires_non_empty_reason() {
        assert!(matches!(
            RawTranscriptOptIn::acknowledge_private_data_risk("  "),
            Err(PublishError::InvalidRawTranscriptOptIn)
        ));
    }

    #[test]
    fn rendered_content_cannot_inject_comment_markers() {
        let record = record(
            COMMENT_MARKER_START,
            COMMENT_MARKER_END,
            vec![COMMENT_MARKER_START],
            10,
        );
        let options = RenderOptions {
            raw_transcript: Some(
                RawTranscriptOptIn::acknowledge_private_data_risk("private fixture").unwrap(),
            ),
        };
        let evidence = evidence(
            "cargo test",
            0,
            COMMENT_MARKER_END,
            COMMENT_MARKER_START,
            11,
        );

        let rendered = render_pr_comment(&[record], &[evidence], &options);

        assert_eq!(rendered.markdown.matches(COMMENT_MARKER_START).count(), 1);
        assert_eq!(rendered.markdown.matches(COMMENT_MARKER_END).count(), 1);
        assert!(rendered
            .markdown
            .contains("[ctx-comment-marker-start-redacted]"));
        assert!(rendered
            .markdown
            .contains("[ctx-comment-marker-end-redacted]"));
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
        assert!(!has_single_comment_marker_section(&reversed));
        assert!(!has_single_comment_marker_section("plain body"));
        assert!(!has_single_comment_marker_section(&format!(
            "{COMMENT_MARKER_START}\none\n{COMMENT_MARKER_END}\n{COMMENT_MARKER_START}\ntwo\n{COMMENT_MARKER_END}"
        )));
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
                    owned_by_ctx: true,
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
                        owned_by_ctx: false,
                    },
                    PullRequestComment {
                        id: 8,
                        body: format!("{COMMENT_MARKER_START}\nold\n{COMMENT_MARKER_END}\n"),
                        owned_by_ctx: true,
                    },
                ],
                &desired
            ),
            UpsertPlan::Update { comment_id: 8 }
        );
    }

    #[test]
    fn upsert_planning_ignores_unowned_marked_comments() {
        let desired = format!("{COMMENT_MARKER_START}\ndesired\n{COMMENT_MARKER_END}\n");
        let attacker = PullRequestComment {
            id: 1,
            body: format!("{COMMENT_MARKER_START}\nattacker\n{COMMENT_MARKER_END}\n"),
            owned_by_ctx: false,
        };

        assert_eq!(
            plan_comment_upsert(&[attacker], &desired),
            UpsertPlan::Create
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
            owned_by_ctx: true,
        }];
        let updated =
            upsert_github_pr_comment(&mut client, &target, &body, &PublishOptions::default())
                .unwrap();
        assert_eq!(updated, PublishOutcome::Updated { comment_id: 7 });
        assert_eq!(client.comments[0].body, body);
    }

    #[test]
    fn upsert_rejects_unmarked_or_ambiguous_desired_body() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let mut client = MockClient {
            comments: Vec::new(),
            calls: Vec::new(),
            fail_list: None,
        };

        let result = upsert_github_pr_comment(
            &mut client,
            &target,
            "plain body",
            &PublishOptions::default(),
        );

        assert!(matches!(result, Err(PublishError::InvalidMarkedComment)));
        assert!(client.calls.is_empty());
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

    #[test]
    fn gh_cli_client_derives_owned_comments_from_authenticated_user() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let mut client = GhCliGitHubPrCommentClient::with_runner(MockGhRunner {
            calls: Vec::new(),
            responses: vec![
                Ok(GhCommandOutput {
                    stdout: "ctx-bot\n".into(),
                    stderr: String::new(),
                }),
                Ok(GhCommandOutput {
                    stdout: "1\t<!-- ctx-work-record:finished-product:start -->\\nold\\n<!-- ctx-work-record:finished-product:end -->\\n\tctx-bot\n2\t<!-- ctx-work-record:finished-product:start -->\\nother\\n<!-- ctx-work-record:finished-product:end -->\\n\tsomeone-else\n".into(),
                    stderr: String::new(),
                }),
            ],
        });

        let comments = client.list_comments(&target).unwrap();
        let runner = client.into_runner();

        assert_eq!(comments.len(), 2);
        assert!(comments[0].owned_by_ctx);
        assert!(!comments[1].owned_by_ctx);
        assert_eq!(
            runner.calls,
            vec![
                vec!["api", "user", "--jq", ".login"],
                vec![
                    "api",
                    "--paginate",
                    "/repos/ctxrs/ctx/issues/1/comments",
                    "--jq",
                    COMMENT_JQ_TSV,
                ],
            ]
        );
    }

    #[test]
    fn gh_cli_client_uses_expected_author_without_auth_lookup() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let mut client = GhCliGitHubPrCommentClient::with_runner_and_options(
            MockGhRunner {
                calls: Vec::new(),
                responses: vec![Ok(GhCommandOutput {
                    stdout: "3\tplain\tctx-bot\n".into(),
                    stderr: String::new(),
                })],
            },
            GhCliClientOptions {
                expected_author: Some("@ctx-bot".into()),
            },
        )
        .unwrap();

        let comments = client.list_comments(&target).unwrap();
        let runner = client.into_runner();

        assert_eq!(comments[0].owned_by_ctx, true);
        assert_eq!(
            runner.calls,
            vec![vec![
                "api",
                "--paginate",
                "/repos/ctxrs/ctx/issues/1/comments",
                "--jq",
                COMMENT_JQ_TSV,
            ]]
        );
    }

    #[test]
    fn gh_cli_client_rejects_unmarked_or_ambiguous_outgoing_bodies() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let mut client = GhCliGitHubPrCommentClient::with_runner_and_options(
            MockGhRunner {
                calls: Vec::new(),
                responses: Vec::new(),
            },
            GhCliClientOptions {
                expected_author: Some("ctx-bot".into()),
            },
        )
        .unwrap();

        let error = client.create_comment(&target, "plain body").unwrap_err();
        assert!(matches!(error, GitHubClientError::InvalidMarkedComment));

        let ambiguous = format!(
            "{COMMENT_MARKER_START}\none\n{COMMENT_MARKER_END}\n{COMMENT_MARKER_START}\ntwo\n{COMMENT_MARKER_END}\n"
        );
        let error = client.update_comment(&target, 7, &ambiguous).unwrap_err();
        assert!(matches!(error, GitHubClientError::InvalidMarkedComment));
        assert!(client.into_runner().calls.is_empty());
    }

    #[test]
    fn gh_cli_client_rejects_ambiguous_owned_remote_comments() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let mut client = GhCliGitHubPrCommentClient::with_runner_and_options(
            MockGhRunner {
                calls: Vec::new(),
                responses: vec![Ok(GhCommandOutput {
                    stdout: "4\t<!-- ctx-work-record:finished-product:start -->\\nold\\n<!-- ctx-work-record:finished-product:end -->\\n<!-- ctx-work-record:finished-product:start -->\\nold2\\n<!-- ctx-work-record:finished-product:end -->\\n\tctx-bot\n".into(),
                    stderr: String::new(),
                })],
            },
            GhCliClientOptions {
                expected_author: Some("ctx-bot".into()),
            },
        )
        .unwrap();

        let error = client.list_comments(&target).unwrap_err();

        assert!(matches!(error, GitHubClientError::InvalidMarkedComment));
    }

    #[test]
    fn gh_cli_client_create_and_update_map_returned_comment() {
        let target =
            PullRequestTarget::github_from_url("https://github.com/ctxrs/ctx/pull/1").unwrap();
        let body = format!("{COMMENT_MARKER_START}\nnew\n{COMMENT_MARKER_END}\n");
        let mut client = GhCliGitHubPrCommentClient::with_runner_and_options(
            MockGhRunner {
                calls: Vec::new(),
                responses: vec![
                    Ok(GhCommandOutput {
                        stdout: "9\t<!-- ctx-work-record:finished-product:start -->\\nnew\\n<!-- ctx-work-record:finished-product:end -->\\n\tctx-bot\n".into(),
                        stderr: String::new(),
                    }),
                    Ok(GhCommandOutput {
                        stdout: "9\t<!-- ctx-work-record:finished-product:start -->\\nnew\\n<!-- ctx-work-record:finished-product:end -->\\n\tctx-bot\n".into(),
                        stderr: String::new(),
                    }),
                ],
            },
            GhCliClientOptions {
                expected_author: Some("ctx-bot".into()),
            },
        )
        .unwrap();

        let created = client.create_comment(&target, &body).unwrap();
        let updated = client.update_comment(&target, 9, &body).unwrap();
        let runner = client.into_runner();

        assert_eq!(created.id, 9);
        assert!(created.owned_by_ctx);
        assert_eq!(updated.id, 9);
        assert_eq!(
            runner.calls,
            vec![
                vec![
                    "api",
                    "--method",
                    "POST",
                    "/repos/ctxrs/ctx/issues/1/comments",
                    "--field",
                    &format!("body={body}"),
                    "--jq",
                    COMMENT_JQ_TSV_SINGLE,
                ],
                vec![
                    "api",
                    "--method",
                    "PATCH",
                    "/repos/ctxrs/ctx/issues/comments/9",
                    "--field",
                    &format!("body={body}"),
                    "--jq",
                    COMMENT_JQ_TSV_SINGLE,
                ],
            ]
        );
    }

    #[test]
    fn gh_cli_error_mapping_exposes_clear_publish_errors() {
        assert!(matches!(
            PublishError::from(map_gh_stderr_to_error(1, "gh: Bad credentials (HTTP 401)")),
            PublishError::AuthRequired
        ));
        assert!(matches!(
            PublishError::from(map_gh_stderr_to_error(
                1,
                "gh: Resource not accessible by integration (HTTP 403)"
            )),
            PublishError::PermissionDenied
        ));
        assert!(matches!(
            PublishError::from(map_gh_stderr_to_error(1, "gh: Not Found (HTTP 404)")),
            PublishError::PullRequestNotFound
        ));
        assert!(matches!(
            PublishError::from(map_gh_stderr_to_error(
                1,
                "API rate limit exceeded for user"
            )),
            PublishError::RateLimited
        ));
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

    struct MockGhRunner {
        calls: Vec<Vec<String>>,
        responses: Vec<std::result::Result<GhCommandOutput, GitHubClientError>>,
    }

    impl GhCommandRunner for MockGhRunner {
        fn run_gh(
            &mut self,
            args: &[OsString],
        ) -> std::result::Result<GhCommandOutput, GitHubClientError> {
            self.calls.push(
                args.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
            );
            self.responses.remove(0)
        }
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
                owned_by_ctx: true,
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
                owned_by_ctx: true,
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
