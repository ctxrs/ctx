//! Explicit, read-only GitHub PR inspection. Ordinary graph queries never call this module.
//! GitHub data is untrusted; errors omit child output and credentials stay in gh's environment.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ingest::{CommandAdapter, run_command},
    model::{GraphSnapshot, Node},
};

const FIELDS: &str = "number,title,state,headRefName,baseRefName,author,isDraft,isCrossRepository,reviewDecision,statusCheckRollup,updatedAt,files,changedFiles";
const MAX_PRS: usize = 100;
const MAX_FILES: usize = 3000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, Args)]
pub struct PrsArgs {
    /// Inspect one PR, including closed or merged PRs.
    #[arg(value_parser = parse_number)]
    pub number: Option<u64>,
    /// GitHub OWNER/REPO; otherwise use this checkout's origin.
    #[arg(short = 'R', long)]
    pub repo: Option<String>,
    /// Expected base; otherwise use the repository's actual default branch.
    #[arg(short = 'b', long)]
    pub base: Option<String>,
    /// Explain the deterministic review queue (no model call).
    #[arg(long)]
    pub triage: bool,
    /// Include verified local branch/worktree mappings.
    #[arg(long)]
    pub worktrees: bool,
    /// Include changed-file/community overlaps, not mergeability predictions.
    #[arg(long)]
    pub conflicts: bool,
    /// Also display PRs targeting a different base.
    #[arg(long)]
    pub wrong_base: bool,
}

fn parse_number(value: &str) -> Result<u64, String> {
    let value = value.strip_prefix('#').unwrap_or(value);
    let number = value
        .parse::<u64>()
        .map_err(|_| "PR number must be a positive integer".to_owned())?;
    if number == 0 || number > i32::MAX as u64 {
        return Err("PR number is outside the supported range".into());
    }
    Ok(number)
}

/// Runtime-only overrides for isolated tests. Never saved in graph/configuration.
#[derive(Debug, Clone)]
pub struct PrsRuntime {
    pub gh_program: String,
    pub git_program: String,
    pub cwd: PathBuf,
    /// Shared budget for all child commands, rounded up to whole seconds per call.
    pub timeout_secs: u64,
    /// Separate stdout/stderr cap for each command.
    pub max_output_bytes: usize,
    pub max_prs: usize,
    pub max_files: usize,
}

