use std::path::{Component, Path, PathBuf};

use ctx_history_core::{RepositoryAbstentionReason, RepositoryEvidenceKind};

pub(super) const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_TOKENS: usize = 16_384;
const MAX_SEGMENTS: usize = 4_096;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_COMMAND_REPOSITORY_PATHS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ShellAbstention {
    pub(super) evidence_kind: RepositoryEvidenceKind,
    pub(super) reason: RepositoryAbstentionReason,
    pub(super) detail: &'static str,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct CommandAnalysis {
    pub(super) derived_effective_cwd: Option<PathBuf>,
    pub(super) repository_paths: Vec<CommandRepositoryPath>,
    pub(super) abstentions: Vec<ShellAbstention>,
    pub(super) blocks_session_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandRepositoryPath {
    pub(super) path: PathBuf,
    pub(super) evidence_kind: RepositoryEvidenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundedOutcomeOperation {
    Commit {
        rewrites_history: bool,
        exact_oid_output: bool,
    },
    PullRequestCreate,
    PullRequestMerge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedOutcomePlan {
    pub(crate) operation: BoundedOutcomeOperation,
    pub(crate) operation_repository_path: PathBuf,
    pub(crate) output_repository_path: Option<PathBuf>,
    pub(crate) expected_pr_repository_path: Option<Vec<String>>,
    pub(crate) expected_pr_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoundedOutcomePlanDisposition {
    Unrecognized,
    Planned(BoundedOutcomePlan),
    Abstained {
        reason: RepositoryAbstentionReason,
        detail: &'static str,
        plan: Option<Box<BoundedOutcomePlan>>,
    },
}

/// Recognizes an ordered, route-preserving outcome plan. Wrappers and prefix
/// assignments are never outcome authority because Codex does not supply a
/// typed executable/argv attestation for them.
#[cfg(test)]
fn bounded_outcome_operation(command: &str) -> Option<BoundedOutcomeOperation> {
    match bounded_outcome_plan(command, Path::new("/")) {
        BoundedOutcomePlanDisposition::Planned(plan) => Some(plan.operation),
        BoundedOutcomePlanDisposition::Unrecognized
        | BoundedOutcomePlanDisposition::Abstained { .. } => None,
    }
}

pub(crate) fn bounded_outcome_evidence_relevant(command: &str) -> bool {
    !matches!(
        bounded_outcome_plan(command, Path::new("/")),
        BoundedOutcomePlanDisposition::Unrecognized
    )
}

pub(crate) fn bounded_outcome_plan(command: &str, base: &Path) -> BoundedOutcomePlanDisposition {
    if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
        return BoundedOutcomePlanDisposition::Unrecognized;
    }
    let Ok((command, terminal)) = strip_comments_and_bound_heredocs(command) else {
        return outcome_abstained(
            RepositoryAbstentionReason::UnsupportedShell,
            "malformed_outcome_command",
        );
    };
    if terminal.is_some() {
        return outcome_abstained(
            RepositoryAbstentionReason::UnsupportedShell,
            "outcome_heredoc_is_unattested",
        );
    }
    let Ok(tokenization) = tokenize(&command) else {
        return outcome_abstained(
            RepositoryAbstentionReason::UnsupportedShell,
            "outcome_command_tokenization_failed",
        );
    };
    if tokenization.terminal_abstention.is_some() {
        return outcome_abstained(
            RepositoryAbstentionReason::UnsupportedShell,
            "dynamic_or_unsupported_outcome_shell",
        );
    }

    let mut current = Some(base.to_path_buf());
    let mut plan = None;
    for segment in tokenization.segments {
        if segment.first().is_some_and(|token| token == "cd") {
            let destination = match segment.as_slice() {
                [_, path] => lexical_absolute(path, current.as_deref()),
                [_, option, path] if option == "--" => lexical_absolute(path, current.as_deref()),
                _ => None,
            };
            let Some(destination) = destination else {
                return outcome_abstained(
                    RepositoryAbstentionReason::DynamicPath,
                    "outcome_cd_is_not_a_bounded_literal",
                );
            };
            current = Some(destination);
            continue;
        }
        if segment
            .first()
            .is_none_or(|token| !matches!(token.as_str(), "git" | "gh"))
        {
            let (unwrapped, _) = unwrap_command_wrappers(&segment);
            if unwrapped.is_some_and(|command| {
                matches!(command.first().map(String::as_str), Some("git" | "gh"))
            }) {
                return outcome_abstained(
                    RepositoryAbstentionReason::UnknownWrapper,
                    "outcome_wrapper_or_assignment_is_unattested",
                );
            }
            return BoundedOutcomePlanDisposition::Unrecognized;
        }
        match segment.first().map(String::as_str) {
            Some("git") => {
                let Some((subcommand, arguments, repository_path)) =
                    bounded_git_invocation(&segment, current.as_deref())
                else {
                    return outcome_abstained(
                        RepositoryAbstentionReason::DynamicPath,
                        "outcome_git_route_is_not_bounded",
                    );
                };
                match subcommand {
                    "commit" | "rebase" => {
                        if plan.is_some() {
                            return outcome_abstained(
                                RepositoryAbstentionReason::Ambiguous,
                                "multiple_outcome_operations",
                            );
                        }
                        plan = Some(BoundedOutcomePlan {
                            operation: BoundedOutcomeOperation::Commit {
                                rewrites_history: subcommand == "rebase"
                                    || arguments.iter().any(|argument| {
                                        argument == "--amend" || argument.starts_with("--amend=")
                                    }),
                                exact_oid_output: false,
                            },
                            operation_repository_path: repository_path,
                            output_repository_path: None,
                            expected_pr_repository_path: None,
                            expected_pr_number: None,
                        });
                    }
                    "rev-parse" if exact_head_oid_request(arguments) => {
                        let Some(BoundedOutcomePlan {
                            operation:
                                BoundedOutcomeOperation::Commit {
                                    exact_oid_output, ..
                                },
                            output_repository_path,
                            ..
                        }) = plan.as_mut()
                        else {
                            return outcome_abstained(
                                RepositoryAbstentionReason::ProviderOutputUnjoined,
                                "outcome_oid_request_precedes_commit_operation",
                            );
                        };
                        if output_repository_path.is_some() {
                            return outcome_abstained(
                                RepositoryAbstentionReason::Ambiguous,
                                "multiple_exact_oid_output_segments",
                            );
                        }
                        *exact_oid_output = true;
                        *output_repository_path = Some(repository_path);
                    }
                    subcommand if known_git_builtin(subcommand) => {}
                    _ => return BoundedOutcomePlanDisposition::Unrecognized,
                }
            }
            Some("gh") => {
                let Some((operation, expected_pr_repository_path, expected_pr_number)) =
                    bounded_gh_operation(&segment)
                else {
                    return BoundedOutcomePlanDisposition::Unrecognized;
                };
                if plan.is_some() {
                    return outcome_abstained(
                        RepositoryAbstentionReason::Ambiguous,
                        "multiple_outcome_operations",
                    );
                }
                let Some(repository_path) = current.clone() else {
                    return outcome_abstained(
                        RepositoryAbstentionReason::UnsafePath,
                        "gh_outcome_has_no_bounded_workdir",
                    );
                };
                plan = Some(BoundedOutcomePlan {
                    operation,
                    operation_repository_path: repository_path,
                    output_repository_path: None,
                    expected_pr_repository_path,
                    expected_pr_number,
                });
            }
            _ => return BoundedOutcomePlanDisposition::Unrecognized,
        }
    }
    let Some(plan) = plan else {
        return BoundedOutcomePlanDisposition::Unrecognized;
    };
    if plan
        .output_repository_path
        .as_ref()
        .is_some_and(|output| output != &plan.operation_repository_path)
    {
        return BoundedOutcomePlanDisposition::Abstained {
            reason: RepositoryAbstentionReason::ConflictingIdentity,
            detail: "operation_and_outcome_output_routes_conflict",
            plan: Some(Box::new(plan)),
        };
    }
    BoundedOutcomePlanDisposition::Planned(plan)
}

fn outcome_abstained(
    reason: RepositoryAbstentionReason,
    detail: &'static str,
) -> BoundedOutcomePlanDisposition {
    BoundedOutcomePlanDisposition::Abstained {
        reason,
        detail,
        plan: None,
    }
}

fn exact_head_oid_request(arguments: &[String]) -> bool {
    matches!(arguments, [head] if head == "HEAD")
        || matches!(arguments, [verify, head] if verify == "--verify" && head == "HEAD")
        || matches!(arguments, [verify, head] if verify == "--verify" && head == "HEAD^{commit}")
}

fn bounded_git_invocation<'a>(
    argv: &'a [String],
    base: Option<&Path>,
) -> Option<(&'a str, &'a [String], PathBuf)> {
    let mut repository_path = base.map(Path::to_path_buf);
    let mut index = 1;
    while let Some(token) = argv.get(index) {
        if token == "-C" {
            repository_path = lexical_absolute(argv.get(index + 1)?, repository_path.as_deref());
            index += 2;
        } else if token == "--" {
            index += 1;
            break;
        } else if matches!(
            token.as_str(),
            "--no-pager" | "--paginate" | "--literal-pathspecs"
        ) {
            index += 1;
        } else if token.starts_with('-') {
            return None;
        } else {
            break;
        }
    }
    let subcommand = argv.get(index)?.as_str();
    Some((
        subcommand,
        argv.get(index + 1..).unwrap_or_default(),
        repository_path?,
    ))
}

fn bounded_gh_operation(
    argv: &[String],
) -> Option<(BoundedOutcomeOperation, Option<Vec<String>>, Option<u64>)> {
    let [gh, group, operation, arguments @ ..] = argv else {
        return None;
    };
    if gh != "gh" || group != "pr" {
        return None;
    }
    let operation = match operation.as_str() {
        "create" => BoundedOutcomeOperation::PullRequestCreate,
        "merge" => BoundedOutcomeOperation::PullRequestMerge,
        _ => return None,
    };
    let mut expected_pr_number = None;
    let mut expected_repository = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let value = if matches!(argument.as_str(), "--repo" | "-R") {
            index += 1;
            Some(arguments.get(index)?.as_str())
        } else {
            argument.strip_prefix("--repo=")
        };
        if let Some(value) = value {
            let parts = value.split('/').map(str::to_owned).collect::<Vec<_>>();
            if parts.len() < 2
                || parts.iter().any(|part| {
                    part.is_empty()
                        || matches!(part.as_str(), "." | "..")
                        || part.bytes().any(|byte| {
                            byte.is_ascii_control() || matches!(byte, b'@' | b':' | b'\\')
                        })
                })
                || expected_repository.replace(parts).is_some()
            {
                return None;
            }
        } else if operation == BoundedOutcomeOperation::PullRequestMerge
            && !argument.starts_with('-')
        {
            if let Ok(number) = argument.parse::<u64>() {
                if number == 0 || expected_pr_number.replace(number).is_some() {
                    return None;
                }
            }
        }
        index += 1;
    }
    Some((operation, expected_repository, expected_pr_number))
}

pub(crate) fn lexical_absolute(value: &str, base: Option<&Path>) -> Option<PathBuf> {
    if value.is_empty()
        || value == "-"
        || value.starts_with('~')
        || value.as_bytes().contains(&0)
        || value.len() > MAX_PATH_BYTES
        || value
            .chars()
            .any(|character| matches!(character, '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return None;
    }
    let value = Path::new(value);
    let joined = if value.is_absolute() {
        value.to_path_buf()
    } else {
        base?.join(value)
    };
    normalize_absolute(&joined)
}

fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized.is_absolute().then_some(normalized)
}

pub(super) fn analyze(command: Option<&str>, base: Option<&Path>) -> CommandAnalysis {
    let Some(command) = command.filter(|command| !command.is_empty()) else {
        return CommandAnalysis::default();
    };
    if command.len() > MAX_COMMAND_BYTES {
        return abstain(
            RepositoryAbstentionReason::CommandTooLarge,
            "command_byte_limit_exceeded",
        );
    }
    let (static_command, static_abstention) = match strip_comments_and_bound_heredocs(command) {
        Ok(command) => command,
        Err(detail) => {
            return abstain(RepositoryAbstentionReason::UnsupportedShell, detail);
        }
    };
    let mut tokenization = match tokenize(&static_command) {
        Ok(tokenization) => tokenization,
        Err((reason, detail)) => return abstain(reason, detail),
    };
    if tokenization.terminal_abstention.is_none() {
        tokenization.terminal_abstention =
            static_abstention.map(|detail| (RepositoryAbstentionReason::UnsupportedShell, detail));
    }

    let mut analysis = CommandAnalysis::default();
    let mut current = base.map(Path::to_path_buf);
    let mut command_candidates = Vec::new();
    analysis.blocks_session_fallback = tokenization.blocks_session_fallback;
    let mut cut_off = false;
    for segment in tokenization.segments {
        if segment.first().is_some_and(|token| token == "cd") {
            let destination = match segment.as_slice() {
                [_, path] => lexical_absolute(path, current.as_deref()),
                [_, option, path] if option == "--" => lexical_absolute(path, current.as_deref()),
                _ => None,
            };
            let Some(destination) = destination else {
                analysis.blocks_session_fallback = true;
                push_analysis_abstention(
                    &mut analysis,
                    RepositoryAbstentionReason::DynamicPath,
                    "unsupported_or_dynamic_cd",
                );
                cut_off = true;
                break;
            };
            current = Some(destination.clone());
            analysis.derived_effective_cwd = Some(destination);
            continue;
        }

        let (git, wrapper_error) = unwrap_wrappers(&segment);
        let Some(git) = git else {
            preserve_derived_candidate(&mut analysis, &mut command_candidates);
            let (reason, detail) = opaque_segment_abstention(&segment, wrapper_error);
            push_analysis_abstention(&mut analysis, reason, detail);
            cut_off = true;
            break;
        };
        match parse_git(git, current.as_deref()) {
            Ok((path, has_git_c)) => {
                if has_git_c {
                    if !push_command_candidate(
                        &mut analysis,
                        &mut command_candidates,
                        CommandRepositoryPath {
                            path,
                            evidence_kind: RepositoryEvidenceKind::CommandSpecificRepositoryPath,
                        },
                    ) {
                        cut_off = true;
                        break;
                    }
                } else if analysis.derived_effective_cwd.is_some()
                    && !push_command_candidate(
                        &mut analysis,
                        &mut command_candidates,
                        CommandRepositoryPath {
                            path,
                            evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
                        },
                    )
                {
                    cut_off = true;
                    break;
                }
            }
            Err((reason, detail)) => {
                analysis.blocks_session_fallback |= matches!(
                    reason,
                    RepositoryAbstentionReason::DynamicPath
                        | RepositoryAbstentionReason::ConflictingIdentity
                );
                preserve_derived_candidate(&mut analysis, &mut command_candidates);
                push_analysis_abstention(&mut analysis, reason, detail);
                cut_off = true;
                break;
            }
        }
    }
    if !cut_off {
        if let Some((reason, detail)) = tokenization.terminal_abstention {
            analysis.blocks_session_fallback |= analysis.derived_effective_cwd.is_some();
            preserve_derived_candidate(&mut analysis, &mut command_candidates);
            push_analysis_abstention(&mut analysis, reason, detail);
        }
    }
    command_candidates.dedup_by(|left, right| {
        left.path == right.path && left.evidence_kind == right.evidence_kind
    });
    analysis.repository_paths = command_candidates;
    analysis
}

fn abstain(reason: RepositoryAbstentionReason, detail: &'static str) -> CommandAnalysis {
    CommandAnalysis {
        abstentions: vec![ShellAbstention {
            evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
            reason,
            detail,
        }],
        ..CommandAnalysis::default()
    }
}

fn push_analysis_abstention(
    analysis: &mut CommandAnalysis,
    reason: RepositoryAbstentionReason,
    detail: &'static str,
) {
    analysis.abstentions.push(ShellAbstention {
        evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
        reason,
        detail,
    });
}

fn preserve_derived_candidate(
    analysis: &mut CommandAnalysis,
    candidates: &mut Vec<CommandRepositoryPath>,
) {
    if let Some(path) = analysis.derived_effective_cwd.clone() {
        push_command_candidate(
            analysis,
            candidates,
            CommandRepositoryPath {
                path,
                evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
            },
        );
    }
}

fn push_command_candidate(
    analysis: &mut CommandAnalysis,
    candidates: &mut Vec<CommandRepositoryPath>,
    candidate: CommandRepositoryPath,
) -> bool {
    if candidates.contains(&candidate) {
        return true;
    }
    if candidates.len() >= MAX_COMMAND_REPOSITORY_PATHS {
        analysis.blocks_session_fallback = true;
        push_analysis_abstention(
            analysis,
            RepositoryAbstentionReason::CandidateLimitExceeded,
            "command_repository_candidate_limit_exceeded",
        );
        return false;
    }
    candidates.push(candidate);
    true
}

fn opaque_segment_abstention(
    segment: &[String],
    wrapper_error: Option<&'static str>,
) -> (RepositoryAbstentionReason, &'static str) {
    let command = segment
        .iter()
        .position(|token| !is_assignment(token))
        .and_then(|index| segment.get(index..));
    if command.is_some_and(|command| {
        matches!(
            command.first().map(String::as_str),
            Some("bash" | "sh" | "zsh")
        ) && command
            .iter()
            .skip(1)
            .any(|token| matches!(token.as_str(), "-c" | "-lc"))
    }) || matches!(
        command
            .and_then(|command| command.first())
            .map(String::as_str),
        Some("source" | ".")
    ) {
        (
            RepositoryAbstentionReason::ProfileDependent,
            "profile_dependent_command_not_cwd_proof",
        )
    } else {
        (
            RepositoryAbstentionReason::UnknownWrapper,
            wrapper_error.unwrap_or("unknown_command_or_wrapper"),
        )
    }
}

fn strip_comments_and_bound_heredocs(
    command: &str,
) -> Result<(String, Option<&'static str>), &'static str> {
    let mut output = String::with_capacity(command.len());
    let mut terminal_abstention = None;
    let mut quote = None;
    let mut escaped = false;
    let mut previous_was_space = true;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            output.push(character);
            escaped = false;
            previous_was_space = character.is_whitespace();
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            output.push(character);
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            output.push(character);
            previous_was_space = false;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            output.push(character);
            previous_was_space = false;
            continue;
        }
        if character == '<' && characters.peek() == Some(&'<') {
            terminal_abstention = Some("heredoc_not_cwd_proof");
            break;
        }
        if character == '#' && previous_was_space {
            for trailing in characters.by_ref() {
                if trailing == '\n' {
                    output.push('\n');
                    break;
                }
            }
            previous_was_space = true;
            continue;
        }
        previous_was_space = character.is_whitespace();
        output.push(character);
    }
    if quote.is_some() || escaped {
        return Err("malformed_quoting");
    }
    Ok((output, terminal_abstention))
}

