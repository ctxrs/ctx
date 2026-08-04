use chrono::DateTime;
use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAlias, RepositoryAliasKind, RepositoryOutcomeLinkage,
    RepositoryPullRequestIdentity,
};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::{collections::HashSet, fmt};
use url::{Host, Url};

use super::shell::bounded_pull_request_association_query;

const MAX_ASSOCIATION_OUTPUT_BYTES: usize = 1024 * 1024;

/// Credential-free canonical authority for a forge URL.
///
/// Default transport ports are aliases of the ordinary host spelling while a
/// nondefault port is part of repository authority. IPv6 literals retain
/// brackets so a following port remains unambiguous.
pub(super) fn canonical_url_authority(url: &Url) -> Option<String> {
    let host = match url.host()? {
        Host::Domain(host) => host.to_ascii_lowercase(),
        Host::Ipv4(host) => host.to_string(),
        Host::Ipv6(host) => format!("[{host}]"),
    };
    let port = url
        .port()
        .filter(|port| Some(*port) != default_port(url.scheme()));
    Some(port.map_or(host.clone(), |port| format!("{host}:{port}")))
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        "ssh" => Some(22),
        "git" => Some(9418),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnscopedPullRequestAssociationObservation {
    pub(crate) repository_path: String,
    pub(crate) pull_request: RepositoryPullRequestIdentity,
    pub(crate) merged_as: GitObjectId,
    pub(crate) linkage: RepositoryOutcomeLinkage,
}

pub(crate) fn exact_pull_request_association(
    command: &str,
    declared_workdir: &str,
    output: &Value,
    linkage: RepositoryOutcomeLinkage,
) -> Option<UnscopedPullRequestAssociationObservation> {
    let repository_path = super::shell::lexical_absolute(declared_workdir, None)?;
    let query = bounded_pull_request_association_query(command, &repository_path)?;
    let output = output.as_str()?;
    if output.is_empty()
        || output.len() > MAX_ASSOCIATION_OUTPUT_BYTES
        || output.contains('\0')
        || output.lines().any(inadmissible_result_line)
    {
        return None;
    }
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() > 512 {
        return None;
    }
    let (json_line, trailing) = lines.split_first()?;
    if query.trailing_noise_allowed != !trailing.is_empty()
        || trailing.iter().any(|line| {
            let line = line.trim_start();
            line.starts_with('{') || line.starts_with('[')
        })
    {
        return None;
    }
    let value = exact_json_value(json_line)?;
    let object = value.as_object()?;
    let expected = if query.merged_at_requested {
        &["state", "mergedAt", "mergeCommit", "url"][..]
    } else {
        &["state", "mergeCommit", "url"][..]
    };
    if object.len() != expected.len()
        || !expected.iter().all(|field| object.contains_key(*field))
        || object.get("state")?.as_str()? != "MERGED"
    {
        return None;
    }
    if let Some(merged_at) = object.get("mergedAt") {
        DateTime::parse_from_rfc3339(merged_at.as_str()?).ok()?;
    }
    let merge = object.get("mergeCommit")?.as_object()?;
    if merge.len() != 1 || !merge.contains_key("oid") {
        return None;
    }
    let merged_as = full_object_id(merge.get("oid")?.as_str()?)?;
    let pull_request = canonical_pull_request(object.get("url")?.as_str()?)?;
    if pull_request.number != query.number
        || query.forge_repository.as_ref().is_some_and(|expected| {
            !alias_identity_matches(expected, &pull_request.forge_repository)
        })
    {
        return None;
    }
    Some(UnscopedPullRequestAssociationObservation {
        repository_path: query.repository_path.to_string_lossy().into_owned(),
        pull_request,
        merged_as,
        linkage,
    })
}

/// Parse one JSON value while rejecting duplicate keys at every object depth.
/// `serde_json::Value` alone retains only the final duplicate and therefore is
/// not an exact schema authority.
fn exact_json_value(input: &str) -> Option<Value> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let value = NoDuplicateJson::deserialize(&mut deserializer).ok()?.0;
    deserializer.end().ok()?;
    Some(value)
}

struct NoDuplicateJson(Value);

impl<'de> Deserialize<'de> for NoDuplicateJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateJsonVisitor)
    }
}

struct NoDuplicateJsonVisitor;

impl<'de> Visitor<'de> for NoDuplicateJsonVisitor {
    type Value = NoDuplicateJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(NoDuplicateJson)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<NoDuplicateJson>()? {
            values.push(value.0);
        }
        Ok(NoDuplicateJson(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            let value = object.next_value::<NoDuplicateJson>()?;
            values.insert(key, value.0);
        }
        Ok(NoDuplicateJson(Value::Object(values)))
    }
}

fn inadmissible_result_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("Warning: truncated output (original token count: ")
        || line.starts_with("Warning: truncated output (original char count: ")
        || (line.starts_with("[omitted ") && line.ends_with(" text items ...]"))
        || (line.starts_with('…') && line.ends_with(" tokens truncated…"))
        || line == "Script failed"
        || line
            .strip_prefix("Process exited with code ")
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code != 0)
}

fn full_object_id(value: &str) -> Option<GitObjectId> {
    let format = match value.len() {
        40 => GitObjectFormat::Sha1,
        64 => GitObjectFormat::Sha256,
        _ => return None,
    };
    let object_id = GitObjectId {
        format,
        hex: value.to_owned(),
    };
    object_id.validate_contract().ok()?;
    Some(object_id)
}

