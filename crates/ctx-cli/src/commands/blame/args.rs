use clap::{Args, Subcommand, ValueEnum};
use ctx_attribution_model::{BlameTarget, LineRange, MAX_BLAME_RESULTS};

use crate::output::JsonOutputFormat;

const DEFAULT_BLAME_LIMIT: u32 = 20;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub(crate) struct BlameArgs {
    #[arg(value_name = "TARGET", required = true)]
    target: Option<String>,
    #[arg(long = "type", value_enum)]
    target_type: Option<BlameTargetType>,
    #[arg(long, value_name = "START[:END]", value_parser = parse_line_range)]
    lines: Option<LineRange>,
    #[command(flatten)]
    options: BlameOptions,
    #[command(subcommand)]
    command: Option<ExplicitBlame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BlameTargetType {
    File,
    Commit,
    Pr,
}

#[derive(Debug, Subcommand)]
enum ExplicitBlame {
    #[command(about = "Show cited agent provenance for committed file lines")]
    File(FileArgs),
    #[command(about = "Show cited agent provenance and exact lineage for a commit")]
    Commit(TargetArgs),
    #[command(about = "Show cited pull-request membership and activity")]
    Pr(TargetArgs),
}

#[derive(Debug, Args)]
struct TargetArgs {
    #[arg(value_name = "TARGET")]
    target: String,
    #[command(flatten)]
    options: BlameOptions,
}

#[derive(Debug, Args)]
struct FileArgs {
    #[command(flatten)]
    target: TargetArgs,
    #[arg(long, value_name = "START[:END]", value_parser = parse_line_range)]
    lines: Option<LineRange>,
}

#[derive(Debug, Args)]
struct BlameOptions {
    #[arg(long)]
    repository: Option<String>,
    #[arg(long, default_value_t = DEFAULT_BLAME_LIMIT, value_parser = parse_blame_limit)]
    limit: u32,
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

impl BlameArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.options().format.is_json()
    }

    fn options(&self) -> &BlameOptions {
        match &self.command {
            Some(ExplicitBlame::File(args)) => &args.target.options,
            Some(ExplicitBlame::Commit(args) | ExplicitBlame::Pr(args)) => &args.options,
            None => &self.options,
        }
    }

    pub(crate) fn target(&self) -> Result<BlameTarget, String> {
        let (kind, target, lines) = match &self.command {
            Some(ExplicitBlame::File(args)) => (
                BlameTargetType::File,
                &args.target.target,
                args.lines.clone(),
            ),
            Some(ExplicitBlame::Commit(args)) => (BlameTargetType::Commit, &args.target, None),
            Some(ExplicitBlame::Pr(args)) => (BlameTargetType::Pr, &args.target, None),
            None => {
                let target = self.target.as_ref().ok_or("a blame target is required")?;
                let kind = self.target_type.or_else(|| classify_target(target)).ok_or(
                    "blame target type is ambiguous; use --type file, --type commit, or --type pr",
                )?;
                if self.lines.is_some() && kind != BlameTargetType::File {
                    return Err("--lines is only valid for file blame; use --type file if the target is a path".to_owned());
                }
                (kind, target, self.lines.clone())
            }
        };
        let repository = self.options().repository.clone();
        let target = match kind {
            BlameTargetType::File => BlameTarget::File {
                path: target.clone(),
                repository,
                lines,
            },
            BlameTargetType::Commit => BlameTarget::Commit {
                oid: target.clone(),
                repository,
            },
            BlameTargetType::Pr => BlameTarget::PullRequest {
                selector: target.clone(),
                repository,
            },
        };
        target.validate().map_err(|error| error.message)?;
        Ok(target)
    }

    pub(crate) fn limit(&self) -> u32 {
        self.options().limit
    }
    pub(crate) fn cursor(&self) -> Option<&str> {
        self.options().cursor.as_deref()
    }
}

// Preserve the released selector precedence: PR, hexadecimal commit, then path.
fn classify_target(target: &str) -> Option<BlameTargetType> {
    let pr_candidate = BlameTarget::PullRequest {
        selector: target.to_owned(),
        repository: Some("auto-detection".to_owned()),
    };
    if pr_candidate.validate().is_ok() {
        return Some(BlameTargetType::Pr);
    }
    if (4..=64).contains(&target.len()) && target.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(BlameTargetType::Commit);
    }
    if looks_like_file_path(target) {
        return Some(BlameTargetType::File);
    }
    None
}

fn looks_like_file_path(target: &str) -> bool {
    if target.contains("://") {
        return false;
    }
    if target.contains(['/', '\\']) {
        return true;
    }
    let Some((stem, extension)) = target.rsplit_once('.') else {
        return false;
    };
    (!stem.is_empty() || target.starts_with('.')) && !extension.is_empty()
}