struct Tokenization {
    segments: Vec<Vec<String>>,
    terminal_abstention: Option<(RepositoryAbstentionReason, &'static str)>,
    blocks_session_fallback: bool,
}

fn tokenize(command: &str) -> Result<Tokenization, (RepositoryAbstentionReason, &'static str)> {
    let mut segments = Vec::new();
    let mut segment = Vec::new();
    let mut token = String::new();
    let mut token_count = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' => {
                return Ok(Tokenization {
                    blocks_session_fallback: incomplete_segment_blocks_session_fallback(
                        &segment, &token,
                    ),
                    segments,
                    terminal_abstention: Some((
                        RepositoryAbstentionReason::DynamicPath,
                        "dynamic_shell_token",
                    )),
                });
            }
            ';' | '|' | '(' | ')' | '<' | '>' | '\n' => {
                return Ok(Tokenization {
                    blocks_session_fallback: incomplete_segment_blocks_session_fallback(
                        &segment, &token,
                    ),
                    segments,
                    terminal_abstention: Some((
                        RepositoryAbstentionReason::UnsupportedShell,
                        "unsupported_shell_control_or_redirection",
                    )),
                });
            }
            '&' => {
                if characters.next() != Some('&') {
                    return Ok(Tokenization {
                        blocks_session_fallback: incomplete_segment_blocks_session_fallback(
                            &segment, &token,
                        ),
                        segments,
                        terminal_abstention: Some((
                            RepositoryAbstentionReason::UnsupportedShell,
                            "unsupported_shell_control_or_redirection",
                        )),
                    });
                }
                if !token.is_empty() {
                    segment.push(std::mem::take(&mut token));
                    token_count += 1;
                }
                if segment.is_empty() {
                    return Err((
                        RepositoryAbstentionReason::UnsupportedShell,
                        "malformed_and_chain",
                    ));
                }
                segments.push(std::mem::take(&mut segment));
                if segments.len() > MAX_SEGMENTS {
                    return Err((
                        RepositoryAbstentionReason::CommandTooLarge,
                        "command_segment_limit_exceeded",
                    ));
                }
            }
            value if value.is_whitespace() => {
                if !token.is_empty() {
                    segment.push(std::mem::take(&mut token));
                    token_count += 1;
                }
            }
            value => token.push(value),
        }
        if token_count > MAX_TOKENS || token.len() > MAX_PATH_BYTES {
            return Err((
                RepositoryAbstentionReason::CommandTooLarge,
                "command_token_limit_exceeded",
            ));
        }
    }
    if quote.is_some() || escaped {
        return Err((
            RepositoryAbstentionReason::UnsupportedShell,
            "malformed_quoting",
        ));
    }
    if !token.is_empty() {
        segment.push(token);
        token_count += 1;
    }
    if segment.is_empty() {
        return Err((
            RepositoryAbstentionReason::UnsupportedShell,
            "malformed_and_chain",
        ));
    }
    segments.push(segment);
    if segments.len() > MAX_SEGMENTS || token_count > MAX_TOKENS {
        return Err((
            RepositoryAbstentionReason::CommandTooLarge,
            "command_segment_limit_exceeded",
        ));
    }
    Ok(Tokenization {
        segments,
        terminal_abstention: None,
        blocks_session_fallback: false,
    })
}

