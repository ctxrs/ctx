use std::path::{Path, PathBuf};

use ctx_history_core::{RepositoryAlias, RepositoryAliasKind};
use url::Url;

use super::{strip_comments_and_bound_heredocs, tokenize, MAX_COMMAND_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedPullRequestAssociationQuery {
    pub(crate) repository_path: PathBuf,
    pub(crate) forge_repository: Option<RepositoryAlias>,
    pub(crate) number: u64,
    pub(crate) trailing_noise_allowed: bool,
    pub(crate) merged_at_requested: bool,
}

/// Exact source query for a forge-reported pull-request merge association.
/// A bounded newline tail may contain only literal Git fetch/log commands;
/// those commands authorize output noise but never contribute identity.
pub(crate) fn bounded_pull_request_association_query(
    command: &str,
    base: &Path,
) -> Option<BoundedPullRequestAssociationQuery> {
    if command.is_empty() || command.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let (command, terminal) = strip_comments_and_bound_heredocs(command).ok()?;
    if terminal.is_some() || command.contains('\r') {
        return None;
    }
    let lines = command.lines().collect::<Vec<_>>();
    if lines.is_empty() || lines.len() > 9 || lines.iter().any(|line| line.trim().is_empty()) {
        return None;
    }
    let mut segments = Vec::with_capacity(lines.len());
    for line in lines {
        let tokenization = tokenize(line).ok()?;
        if tokenization.terminal_abstention.is_some() {
            return None;
        }
        let [segment] = tokenization.segments.as_slice() else {
            return None;
        };
        segments.push(segment.clone());
    }
    if segments[1..]
        .iter()
        .any(|segment| !bounded_association_git_tail(segment))
    {
        return None;
    }
    let [gh, group, operation, arguments @ ..] = segments[0].as_slice() else {
        return None;
    };
    if gh != "gh" || group != "pr" || operation != "view" {
        return None;
    }
    let mut target = None;
    let mut forge_repository = None;
    let mut requested_fields = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let repository = if matches!(argument.as_str(), "--repo" | "-R") {
            index += 1;
            Some(arguments.get(index)?.as_str())
        } else {
            argument.strip_prefix("--repo=")
        };
        if let Some(repository) = repository {
            if forge_repository
                .replace(parse_forge_repository(repository)?)
                .is_some()
            {
                return None;
            }
        } else {
            let fields = if argument == "--json" {
                index += 1;
                Some(arguments.get(index)?.as_str())
            } else {
                argument.strip_prefix("--json=")
            };
            if let Some(fields) = fields {
                if requested_fields.replace(fields).is_some() {
                    return None;
                }
            } else if argument.starts_with('-') || target.replace(argument.as_str()).is_some() {
                return None;
            }
        }
        index += 1;
    }
    let mut fields = requested_fields?.split(',').collect::<Vec<_>>();
    fields.sort_unstable();
    if fields.windows(2).any(|pair| pair[0] == pair[1])
        || !matches!(
            fields.as_slice(),
            ["mergeCommit", "state", "url"] | ["mergeCommit", "mergedAt", "state", "url"]
        )
    {
        return None;
    }
    let (number, target_repository) = exact_pull_request_target(target?)?;
    if forge_repository
        .as_ref()
        .zip(target_repository.as_ref())
        .is_some_and(|(expected, target)| !alias_identity_matches(expected, target))
    {
        return None;
    }
    Some(BoundedPullRequestAssociationQuery {
        repository_path: base.to_path_buf(),
        forge_repository: forge_repository.or(target_repository),
        number,
        trailing_noise_allowed: segments.len() > 1,
        merged_at_requested: fields.contains(&"mergedAt"),
    })
}

fn bounded_association_git_tail(segment: &[String]) -> bool {
    match segment {
        [git, fetch, remote, reference]
            if git == "git"
                && fetch == "fetch"
                && safe_association_git_atom(remote)
                && safe_association_git_atom(reference) =>
        {
            true
        }
        [git, log, count, oneline, revision]
            if git == "git"
                && log == "log"
                && oneline == "--oneline"
                && count
                    .strip_prefix('-')
                    .and_then(|value| value.parse::<u16>().ok())
                    .is_some_and(|value| (1..=64).contains(&value))
                && safe_association_git_atom(revision) =>
        {
            true
        }
        _ => false,
    }
}

fn safe_association_git_atom(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.contains("..")
        && !value.contains("@{")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
}

fn exact_pull_request_target(value: &str) -> Option<(u64, Option<RepositoryAlias>)> {
    if let Ok(number) = value.parse::<u64>() {
        return (number > 0 && value == number.to_string()).then_some((number, None));
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let segments = url.path_segments()?.collect::<Vec<_>>();
    if segments.len() < 4 || segments[segments.len() - 2] != "pull" {
        return None;
    }
    let number = segments.last()?.parse::<u64>().ok()?;
    let repository_path = segments[..segments.len() - 2].join("/");
    let repository = parse_forge_repository(&format!("{host}/{repository_path}"))?;
    let canonical = format!("https://{host}/{repository_path}/pull/{number}");
    (number > 0 && canonical == value).then_some((number, Some(repository)))
}

fn parse_forge_repository(value: &str) -> Option<RepositoryAlias> {
    if value.contains("//")
        || value.bytes().any(|byte| {
            byte.is_ascii_control() || matches!(byte, b'@' | b':' | b'\\' | b'?' | b'#')
        })
    {
        return None;
    }
    let parts = value.split('/').collect::<Vec<_>>();
    if parts.len() < 2
        || parts
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | ".."))
    {
        return None;
    }
    let (host, path) = if parts.len() == 2 {
        ("github.com", parts.as_slice())
    } else {
        (parts[0], &parts[1..])
    };
    let (name, namespace) = path.split_last()?;
    Some(RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host: host.to_ascii_lowercase(),
        namespace: namespace.iter().map(|part| (*part).to_owned()).collect(),
        name: (*name).to_owned(),
        remote_name: None,
    })
}

fn alias_identity_matches(left: &RepositoryAlias, right: &RepositoryAlias) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.namespace == right.namespace
        && left.name == right.name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_number_requires_canonical_positive_decimal_spelling() {
        assert_eq!(
            exact_pull_request_target("203").map(|target| target.0),
            Some(203)
        );
        for rejected in ["0", "00", "0203", "+203", " 203", "203 "] {
            assert!(
                exact_pull_request_target(rejected).is_none(),
                "admitted {rejected:?}"
            );
        }
    }
}