fn parse_line_range(value: &str) -> Result<LineRange, String> {
    let mut parts = value.split(':');
    let start = parts
        .next()
        .unwrap_or_default()
        .parse::<u32>()
        .map_err(|error| format!("invalid line number: {error}"))?;
    let end = parts
        .next()
        .map(|part| part.parse::<u32>())
        .transpose()
        .map_err(|error| format!("invalid line number: {error}"))?
        .unwrap_or(start);
    if parts.next().is_some() {
        return Err("line range must be START or START:END with END >= START".to_owned());
    }
    let range = LineRange { start, end };
    range.validate().map_err(|error| error.message)?;
    Ok(range)
}

fn parse_blame_limit(value: &str) -> Result<u32, String> {
    let limit = value
        .parse::<u32>()
        .map_err(|error| format!("invalid blame limit: {error}"))?;
    if !(1..=MAX_BLAME_RESULTS).contains(&limit) {
        return Err(format!(
            "blame limit must be between 1 and {MAX_BLAME_RESULTS}"
        ));
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, CommandRoot};
    use clap::{CommandFactory, Parser};

    fn parsed(argv: &[&str]) -> BlameArgs {
        let CommandRoot::Blame(args) = Cli::try_parse_from(argv).unwrap().command else {
            panic!("Blame command expected");
        };
        args
    }

    #[test]
    fn both_forms_keep_globals_lines_limits_and_cursor() {
        let shorthand = parsed(&[
            "ctx",
            "--quiet",
            "blame",
            "src/a.rs",
            "--type",
            "file",
            "--lines",
            "3:8",
            "--repository",
            "repo",
            "--limit",
            "2",
            "--cursor",
            "continuation",
            "--format",
            "json",
        ]);
        let explicit = parsed(&[
            "ctx",
            "blame",
            "file",
            "src/a.rs",
            "--lines=3:8",
            "--repository=repo",
            "--limit=2",
            "--cursor=continuation",
            "--format=json",
            "--color=never",
        ]);
        assert_eq!(shorthand.target().unwrap(), explicit.target().unwrap());
        assert_eq!(shorthand.limit(), 2);
        assert_eq!(explicit.cursor(), Some("continuation"));
        assert!(explicit.json_output());
        assert_eq!(
            parsed(&["ctx", "blame", "file", "--", "-file.rs"])
                .target()
                .unwrap(),
            BlameTarget::File {
                path: "-file.rs".into(),
                repository: None,
                lines: None
            }
        );
        assert_eq!(
            parsed(&["ctx", "blame", "--type", "file", "--", "file"])
                .target()
                .unwrap(),
            BlameTarget::File {
                path: "file".into(),
                repository: None,
                lines: None
            }
        );
    }

    #[test]
    fn inference_retains_pr_commit_path_precedence_and_rejects_ambiguous_selectors() {
        for (target, expected) in [
            (
                "https://github.com/example/repo/pull/12",
                BlameTargetType::Pr,
            ),
            ("abcdef", BlameTargetType::Commit),
            (".gitignore", BlameTargetType::File),
            ("src/a.rs", BlameTargetType::File),
        ] {
            assert_eq!(classify_target(target), Some(expected));
        }
        assert!(parsed(&["ctx", "blame", "README"]).target().is_err());
        assert!(parsed(&["ctx", "blame", "12"]).target().is_err());
        assert!(matches!(
            parsed(&["ctx", "blame", "pr", "12", "--repository", "example/repo"])
                .target()
                .unwrap(),
            BlameTarget::PullRequest { .. }
        ));
        assert!(parsed(&["ctx", "blame", "abcdef", "--lines", "1"])
            .target()
            .is_err());
        for argv in [
            vec!["ctx", "blame", "file", "a.rs", "--lines", "0"],
            vec!["ctx", "blame", "file", "a.rs", "--lines", "3:2"],
            vec!["ctx", "blame", "file", "a.rs", "--limit", "0"],
            vec!["ctx", "blame", "commit", "abcdef", "--lines", "1"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
    }

    #[test]
    fn real_clap_help_and_man_tree_advertise_every_blame_form() {
        let mut root = Cli::command();
        root.build();
        assert!(root.find_subcommand("pro").is_none());
        assert!(root.find_subcommand("referral").is_none());
        let blame = root.find_subcommand_mut("blame").unwrap();
        let help = blame.render_long_help().to_string();
        for value in [
            "--type",
            "--lines",
            "--limit",
            "--cursor",
            "--repository",
            "file",
            "commit",
            "pr",
        ] {
            assert!(help.contains(value), "missing {value}: {help}");
        }
        let bundle = crate::docs::managed_man_bundle(&root).unwrap();
        for name in [
            "ctx-blame.1",
            "ctx-blame-file.1",
            "ctx-blame-commit.1",
            "ctx-blame-pr.1",
        ] {
            assert!(
                bundle
                    .pages
                    .iter()
                    .any(|page| page.name == name && !page.bytes.is_empty()),
                "missing {name}"
            );
        }
    }
}