fn incomplete_segment_blocks_session_fallback(segment: &[String], token: &str) -> bool {
    let mut values = segment.iter().map(String::as_str).collect::<Vec<_>>();
    if !token.is_empty() {
        values.push(token);
    }
    values.first() == Some(&"cd")
        || values
            .iter()
            .position(|value| *value == "git")
            .is_some_and(|index| {
                values.iter().skip(index + 1).any(|value| {
                    *value == "-C"
                        || value.starts_with("--git-dir")
                        || value.starts_with("--work-tree")
                })
            })
}

fn unwrap_wrappers(segment: &[String]) -> (Option<&[String]>, Option<&'static str>) {
    let (remaining, error) = unwrap_command_wrappers(segment);
    let Some(remaining) = remaining else {
        return (None, error);
    };
    if remaining.first().map(String::as_str) != Some("git") {
        return (None, Some("unknown_command_or_wrapper"));
    }
    (Some(remaining), None)
}

fn unwrap_command_wrappers(segment: &[String]) -> (Option<&[String]>, Option<&'static str>) {
    let mut cursor = 0;
    while segment
        .get(cursor)
        .is_some_and(|token| is_assignment(token))
    {
        if !safe_assignment(&segment[cursor]) {
            return (None, Some("unsafe_prefix_assignment"));
        }
        cursor += 1;
    }
    loop {
        match segment.get(cursor).map(String::as_str) {
            Some("env") => {
                cursor += 1;
                if segment.get(cursor).is_some_and(|token| token == "--") {
                    cursor += 1;
                }
                while segment
                    .get(cursor)
                    .is_some_and(|token| is_assignment(token))
                {
                    if !safe_assignment(&segment[cursor]) {
                        return (None, Some("unsafe_env_assignment"));
                    }
                    cursor += 1;
                }
                if segment
                    .get(cursor)
                    .is_some_and(|token| token.starts_with('-'))
                {
                    return (None, Some("unsupported_env_option"));
                }
            }
            Some("command") => {
                cursor += 1;
                if segment.get(cursor).is_some_and(|token| token == "--") {
                    cursor += 1;
                } else if segment
                    .get(cursor)
                    .is_some_and(|token| token.starts_with('-'))
                {
                    return (None, Some("unsupported_command_option"));
                }
            }
            Some("time") => {
                cursor += 1;
                if segment.get(cursor).is_some_and(|token| token == "-p") {
                    cursor += 1;
                }
                if segment.get(cursor).is_some_and(|token| token == "--") {
                    cursor += 1;
                } else if segment
                    .get(cursor)
                    .is_some_and(|token| token.starts_with('-'))
                {
                    return (None, Some("unsupported_time_option"));
                }
            }
            Some("timeout") => {
                cursor += 1;
                if segment.get(cursor).is_some_and(|token| token == "--") {
                    cursor += 1;
                }
                let Some(duration) = segment.get(cursor) else {
                    return (None, Some("timeout_missing_duration"));
                };
                if !literal_duration(duration) {
                    return (None, Some("unsupported_timeout_shape"));
                }
                cursor += 1;
            }
            _ => break,
        }
    }
    let remaining = segment.get(cursor..).unwrap_or_default();
    if remaining.is_empty() {
        return (None, Some("missing_wrapped_command"));
    }
    (Some(remaining), None)
}

