use std::path::{Path, PathBuf};

use ctx_history_core::RepositoryAbstentionReason;

use super::{
    known_git_builtin, lexical_absolute, literal_cd_destination, strip_comments_and_bound_heredocs,
    tokenize, unwrap_command_wrappers, MAX_COMMAND_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) enum BoundedCommitProducer {
    Commit,
    Merge,
    Rebase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundedOutcomeOperation {
    Commit {
        producer: BoundedCommitProducer,
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
    pub(crate) machine_output_isolated: bool,
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
pub(super) fn bounded_outcome_operation(command: &str) -> Option<BoundedOutcomeOperation> {
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

    let unconditional_separators = tokenization.unconditional_separators_after_segments;
    let mut current = Some(base.to_path_buf());
    let mut plan = None;
    let mut operation_segment_index = None;
    let mut prior_git_segment = false;
    for (segment_index, segment) in tokenization.segments.into_iter().enumerate() {
        if segment.first().is_some_and(|token| token == "cd") {
            let destination = match segment.as_slice() {
                [_, path] => literal_cd_destination(path, current.as_deref()),
                [_, option, path] if option == "--" => {
                    literal_cd_destination(path, current.as_deref())
                }
                _ => None,
            };
            let Some(destination) = destination else {
                return outcome_abstained_with_plan(
                    RepositoryAbstentionReason::DynamicPath,
                    "outcome_cd_is_not_a_bounded_literal",
                    plan,
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
                return outcome_abstained_with_plan(
                    RepositoryAbstentionReason::UnknownWrapper,
                    "outcome_wrapper_or_assignment_is_unattested",
                    plan,
                );
            }
            if plan.as_ref().is_some_and(|plan| {
                matches!(
                    plan.operation,
                    BoundedOutcomeOperation::Commit {
                        producer: BoundedCommitProducer::Commit,
                        rewrites_history: false,
                        exact_oid_output: true,
                    }
                )
            }) {
                // A later, unrelated command cannot revoke a commit that the
                // same bounded route has already observed exactly. The result
                // parser still requires one canonical commit receipt and the
                // certifier resolves that receipt against this repository;
                // multiple or conflicting receipts continue to fail closed.
                continue;
            }
            if plan.is_some() {
                return outcome_abstained_with_plan(
                    RepositoryAbstentionReason::Ambiguous,
                    "ambiguous_command_between_outcome_operation_and_result",
                    plan,
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
                    "commit" | "rebase" | "merge" => {
                        if subcommand == "merge"
                            && !arguments.iter().any(|argument| argument == "--no-ff")
                        {
                            return BoundedOutcomePlanDisposition::Unrecognized;
                        }
                        let producer = match subcommand {
                            "commit" => BoundedCommitProducer::Commit,
                            "merge" => BoundedCommitProducer::Merge,
                            "rebase" => BoundedCommitProducer::Rebase,
                            _ => return BoundedOutcomePlanDisposition::Unrecognized,
                        };
                        let candidate = BoundedOutcomePlan {
                            operation: BoundedOutcomeOperation::Commit {
                                producer,
                                rewrites_history: subcommand == "rebase"
                                    || arguments.iter().any(|argument| {
                                        argument == "--amend" || argument.starts_with("--amend=")
                                    }),
                                exact_oid_output: false,
                            },
                            operation_repository_path: repository_path,
                            output_repository_path: None,
                            machine_output_isolated: !prior_git_segment,
                            expected_pr_repository_path: None,
                            expected_pr_number: None,
                        };
                        if producer_is_non_producing_mode(producer, arguments) {
                            return outcome_abstained_with_plan(
                                RepositoryAbstentionReason::OutcomeResultInadmissible,
                                "outcome_operation_is_non_producing_mode",
                                Some(candidate),
                            );
                        }
                        if plan.is_some() {
                            return outcome_abstained_with_plan(
                                RepositoryAbstentionReason::Ambiguous,
                                "multiple_outcome_operations",
                                plan,
                            );
                        }
                        operation_segment_index = Some(segment_index);
                        plan = Some(candidate);
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
                    subcommand if known_git_builtin(subcommand) => {
                        let awaiting_observation = matches!(
                            plan.as_ref().map(|plan| plan.operation),
                            Some(BoundedOutcomeOperation::Commit {
                                exact_oid_output: false,
                                ..
                            })
                        );
                        if awaiting_observation && !head_stable_git_builtin(subcommand) {
                            return outcome_abstained_with_plan(
                                RepositoryAbstentionReason::Ambiguous,
                                "head_changing_or_ambiguous_command_before_exact_oid",
                                plan,
                            );
                        }
                        if awaiting_observation {
                            if let Some(plan) = plan.as_mut() {
                                plan.machine_output_isolated = false;
                            }
                        } else if plan.is_none() {
                            prior_git_segment = true;
                        }
                    }
                    _ if plan.is_some() => {
                        return outcome_abstained_with_plan(
                            RepositoryAbstentionReason::Ambiguous,
                            "git_alias_or_unknown_command_before_exact_oid",
                            plan,
                        );
                    }
                    _ => return BoundedOutcomePlanDisposition::Unrecognized,
                }
            }
            Some("gh") => {
                let Some((operation, expected_pr_repository_path, expected_pr_number)) =
                    bounded_gh_operation(&segment)
                else {
                    if plan.is_some() {
                        return outcome_abstained_with_plan(
                            RepositoryAbstentionReason::Ambiguous,
                            "ambiguous_gh_command_between_outcome_operation_and_result",
                            plan,
                        );
                    }
                    return BoundedOutcomePlanDisposition::Unrecognized;
                };
                if plan.is_some() {
                    return outcome_abstained_with_plan(
                        RepositoryAbstentionReason::Ambiguous,
                        "multiple_outcome_operations",
                        plan,
                    );
                }
                let Some(repository_path) = current.clone() else {
                    return outcome_abstained(
                        RepositoryAbstentionReason::UnsafePath,
                        "gh_outcome_has_no_bounded_workdir",
                    );
                };
                operation_segment_index = Some(segment_index);
                plan = Some(BoundedOutcomePlan {
                    operation,
                    operation_repository_path: repository_path,
                    output_repository_path: None,
                    machine_output_isolated: true,
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
    if operation_segment_index.is_some_and(|operation| {
        unconditional_separators
            .iter()
            .any(|separator| *separator >= operation)
    }) {
        return outcome_abstained_with_plan(
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "outcome_operation_is_not_terminal_across_unconditional_separator",
            Some(plan),
        );
    }
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

fn outcome_abstained_with_plan(
    reason: RepositoryAbstentionReason,
    detail: &'static str,
    plan: Option<BoundedOutcomePlan>,
) -> BoundedOutcomePlanDisposition {
    BoundedOutcomePlanDisposition::Abstained {
        reason,
        detail,
        plan: plan.map(Box::new),
    }
}

fn producer_is_non_producing_mode(producer: BoundedCommitProducer, arguments: &[String]) -> bool {
    let rejected = match producer {
        BoundedCommitProducer::Commit => &[
            "--dry-run",
            "--short",
            "--branch",
            "--porcelain",
            "--long",
            "-z",
        ][..],
        BoundedCommitProducer::Merge => &["--no-commit", "--squash", "--abort", "--quit"][..],
        BoundedCommitProducer::Rebase => {
            &["--abort", "--quit", "--edit-todo", "--show-current-patch"][..]
        }
    };
    rejected
        .iter()
        .any(|option| outcome_option_present(arguments, option))
}

fn outcome_option_present(arguments: &[String], option: &str) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| {
            argument == option
                || argument
                    .strip_prefix(option)
                    .is_some_and(|suffix| suffix.starts_with('='))
        })
}

fn head_stable_git_builtin(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "blame"
            | "describe"
            | "diff"
            | "for-each-ref"
            | "grep"
            | "log"
            | "rev-list"
            | "rev-parse"
            | "show"
            | "show-ref"
            | "status"
    )
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
    if arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| {
            matches!(argument.as_str(), "--help" | "-h")
                || argument.starts_with("--help=")
                || argument.starts_with("-h=")
                || (operation == BoundedOutcomeOperation::PullRequestCreate
                    && (matches!(argument.as_str(), "--dry-run" | "--web" | "-w")
                        || argument.starts_with("--dry-run=")
                        || argument.starts_with("--web=")
                        || argument.starts_with("-w=")))
        })
    {
        return None;
    }
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