fn canonical_pull_request(value: &str) -> Option<RepositoryPullRequestIdentity> {
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
    if segments.len() < 4
        || segments[segments.len() - 2] != "pull"
        || segments[..segments.len() - 2]
            .iter()
            .any(|component| !safe_forge_component(component))
    {
        return None;
    }
    let number = segments.last()?.parse::<u64>().ok()?;
    let name = segments.get(segments.len() - 3)?.to_string();
    let namespace = segments[..segments.len() - 3]
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect::<Vec<_>>();
    let canonical = format!(
        "https://{host}/{}/{name}/pull/{number}",
        namespace.join("/")
    );
    if value != canonical || number == 0 || namespace.is_empty() || name.is_empty() {
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

fn safe_forge_component(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn alias_identity_matches(left: &RepositoryAlias, right: &RepositoryAlias) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.namespace == right.namespace
        && left.name == right.name
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn linkage() -> RepositoryOutcomeLinkage {
        RepositoryOutcomeLinkage {
            provider: "codex".to_owned(),
            origin_call_id: "call-origin".to_owned(),
            result_call_id: "call-result".to_owned(),
            origin_event_sequence: 7,
            continuation_call_id_sha256: Vec::new(),
            result_record_sha256: [7; 32],
        }
    }

    fn exact_output() -> String {
        json!({
            "mergeCommit": {"oid": "103c0105645cc02c730f98eba2831fba854d3569"},
            "mergedAt": "2026-07-29T17:11:30Z",
            "state": "MERGED",
            "url": "https://github.com/ctxrs/ctx/pull/203"
        })
        .to_string()
    }

    #[test]
    fn exact_merged_pull_request_yields_only_source_authoritative_identity() {
        let association = exact_pull_request_association(
            "gh pr view 203 --repo ctxrs/ctx --json state,mergedAt,mergeCommit,url",
            "/repo",
            &Value::String(exact_output()),
            linkage(),
        )
        .expect("exact association");
        assert_eq!(association.pull_request.number, 203);
        assert_eq!(
            association.merged_as.hex,
            "103c0105645cc02c730f98eba2831fba854d3569"
        );
    }

    #[test]
    fn custom_port_is_part_of_pull_request_repository_authority() {
        let output = json!({
            "mergeCommit": {"oid": "103c0105645cc02c730f98eba2831fba854d3569"},
            "mergedAt": "2026-07-29T17:11:30Z",
            "state": "MERGED",
            "url": "https://forge.example.test:8443/acme/repo/pull/203"
        })
        .to_string();
        let association = exact_pull_request_association(
            "gh pr view 203 --repo forge.example.test:8443/acme/repo --json state,mergedAt,mergeCommit,url",
            "/repo",
            &Value::String(output),
            linkage(),
        )
        .expect("custom-port association");
        assert_eq!(
            association.pull_request.forge_repository.host,
            "forge.example.test:8443"
        );
    }

    #[test]
    fn wrappers_substitutions_conflicts_and_ambiguous_output_are_rejected() {
        let exact = exact_output();
        for (command, output) in [
            (
                "env gh pr view 203 --json state,mergedAt,mergeCommit,url",
                exact.clone(),
            ),
            (
                "gh pr view $PR --json state,mergedAt,mergeCommit,url",
                exact.clone(),
            ),
            (
                "gh pr view 203 --json state,mergedAt,mergeCommit,url --jq .",
                exact.clone(),
            ),
            (
                "gh pr view 204 --json state,mergedAt,mergeCommit,url",
                exact.clone(),
            ),
            (
                "gh pr view 203 --json state,mergedAt,mergeCommit,url",
                exact.replace("/203\"", "/204\""),
            ),
            (
                "gh pr view 203 --json state,mergedAt,mergeCommit,url",
                format!("{exact}\n{exact}"),
            ),
            (
                "gh pr view 203 --json state,mergedAt,mergeCommit,url\ngit fetch origin pull/203/head",
                format!("{exact}\n{{\n  \"state\": \"MERGED\"\n}}"),
            ),
            (
                "gh pr view 203 --json state,mergedAt,mergeCommit,url",
                r#"{"state":"OPEN","state":"MERGED","mergedAt":"2026-07-29T17:11:30Z","mergeCommit":{"oid":"103c0105645cc02d730f98eba2831fba854d3569"},"url":"https://github.com/ctxrs/ctx/pull/203"}"#.to_owned(),
            ),
            (
                "gh pr view 203 --json state,mergedAt,mergeCommit,url",
                r#"{"state":"MERGED","mergedAt":"2026-07-29T17:11:30Z","mergeCommit":{"oid":"0000000000000000000000000000000000000000","oid":"103c0105645cc02d730f98eba2831fba854d3569"},"url":"https://github.com/ctxrs/ctx/pull/203"}"#.to_owned(),
            ),
        ] {
            assert!(
                exact_pull_request_association(
                    command,
                    "/repo",
                    &Value::String(output),
                    linkage(),
                )
                .is_none(),
                "admitted {command:?}",
            );
        }
    }

    #[test]
    fn bounded_literal_multiline_tail_is_accepted_but_never_supplies_identity() {
        let command = "gh pr view https://github.com/ctxrs/ctx/pull/203 --json url,state,mergeCommit\ngit fetch origin pull/203/head\ngit log -20 --oneline FETCH_HEAD";
        let output = format!(
            "{}\nFrom github.com:ctxrs/ctx\n1234567 subject",
            json!({
                "mergeCommit": {"oid": "103c0105645cc02c730f98eba2831fba854d3569"},
                "state": "MERGED",
                "url": "https://github.com/ctxrs/ctx/pull/203"
            })
        );
        assert!(exact_pull_request_association(
            command,
            "/repo",
            &Value::String(output),
            linkage(),
        )
        .is_some());
        assert!(exact_pull_request_association(
            &command.replace(
                "git fetch origin pull/203/head",
                "git fetch origin $(cat ref)"
            ),
            "/repo",
            &Value::String(exact_output()),
            linkage(),
        )
        .is_none());
    }
}
