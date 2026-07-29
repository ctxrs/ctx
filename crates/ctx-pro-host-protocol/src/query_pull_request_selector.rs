#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PullRequestSelectorKind {
    Number,
    CanonicalUrl,
}

pub(super) fn pull_request_selector_kind(value: &str) -> Option<PullRequestSelectorKind> {
    if positive_decimal(value) {
        return Some(PullRequestSelectorKind::Number);
    }
    canonical_pull_request_url(value).then_some(PullRequestSelectorKind::CanonicalUrl)
}

fn positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
        && value.parse::<u64>().is_ok_and(|number| number > 0)
}

fn canonical_pull_request_url(value: &str) -> bool {
    let Some(remainder) = value.strip_prefix("https://") else {
        return false;
    };
    let mut components = remainder.split('/');
    let Some(host) = components.next() else {
        return false;
    };
    if !valid_forge_host(host) {
        return false;
    }
    let path = components.collect::<Vec<_>>();
    if path
        .iter()
        .any(|component| !valid_url_path_component(component))
    {
        return false;
    }

    if host == "github.com" {
        return path.len() == 4 && path[2] == "pull" && positive_decimal(path[3]);
    }
    if host == "codeberg.org" {
        return path.len() == 4 && path[2] == "pulls" && positive_decimal(path[3]);
    }
    if host == "bitbucket.org" {
        return false;
    }
    path.len() >= 5
        && path[path.len() - 3] == "-"
        && path[path.len() - 2] == "merge_requests"
        && positive_decimal(path[path.len() - 1])
}

fn valid_forge_host(host: &str) -> bool {
    !host.is_empty()
        && host.bytes().all(|byte| !byte.is_ascii_uppercase())
        && host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_url_path_component(component: &&str) -> bool {
    !component.is_empty()
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
        && !matches!(*component, "." | "..")
}
