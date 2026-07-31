use std::path::{Component, Path, PathBuf};

use ctx_history_core::{RepositoryAbstentionReason, RepositoryEvidenceKind};

pub(super) const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_TOKENS: usize = 16_384;
const MAX_SEGMENTS: usize = 4_096;
const MAX_PATH_BYTES: usize = 16 * 1024;

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

pub(super) fn lexical_absolute(value: &str, base: Option<&Path>) -> Option<PathBuf> {
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
            preserve_derived_candidate(&analysis, &mut command_candidates);
            let (reason, detail) = opaque_segment_abstention(&segment, wrapper_error);
            push_analysis_abstention(&mut analysis, reason, detail);
            cut_off = true;
            break;
        };
        match parse_git(git, current.as_deref()) {
            Ok((path, has_git_c)) => {
                if has_git_c {
                    command_candidates.push(CommandRepositoryPath {
                        path,
                        evidence_kind: RepositoryEvidenceKind::CommandSpecificRepositoryPath,
                    });
                } else if analysis.derived_effective_cwd.is_some() {
                    command_candidates.push(CommandRepositoryPath {
                        path,
                        evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
                    });
                }
            }
            Err((reason, detail)) => {
                analysis.blocks_session_fallback |= matches!(
                    reason,
                    RepositoryAbstentionReason::DynamicPath
                        | RepositoryAbstentionReason::ConflictingIdentity
                );
                preserve_derived_candidate(&analysis, &mut command_candidates);
                push_analysis_abstention(&mut analysis, reason, detail);
                cut_off = true;
                break;
            }
        }
    }
    if !cut_off {
        if let Some((reason, detail)) = tokenization.terminal_abstention {
            analysis.blocks_session_fallback |= analysis.derived_effective_cwd.is_some();
            preserve_derived_candidate(&analysis, &mut command_candidates);
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
    analysis: &CommandAnalysis,
    candidates: &mut Vec<CommandRepositoryPath>,
) {
    if let Some(path) = &analysis.derived_effective_cwd {
        candidates.push(CommandRepositoryPath {
            path: path.clone(),
            evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
        });
    }
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
    if remaining.first().map(String::as_str) != Some("git") {
        return (None, Some("unknown_command_or_wrapper"));
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