fn is_assignment(token: &str) -> bool {
    token
        .split_once('=')
        .is_some_and(|(name, _)| valid_env_name(name))
}

fn safe_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    valid_env_name(name)
        && !matches!(
            name,
            "CDPATH"
                | "GIT_DIR"
                | "GIT_WORK_TREE"
                | "GIT_COMMON_DIR"
                | "GIT_CEILING_DIRECTORIES"
                | "GIT_DISCOVERY_ACROSS_FILESYSTEM"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_CONFIG_SYSTEM"
                | "HOME"
                | "OLDPWD"
                | "PATH"
                | "PWD"
        )
        && !value.as_bytes().contains(&0)
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn literal_duration(value: &str) -> bool {
    let (number, suffix) = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .map_or((value, ""), |index| value.split_at(index));
    let mut parts = number.split('.');
    let integral = parts.next().unwrap_or_default();
    let fractional = parts.next();
    !integral.is_empty()
        && integral.bytes().all(|byte| byte.is_ascii_digit())
        && fractional
            .is_none_or(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && parts.next().is_none()
        && matches!(suffix, "" | "s" | "m" | "h" | "d")
}

fn parse_git(
    argv: &[String],
    base: Option<&Path>,
) -> Result<(PathBuf, bool), (RepositoryAbstentionReason, &'static str)> {
    let mut directory = base.map(Path::to_path_buf);
    let mut has_git_c = false;
    let mut index = 1;
    while let Some(token) = argv.get(index) {
        if token == "-C" {
            let Some(path) = argv.get(index + 1) else {
                return Err((
                    RepositoryAbstentionReason::DynamicPath,
                    "git_c_missing_operand",
                ));
            };
            directory = lexical_absolute(path, directory.as_deref());
            if directory.is_none() {
                return Err((
                    RepositoryAbstentionReason::DynamicPath,
                    "git_c_dynamic_operand",
                ));
            }
            has_git_c = true;
            index += 2;
            continue;
        }
        if token == "--" {
            index += 1;
            break;
        }
        if token.starts_with('-') {
            return Err((
                RepositoryAbstentionReason::ConflictingIdentity,
                "unsupported_git_global_option",
            ));
        }
        break;
    }
    let Some(subcommand) = argv.get(index) else {
        return Err((
            RepositoryAbstentionReason::UnsupportedShell,
            "git_subcommand_missing",
        ));
    };
    if !known_git_builtin(subcommand) {
        return Err((
            RepositoryAbstentionReason::UnknownWrapper,
            "git_alias_or_unknown_subcommand",
        ));
    }
    directory.map(|directory| (directory, has_git_c)).ok_or((
        RepositoryAbstentionReason::UnsafePath,
        "git_has_no_absolute_base",
    ))
}

fn known_git_builtin(value: &str) -> bool {
    matches!(
        value,
        "add"
            | "am"
            | "apply"
            | "bisect"
            | "blame"
            | "branch"
            | "checkout"
            | "cherry-pick"
            | "clean"
            | "clone"
            | "commit"
            | "describe"
            | "diff"
            | "fetch"
            | "for-each-ref"
            | "grep"
            | "init"
            | "log"
            | "merge"
            | "mv"
            | "pull"
            | "push"
            | "rebase"
            | "remote"
            | "reset"
            | "restore"
            | "rev-list"
            | "rev-parse"
            | "rm"
            | "show"
            | "show-ref"
            | "sparse-checkout"
            | "stash"
            | "status"
            | "submodule"
            | "switch"
            | "tag"
            | "worktree"
    )
}

#[cfg(test)]
mod outcome_tests {
    use super::{bounded_outcome_operation, BoundedOutcomeOperation};

    #[test]
    fn outcome_recognition_is_bounded_and_alias_free() {
        assert_eq!(
            bounded_outcome_operation("git commit -m exact && git rev-parse --verify HEAD"),
            Some(BoundedOutcomeOperation::Commit {
                rewrites_history: false,
                exact_oid_output: true,
            })
        );
        assert!(bounded_outcome_operation("git ci -m alias").is_none());
        assert!(bounded_outcome_operation("git commit -m exact && echo $HEAD").is_none());
        assert!(bounded_outcome_operation("bash -lc 'git commit -m hidden'").is_none());
    }
}
