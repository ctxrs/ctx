use ctx_history_core::{
    GitObjectId, RepositoryAlias, RepositoryAliasKind, RepositoryPullRequestIdentity,
};
use serde_json::{Map, Value};
use url::Url;

use super::{exact_json_object, keys_are_subset, object_id, BoundedOutcomePlan};
use crate::repository_attribution::identity::canonical_url_authority;

pub(super) fn exact_pr_create_result(output: &Value) -> Option<RepositoryPullRequestIdentity> {
    if let Some(url) = output.as_str() {
        return pull_request_from_url(url.trim());
    }
    pull_request_from_exact_object(exact_json_object(output)?)
}

pub(super) fn exact_pr_merge_result(
    output: &Value,
) -> Option<(RepositoryPullRequestIdentity, GitObjectId)> {
    let object = exact_json_object(output)?;
    let allowed = ["url", "number", "id", "node_id", "merge_commit_oid"];
    if !keys_are_subset(object, &allowed)
        || object.len() < 2
        || !object.contains_key("url")
        || !object.contains_key("merge_commit_oid")
    {
        return None;
    }
    let pull_request = pull_request_from_exact_object(object)?;
    let merge_oid = object_id(object.get("merge_commit_oid")?.as_str()?)?;
    Some((pull_request, merge_oid))
}

fn pull_request_from_exact_object(
    object: &Map<String, Value>,
) -> Option<RepositoryPullRequestIdentity> {
    let allowed = ["url", "number", "id", "node_id", "merge_commit_oid"];
    if !keys_are_subset(object, &allowed)
        || !object.contains_key("url")
        || (object.contains_key("id") && object.contains_key("node_id"))
    {
        return None;
    }
    let mut identity = pull_request_from_url(object.get("url")?.as_str()?)?;
    if let Some(number) = object.get("number") {
        if number.as_u64()? != identity.number {
            return None;
        }
    }
    identity.provider_id = match object.get("id").or_else(|| object.get("node_id")) {
        Some(value) => Some(value.as_str().filter(|value| !value.is_empty())?.to_owned()),
        None => None,
    };
    Some(identity)
}

fn pull_request_from_url(value: &str) -> Option<RepositoryPullRequestIdentity> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = canonical_url_authority(&url)?;
    let segments = url.path_segments()?.collect::<Vec<_>>();
    if segments.len() < 4 || segments[segments.len() - 2] != "pull" {
        return None;
    }
    let number = segments.last()?.parse::<u64>().ok()?;
    let name = segments.get(segments.len() - 3)?.to_string();
    let namespace = segments[..segments.len() - 3]
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect::<Vec<_>>();
    if number == 0 || namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(RepositoryPullRequestIdentity {
        forge_repository: RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host,
            namespace,
            name,
            remote_name: None,
        },
        number,
        provider_id: None,
    })
}

pub(super) fn pr_matches_plan(
    pull_request: &RepositoryPullRequestIdentity,
    plan: &BoundedOutcomePlan,
) -> bool {
    if plan
        .expected_pr_number
        .is_some_and(|number| number != pull_request.number)
    {
        return false;
    }
    if let Some(expected) = &plan.expected_pr_repository_path {
        let mut actual = pull_request.forge_repository.namespace.clone();
        actual.push(pull_request.forge_repository.name.clone());
        if &actual != expected {
            return false;
        }
    }
    true
}