impl Default for PrsRuntime {
    fn default() -> Self {
        Self {
            gh_program: "gh".into(),
            git_program: "git".into(),
            cwd: PathBuf::from("."),
            timeout_secs: 30,
            max_output_bytes: MAX_BYTES,
            max_prs: 50,
            max_files: MAX_FILES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub author: Option<Author>,
    pub is_draft: bool,
    pub is_cross_repository: bool,
    pub review_decision: Option<String>,
    pub status_check_rollup: Option<Vec<Check>>,
    pub updated_at: String,
    pub files: Vec<ChangedFile>,
    pub changed_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    pub login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub conclusion: Option<String>,
    pub status: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
}

/// Caller-provided data can be used without running any executable or network operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrsData {
    pub repo: String,
    pub expected_base: String,
    pub prs: Vec<PullRequest>,
    pub list_may_be_truncated: bool,
    pub worktrees: Vec<Worktree>,
    pub worktrees_available: bool,
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CiStatus {
    Failure,
    Pending,
    Success,
    None,
    Unknown,
}

/// Stored identities retain their integer/string type and composition namespace.
/// Computed identities belong only to this snapshot and never replace stored IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommunityIdentity {
    pub source: String,
    pub project: Vec<String>,
    pub id: Value,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Impact {
    pub generation: u64,
    pub graph_kind: String,
    pub graph_root: Option<String>,
    pub graph_metadata: Value,
    /// Exact node records retain original IDs, source paths and composition provenance.
    pub nodes: Vec<Node>,
    /// Preserved groups when supplied; otherwise computed structural groups.
    pub communities: Vec<CommunityIdentity>,
    pub nodes_without_community: usize,
    pub unmatched_files: Vec<String>,
    pub ambiguous_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrEntry {
    pub pr: PullRequest,
    pub ci: CiStatus,
    pub status: String,
    pub reasons: Vec<String>,
    pub age_days: Option<u64>,
    pub files_complete: bool,
    pub impact: Option<Impact>,
    pub worktrees: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overlap {
    pub prs: [u64; 2],
    pub files: Vec<String>,
    pub communities: Vec<CommunityIdentity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrsReport {
    pub repo: String,
    pub expected_base: String,
    pub observed_at_unix_secs: u64,
    pub list_may_be_truncated: bool,
    pub hidden_wrong_base: usize,
    pub graph_available: bool,
    pub worktrees_available: bool,
    pub worktrees: Vec<Worktree>,
    pub entries: Vec<PrEntry>,
    pub overlaps: Vec<Overlap>,
    pub triage_method: String,
    pub notices: Vec<String>,
}

/// Validate before handing a repository to gh or matching a server allowlist.
/// Only github.com OWNER/REPO identities are accepted; no URL, host or flags.
pub fn validate_repo(repo: &str) -> Result<()> {
    let parts: Vec<_> = repo.split('/').collect();
    ensure!(parts.len() == 2, "repository must be GitHub OWNER/REPO");
    ensure!(
        parts[0].len() <= 39 && parts[1].len() <= 100,
        "repository name is too long"
    );
    for part in parts {
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.starts_with('-')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
            "repository must be GitHub OWNER/REPO"
        );
    }
    Ok(())
}

fn validate_args(args: &PrsArgs) -> Result<()> {
    if let Some(repo) = &args.repo {
        validate_repo(repo)?;
    }
    if let Some(number) = args.number {
        parse_number(&number.to_string()).map_err(anyhow::Error::msg)?;
    }
    if let Some(base) = &args.base {
        ensure!(
            !base.is_empty() && base.len() <= 1024 && !base.chars().any(char::is_control),
            "invalid expected base"
        );
    }
    Ok(())
}

struct Commands<'a> {
    runtime: &'a PrsRuntime,
    started: Instant,
}
impl Commands<'_> {
    fn call(&self, program: &str, args: Vec<String>) -> Result<String> {
        let left = std::time::Duration::from_secs(self.runtime.timeout_secs)
            .checked_sub(self.started.elapsed())
            .context("PR inspection timed out")?;
        ensure!(!left.is_zero(), "PR inspection timed out");
        run_command(&CommandAdapter { program: program.into(), args, output_file: false }, None, None,
            left.as_secs() + u64::from(left.subsec_nanos() > 0), self.runtime.max_output_bytes)
            .context("PR command failed; check installed gh/git and GitHub authentication (child output omitted)")
    }
    fn git(&self, args: &[&str]) -> Result<String> {
        let cwd = self
            .runtime
            .cwd
            .canonicalize()
            .context("cannot resolve PR working directory")?;
        let cwd = cwd.to_str().context("PR working directory must be UTF-8")?;
        // CommandAdapter expands placeholders; reject them in filesystem argv.
        ensure!(
            !cwd.contains('{') && !cwd.contains('}'),
            "unsupported braces in PR working directory"
        );
        let mut argv = vec!["-C".into(), cwd.into()];
        argv.extend(args.iter().map(|s| (*s).into()));
        self.call(&self.runtime.git_program, argv)
    }
    fn origin(&self) -> Result<String> {
        let raw = self.git(&["remote", "get-url", "origin"])?;
        let raw = raw.trim();
        let repo = raw
            .strip_prefix("https://github.com/")
            .or_else(|| raw.strip_prefix("git@github.com:"))
            .or_else(|| raw.strip_prefix("ssh://git@github.com/"))
            .context("origin is not a supported GitHub remote; use --repo OWNER/REPO")?;
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        validate_repo(repo)?;
        Ok(repo.to_owned())
    }
    fn gh_json<T: serde::de::DeserializeOwned>(&self, args: Vec<String>) -> Result<T> {
        let raw = self.call(&self.runtime.gh_program, args)?;
        // serde's data errors may echo values from child output; deliberately omit them.
        serde_json::from_str(&raw)
            .map_err(|_| anyhow::anyhow!("gh returned invalid or unexpected JSON"))
    }
}

/// Explicit network operation. Uses only gh repo view / pr list / pr view and
/// optionally local git remote get-url / worktree list. Never invokes a shell.
pub fn load(args: &PrsArgs, runtime: &PrsRuntime) -> Result<PrsData> {
    validate_args(args)?;
    ensure!(
        (1..=MAX_PRS).contains(&runtime.max_prs) && (1..=MAX_FILES).contains(&runtime.max_files),
        "invalid PR/file limit"
    );
    ensure!(
        (1..=MAX_BYTES).contains(&runtime.max_output_bytes)
            && (1..=120).contains(&runtime.timeout_secs),
        "invalid PR command limits"
    );
    let commands = Commands {
        runtime,
        started: Instant::now(),
    };
    let repo = match &args.repo {
        Some(repo) => repo.clone(),
        None => commands.origin()?,
    };
    let expected_base = match &args.base {
        Some(base) => base.clone(),
        None => {
            let value: Value = commands.gh_json(vec![
                "repo".into(),
                "view".into(),
                format!("github.com/{repo}"),
                "--json".into(),
                "defaultBranchRef".into(),
            ])?;
            value
                .pointer("/defaultBranchRef/name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .context("GitHub did not report a default branch; specify --base")?
                .to_owned()
        }
    };
    let mut argv = vec!["pr".into()];
    match args.number {
        Some(number) => argv.extend(["view".into(), number.to_string()]),
        None => argv.extend([
            "list".into(),
            "--state".into(),
            "open".into(),
            "--limit".into(),
            runtime.max_prs.to_string(),
        ]),
    }
    argv.extend([
        "--repo".into(),
        format!("github.com/{repo}"),
        "--json".into(),
        FIELDS.into(),
    ]);
    let prs: Vec<PullRequest> = if args.number.is_some() {
        vec![commands.gh_json(argv)?]
    } else {
        commands.gh_json(argv)?
    };
    ensure!(
        prs.len() <= runtime.max_prs && prs.iter().all(|p| p.files.len() <= runtime.max_files),
        "gh result exceeds PR/file count limit"
    );
    let mut data = PrsData {
        repo,
        expected_base,
        list_may_be_truncated: args.number.is_none() && prs.len() == runtime.max_prs,
        prs,
        worktrees: vec![],
        worktrees_available: false,
        notices: vec![],
    };
    if args.worktrees {
        match commands.origin() {
            Ok(origin) if origin.eq_ignore_ascii_case(&data.repo) => {
                match commands.git(&["worktree", "list", "--porcelain", "-z"]) {
                    Ok(raw) => { data.worktrees = parse_worktrees(&raw)?; data.worktrees_available = true; }
                    Err(_) => data.notices.push("Local worktree inspection unavailable; no mappings inferred.".into()),
                }
            }
            _ => data.notices.push("Worktree mapping unavailable: local origin does not identify the selected GitHub repository.".into()),
        }
    }
    Ok(data)
}

pub fn run(args: &PrsArgs, graph: Option<&GraphSnapshot>) -> Result<PrsReport> {
    run_with(args, graph, &PrsRuntime::default())
}

pub fn run_with(
    args: &PrsArgs,
    graph: Option<&GraphSnapshot>,
    runtime: &PrsRuntime,
) -> Result<PrsReport> {
    let data = load(args, runtime)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    report(args, data, graph, now)
}

/// Parse git's NUL-delimited format without confusing quoted, Unicode or detached paths.
pub fn parse_worktrees(raw: &str) -> Result<Vec<Worktree>> {
    ensure!(raw.len() <= MAX_BYTES, "worktree output exceeds byte limit");
    let mut entries = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in raw.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(old) = current.take() {
                entries.push(old);
            }
            ensure!(!path.is_empty(), "invalid worktree record");
            current = Some(Worktree {
                path: path.into(),
                branch: None,
            });
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
            if let Some(entry) = &mut current {
                entry.branch = Some(branch.into());
            }
        } else if field.is_empty()
            && let Some(old) = current.take()
        {
            entries.push(old);
        }
        ensure!(entries.len() <= 1000, "too many worktrees");
    }
    if let Some(old) = current {
        entries.push(old);
    }
    ensure!(entries.len() <= 1000, "too many worktrees");
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub fn ci_status(checks: Option<&[Check]>) -> CiStatus {
    let Some(checks) = checks else {
        return CiStatus::Unknown;
    };
    if checks.is_empty() {
        return CiStatus::None;
    }
    let mut pending = false;
    let mut unknown = false;
    for check in checks {
        let conclusion = check.conclusion.as_deref().unwrap_or("");
        let status = check.status.as_deref().unwrap_or("");
        let state = check.state.as_deref().unwrap_or("");
        if [
            "FAILURE",
            "ERROR",
            "CANCELLED",
            "TIMED_OUT",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
            "STALE",
        ]
        .contains(&conclusion)
            || ["FAILURE", "ERROR"].contains(&state)
        {
            return CiStatus::Failure;
        }
        if ["IN_PROGRESS", "QUEUED", "PENDING", "WAITING", "REQUESTED"].contains(&status)
            || ["PENDING", "EXPECTED"].contains(&state)
        {
            pending = true;
        } else if !["SUCCESS", "NEUTRAL", "SKIPPED"].contains(&conclusion) && state != "SUCCESS" {
            unknown = true;
        }
    }
    if pending {
        CiStatus::Pending
    } else if unknown {
        CiStatus::Unknown
    } else {
        CiStatus::Success
    }
}

// GitHub's UTC second-precision timestamp. Invalid/future timestamps stay unknown,
// rather than classifying malformed input as old. Gregorian conversion, no locale.
fn timestamp(text: &str) -> Option<u64> {
    if text.len() != 20
        || !text.is_ascii()
        || &text[4..5] != "-"
        || &text[7..8] != "-"
        || &text[10..11] != "T"
        || &text[13..14] != ":"
        || &text[16..17] != ":"
        || &text[19..] != "Z"
    {
        return None;
    }
    let n = |a, b| {
        let part = &text[a..b];
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse::<u64>().ok()
        } else {
            None
        }
    };
    let (y, m, d, h, min, s) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || h > 23 || min > 59 || s > 59 {
        return None;
    }
    let leap = |y: u64| y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let months = [
        31,
        28 + u64::from(leap(y)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if d == 0 || d > months[(m - 1) as usize] {
        return None;
    }
    let before = |y: u64| (y - 1) / 4 - (y - 1) / 100 + (y - 1) / 400;
    let days = (y - 1970) * 365 + before(y) - before(1970)
        + months[..(m - 1) as usize].iter().sum::<u64>()
        + d
        - 1;
    Some(days * 86400 + h * 3600 + min * 60 + s)
}

fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains('\0')
        && !path
            .split('/')
            .any(|p| p == "." || p == ".." || p.is_empty())
}

fn boundary_match(a: &str, b: &str) -> bool {
    a == b
        || a.strip_suffix(b)
            .is_some_and(|prefix| prefix.ends_with('/'))
        || b.strip_suffix(a)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

fn namespace(node: &Node) -> String {
    let mut value = &node.metadata;
    let mut projects = Vec::new();
    while let (Some(project), Some(_), Some(original)) = (
        value.get("project").and_then(Value::as_str),
        value.get("original_id").and_then(Value::as_str),
        value.get("original_metadata"),
    ) {
        projects.push(project);
        value = original;
    }
    serde_json::to_string(&projects).expect("string vector serializes")
}

/// Match the MCP structural-analysis budget without allocating a serialized copy.
/// This gates computation only; larger snapshots still support direct PR impact.
fn community_limit_notice(graph: &GraphSnapshot) -> Result<Option<&'static str>> {
    let references = graph
        .metadata
        .get("graf_unresolved_references")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if graph.nodes.len() > 5_000 || graph.edges.len() > 20_000 || references > 20_000 {
        return Ok(Some(
            "Computed communities omitted: unlabeled snapshot exceeds the analysis limit of 5000 nodes, 20000 edges or 20000 unresolved references. File/node impact remains available; empty communities do not mean no community overlap.",
        ));
    }
    struct ByteLimit {
        bytes: usize,
        exceeded: bool,
    }
    impl std::io::Write for ByteLimit {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len() > MAX_BYTES - self.bytes {
                self.exceeded = true;
                return Err(std::io::Error::other("analysis byte limit"));
            }
            self.bytes += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut limit = ByteLimit {
        bytes: 0,
        exceeded: false,
    };
    let serialized = serde_json::to_writer(&mut limit, graph);
    if limit.exceeded {
        return Ok(Some(
            "Computed communities omitted: unlabeled snapshot exceeds the 8 MiB serialized analysis limit. File/node impact remains available; empty communities do not mean no community overlap.",
        ));
    }
    serialized.context("cannot measure PR analysis snapshot")?;
    Ok(None)
}

struct GraphIndex<'a> {
    graph: &'a GraphSnapshot,
    files: BTreeMap<(String, String), Vec<&'a Node>>,
    memberships: BTreeMap<String, String>,
    communities: BTreeMap<String, CommunityIdentity>,
    community_notice: Option<&'static str>,
}
impl<'a> GraphIndex<'a> {
    fn new(graph: &'a GraphSnapshot) -> Result<Self> {
        ensure!(
            graph.nodes.len() <= 100_000 && graph.edges.len() <= 1_000_000,
            "PR impact snapshot exceeds node/edge count limits"
        );
        crate::analysis::validate(graph)?;
        let preserved = crate::analysis::preserved_communities(graph);
        let mut memberships = BTreeMap::new();
        let mut communities = BTreeMap::new();
        let community_notice = if preserved.is_empty() {
            community_limit_notice(graph)?
        } else {
            None
        };
        if preserved.is_empty() && community_notice.is_none() {
            let analysis = crate::analysis::analyze(graph, &Default::default())?;
            for group in analysis.communities {
                let key = group.id.to_string();
                for node in group.nodes {
                    memberships.insert(node, key.clone());
                }
                communities.insert(
                    key,
                    CommunityIdentity {
                        source: "computed".into(),
                        project: vec![],
                        id: serde_json::json!(group.id),
                        names: vec![group.label],
                    },
                );
            }
        } else {
            // Do not mix newly computed IDs with partially recorded memberships.
            for group in preserved {
                let key = serde_json::json!([group.project, group.id]).to_string();
                for node in group.nodes {
                    memberships.insert(node, key.clone());
                }
                communities.insert(
                    key,
                    CommunityIdentity {
                        source: "stored".into(),
                        project: group.project,
                        id: group.id,
                        names: group.names,
                    },
                );
            }
        }
        let mut files: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for node in &graph.nodes {
            if !node.file.is_empty() {
                files
                    .entry((namespace(node), node.file.clone()))
                    .or_default()
                    .push(node);
            }
        }
        Ok(Self {
            graph,
            files,
            memberships,
            communities,
            community_notice,
        })
    }
    fn impact(&self, paths: &[ChangedFile]) -> Impact {
        let mut selected: BTreeSet<(String, String)> = BTreeSet::new();
        let mut unmatched = Vec::new();
        let mut ambiguous = Vec::new();
        for changed in paths {
            let matches: Vec<_> = self
                .files
                .keys()
                .filter(|(_, file)| {
                    let relative = self
                        .graph
                        .root
                        .as_deref()
                        .and_then(|root| file.strip_prefix(root.trim_end_matches('/')))
                        .and_then(|tail| tail.strip_prefix('/'))
                        .unwrap_or(file);
                    boundary_match(relative, &changed.path)
                })
                .collect();
            // Exact path wins over a weaker suffix, but duplicate composed sources
            // remain ambiguous: choosing a project by basename would erase provenance.
            let exact: Vec<_> = matches
                .iter()
                .copied()
                .filter(|(_, file)| {
                    file == &changed.path
                        || self.graph.root.as_deref().is_some_and(|root| {
                            file == &format!(
                                "{}/{path}",
                                root.trim_end_matches('/'),
                                path = changed.path
                            )
                        })
                })
                .collect();
            let candidates = if exact.is_empty() { matches } else { exact };
            match candidates.as_slice() {
                [] => unmatched.push(changed.path.clone()),
                [key] => {
                    selected.insert((**key).clone());
                }
                _ => ambiguous.push(changed.path.clone()),
            }
        }
        let mut nodes: Vec<Node> = selected
            .iter()
            .flat_map(|key| self.files[key].iter().map(|node| (*node).clone()))
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let nodes_without_community = nodes
            .iter()
            .filter(|node| !self.memberships.contains_key(&node.id))
            .count();
        let communities = nodes
            .iter()
            .filter_map(|node| self.memberships.get(&node.id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|key| self.communities[key].clone())
            .collect();
        Impact {
            generation: self.graph.generation,
            graph_kind: self.graph.kind.clone(),
            graph_root: self.graph.root.clone(),
            graph_metadata: self.graph.metadata.clone(),
            nodes,
            communities,
            nodes_without_community,
            unmatched_files: unmatched,
            ambiguous_files: ambiguous,
        }
    }
}

/// Pure, deterministic projection of a captured GitHub response and optional snapshot.
/// Triage is a transparent attention queue, not an assessment of business value.
pub fn report(
    args: &PrsArgs,
    mut data: PrsData,
    graph: Option<&GraphSnapshot>,
    now_unix_secs: u64,
) -> Result<PrsReport> {
    validate_args(args)?;
    validate_repo(&data.repo)?;
    ensure!(
        args.repo
            .as_ref()
            .is_none_or(|repo| repo.eq_ignore_ascii_case(&data.repo)),
        "PR fixture repository mismatch"
    );
    if let Some(base) = &args.base {
        data.expected_base = base.clone();
    }
    ensure!(!data.expected_base.is_empty(), "expected base is missing");
    ensure!(
        data.prs.len() <= MAX_PRS && data.worktrees.len() <= 1000,
        "too many PRs/worktrees"
    );
    let index = graph.map(GraphIndex::new).transpose()?;
    if let Some(notice) = index.as_ref().and_then(|index| index.community_notice) {
        data.notices.push(notice.into());
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    let mut hidden_wrong_base = 0;
    for mut pr in data.prs {
        parse_number(&pr.number.to_string()).map_err(anyhow::Error::msg)?;
        ensure!(seen.insert(pr.number), "duplicate PR number");
        ensure!(
            pr.files.len() <= MAX_FILES && pr.files.iter().all(|file| path_valid(&file.path)),
            "invalid or excessive changed-file paths"
        );
        if args.number.is_some_and(|number| pr.number != number) {
            continue;
        }
        let wrong = pr.base_ref_name != data.expected_base;
        if wrong && !args.wrong_base && args.number.is_none() {
            hidden_wrong_base += 1;
            continue;
        }
        pr.files.sort_by(|a, b| a.path.cmp(&b.path));
        pr.files.dedup_by(|a, b| a.path == b.path);
        let files_complete = pr.files.len() == pr.changed_files;
        let ci = ci_status(pr.status_check_rollup.as_deref());
        let age_days = timestamp(&pr.updated_at)
            .and_then(|updated| now_unix_secs.checked_sub(updated))
            .map(|age| age / 86400);
        let review = pr.review_decision.as_deref().unwrap_or("");
        let mut reasons = Vec::new();
        if wrong {
            reasons.push(format!(
                "Targets {}; expected {}",
                pr.base_ref_name, data.expected_base
            ));
        }
        reasons.push(format!(
            "CI: {ci:?}; review: {}",
            if review.is_empty() {
                "not reported"
            } else {
                review
            }
        ));
        if pr.is_draft {
            reasons.push("Draft PR".into());
        }
        if age_days.is_some_and(|age| age >= 14) {
            reasons.push("No update for at least 14 days".into());
        }
        if age_days.is_none() {
            reasons.push("Update age unknown (invalid or future timestamp)".into());
        }
        if !files_complete {
            reasons.push(
                "Changed-file response is incomplete; impact and overlaps are partial".into(),
            );
        }
        let status = if pr.state == "MERGED" {
            "MERGED"
        } else if pr.state == "CLOSED" {
            "CLOSED"
        } else if pr.state != "OPEN" {
            "UNKNOWN"
        } else if wrong {
            "WRONG-BASE"
        } else if ci == CiStatus::Failure {
            "CI-FAIL"
        } else if review == "CHANGES_REQUESTED" {
            "CHANGES-REQ"
        } else if pr.is_draft {
            "DRAFT"
        } else if age_days.is_some_and(|age| age >= 14) {
            "STALE"
        } else if ci == CiStatus::Pending {
            "PENDING"
        } else if ci == CiStatus::Unknown {
            "CI-UNKNOWN"
        } else if ci == CiStatus::None {
            "NO-CHECKS"
        } else if review == "APPROVED" {
            "APPROVED"
        } else {
            "REVIEW"
        };
        let impact = index.as_ref().map(|index| index.impact(&pr.files));
        let worktrees = if args.worktrees && data.worktrees_available && !pr.is_cross_repository {
            data.worktrees
                .iter()
                .filter(|w| w.branch.as_deref() == Some(pr.head_ref_name.as_str()))
                .map(|w| w.path.clone())
                .collect()
        } else {
            vec![]
        };
        entries.push(PrEntry {
            pr,
            ci,
            status: status.into(),
            reasons,
            age_days,
            files_complete,
            impact,
            worktrees,
        });
    }
    if args.number.is_some() && entries.is_empty() {
        bail!("requested PR was absent from the response");
    }
    const ORDER: &[&str] = &[
        "WRONG-BASE",
        "CI-FAIL",
        "CHANGES-REQ",
        "DRAFT",
        "STALE",
        "PENDING",
        "CI-UNKNOWN",
        "NO-CHECKS",
        "APPROVED",
        "REVIEW",
        "CLOSED",
        "MERGED",
        "UNKNOWN",
    ];
    entries.sort_by_key(|e| {
        (
            ORDER
                .iter()
                .position(|s| *s == e.status)
                .unwrap_or(ORDER.len()),
            std::cmp::Reverse(e.age_days.unwrap_or(0)),
            e.pr.number,
        )
    });
    let mut overlaps = Vec::new();
    if args.conflicts || args.triage {
        for (i, a) in entries.iter().enumerate() {
            for b in &entries[i + 1..] {
                if a.pr.state != "OPEN"
                    || b.pr.state != "OPEN"
                    || a.pr.base_ref_name != b.pr.base_ref_name
                {
                    continue;
                }
                let other: BTreeSet<_> = b.pr.files.iter().map(|f| &f.path).collect();
                let files: Vec<_> =
                    a.pr.files
                        .iter()
                        .filter(|f| other.contains(&f.path))
                        .map(|f| f.path.clone())
                        .collect();
                let communities: Vec<_> = match (&a.impact, &b.impact) {
                    (Some(x), Some(y)) => x
                        .communities
                        .iter()
                        .filter(|c| y.communities.contains(c))
                        .cloned()
                        .collect(),
                    _ => vec![],
                };
                if !files.is_empty() || !communities.is_empty() {
                    let mut prs = [a.pr.number, b.pr.number];
                    prs.sort();
                    overlaps.push(Overlap {
                        prs,
                        files,
                        communities,
                    });
                }
            }
        }
        overlaps.sort_by_key(|overlap| overlap.prs);
    }
    if graph.is_some() {
        data.notices.push("Impact matches paths in the supplied snapshot; its repository identity and revision are not verified against the PR. Communities are snapshot-local structural groups, not merge conflicts.".into());
    }
    if graph.is_none() {
        data.notices
            .push("Graph unavailable: node/community impact was not computed.".into());
    }
    if data.list_may_be_truncated {
        data.notices
            .push("PR list reached its limit; the queue may omit additional PRs.".into());
    }
    if args.conflicts || args.triage {
        data.notices.push("Overlaps identify review risk, not textual conflicts or safe merge order; absence is not proof of independence.".into());
    }
    Ok(PrsReport {
        repo: data.repo,
        expected_base: data.expected_base,
        observed_at_unix_secs: now_unix_secs,
        list_may_be_truncated: data.list_may_be_truncated,
        hidden_wrong_base,
        graph_available: graph.is_some(),
        worktrees_available: args.worktrees && data.worktrees_available,
        worktrees: if args.worktrees {
            data.worktrees
        } else {
            vec![]
        },
        entries,
        overlaps,
        notices: data.notices,
        triage_method: format!(
            "Attention order: {}; ties use oldest update then PR number. All facts retained. No model, business-value ranking or merge recommendation.",
            ORDER.join(", ")
        ),
    })
}

/// Terminal-safe text; JSON retains the original Unicode and source records.
pub fn format_text(report: &PrsReport) -> String {
    let safe = |s: &str| {
        s.chars()
            .flat_map(|c| {
                if c.is_control() {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect::<String>()
    };
    let mut lines = vec![format!(
        "PRs for {} · base {} · {} shown ({} wrong-base hidden)",
        safe(&report.repo),
        safe(&report.expected_base),
        report.entries.len(),
        report.hidden_wrong_base
    )];
    for entry in &report.entries {
        lines.push(format!(
            "#{} [{}] {}",
            entry.pr.number,
            entry.status,
            safe(&entry.pr.title)
        ));
        lines.push(format!(
            "  {} -> {}; author {}; CI {:?}; age {}",
            safe(&entry.pr.head_ref_name),
            safe(&entry.pr.base_ref_name),
            safe(
                entry
                    .pr
                    .author
                    .as_ref()
                    .map_or("unknown", |a| a.login.as_str())
            ),
            entry.ci,
            entry
                .age_days
                .map_or("unknown".into(), |age| format!("{age}d"))
        ));
        for reason in &entry.reasons {
            lines.push(format!("  {}", safe(reason)));
        }
        lines.push(format!(
            "  {} changed files{}",
            entry.pr.files.len(),
            if entry.files_complete {
                ""
            } else {
                " (incomplete)"
            }
        ));
        if let Some(impact) = &entry.impact {
            lines.push(format!(
                "  {} directly affected nodes; communities {:?}; {} unmatched / {} ambiguous paths",
                impact.nodes.len(),
                impact.communities,
                impact.unmatched_files.len(),
                impact.ambiguous_files.len()
            ));
        }
        for path in &entry.worktrees {
            lines.push(format!("  worktree {}", safe(path)));
        }
    }
    for overlap in &report.overlaps {
        lines.push(format!(
            "Overlap #{} / #{}: files {}; communities {:?}",
            overlap.prs[0],
            overlap.prs[1],
            overlap
                .files
                .iter()
                .map(|f| safe(f))
                .collect::<Vec<_>>()
                .join(", "),
            overlap.communities
        ));
    }
    if report.worktrees_available {
        for worktree in &report.worktrees {
            lines.push(format!(
                "Local worktree {}: {}",
                safe(&worktree.path),
                safe(worktree.branch.as_deref().unwrap_or("detached HEAD"))
            ));
        }
    }
    lines.push(report.triage_method.clone());
    lines.extend(report.notices.iter().map(|s| safe(s)));
    lines.join("\n") + "\n"
}
