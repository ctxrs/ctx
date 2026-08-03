use std::{collections::HashSet, ffi::OsStr, fs, io::Read, path::Path};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAlias, RepositoryAliasKind,
    RepositoryFileObservationKind,
};
use sha2::{Digest, Sha256};
use url::Url;

use super::{
    ProbeFailure, ResolvedCommitFile, MAX_GIT_OUTPUT_BYTES, MAX_REMOTES, MAX_RESOLVED_COMMIT_FILES,
};

pub(super) fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

pub(super) fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, ProbeFailure> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut exceeded = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| ProbeFailure::Failed("git_output_read_failed"))?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    if exceeded {
        Err(ProbeFailure::Failed("git_output_limit_exceeded"))
    } else {
        Ok(output)
    }
}

pub(super) fn utf8_lines(value: &[u8]) -> Result<Vec<&str>, ProbeFailure> {
    std::str::from_utf8(value)
        .map_err(|_| ProbeFailure::Unsafe("git_output_is_not_unicode"))
        .map(|value| value.lines().collect())
}

fn parse_optional_line(value: &[u8]) -> Result<Option<String>, ProbeFailure> {
    let lines = utf8_lines(value)?;
    match lines.as_slice() {
        [] => Ok(None),
        [line] if !line.is_empty() => Ok(Some((*line).to_owned())),
        _ => Err(ProbeFailure::Failed("unexpected_git_scalar")),
    }
}

fn parse_optional_oid(
    value: &[u8],
    format: GitObjectFormat,
) -> Result<Option<GitObjectId>, ProbeFailure> {
    let Some(hex) = parse_optional_line(value)? else {
        return Ok(None);
    };
    let object = GitObjectId { format, hex };
    object
        .validate_contract()
        .map_err(|_| ProbeFailure::Failed("invalid_git_head"))?;
    Ok(Some(object))
}

pub(super) fn parse_resolved_commit_metadata(
    value: &[u8],
    format: GitObjectFormat,
) -> Result<(GitObjectId, Vec<GitObjectId>, String), ProbeFailure> {
    let value = std::str::from_utf8(value)
        .map_err(|_| ProbeFailure::Unsafe("git_commit_metadata_is_not_unicode"))?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    let fields = value.split('\0').collect::<Vec<_>>();
    let [object, parents, subject] = fields.as_slice() else {
        return Err(ProbeFailure::Failed("unexpected_git_commit_metadata"));
    };
    if subject.is_empty() || subject.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ProbeFailure::Unsafe("git_commit_subject_is_not_bounded"));
    }
    let object_id = parse_full_object_id(object, format)?;
    let parent_object_ids = if parents.is_empty() {
        Vec::new()
    } else {
        parents
            .split(' ')
            .map(|parent| parse_full_object_id(parent, format))
            .collect::<Result<Vec<_>, _>>()?
    };
    if parent_object_ids.len() > 64
        || parent_object_ids.iter().collect::<HashSet<_>>().len() != parent_object_ids.len()
    {
        return Err(ProbeFailure::Failed("invalid_git_commit_parents"));
    }
    Ok((object_id, parent_object_ids, (*subject).to_owned()))
}

pub(super) fn parse_exact_merge_metadata(
    value: &[u8],
    format: GitObjectFormat,
) -> Result<(GitObjectId, [GitObjectId; 2]), ProbeFailure> {
    let value = std::str::from_utf8(value)
        .map_err(|_| ProbeFailure::Unsafe("git_merge_metadata_is_not_unicode"))?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    let fields = value.split('\0').collect::<Vec<_>>();
    let [object, parents] = fields.as_slice() else {
        return Err(ProbeFailure::Failed("unexpected_git_merge_metadata"));
    };
    let object_id = parse_full_object_id(object, format)?;
    let parents = parents
        .split(' ')
        .map(|parent| parse_full_object_id(parent, format))
        .collect::<Result<Vec<_>, _>>()?;
    let [first, second] = parents.as_slice() else {
        return Err(ProbeFailure::Failed(
            "pull_request_merge_has_invalid_parent_topology",
        ));
    };
    if first == second {
        return Err(ProbeFailure::Failed(
            "pull_request_merge_has_invalid_parent_topology",
        ));
    }
    Ok((object_id, [first.clone(), second.clone()]))
}

fn parse_full_object_id(value: &str, format: GitObjectFormat) -> Result<GitObjectId, ProbeFailure> {
    let object = GitObjectId {
        format,
        hex: value.to_ascii_lowercase(),
    };
    object
        .validate_contract()
        .map_err(|_| ProbeFailure::Failed("invalid_git_object_id"))?;
    Ok(object)
}

pub(super) fn parse_resolved_commit_files(
    value: &[u8],
) -> Result<Vec<ResolvedCommitFile>, ProbeFailure> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.last() != Some(&0) {
        return Err(ProbeFailure::Failed("unterminated_git_name_status"));
    }
    let fields = value[..value.len() - 1]
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut index = 0;
    while index < fields.len() {
        let status = std::str::from_utf8(fields[index])
            .map_err(|_| ProbeFailure::Unsafe("git_name_status_is_not_unicode"))?;
        index += 1;
        let (kind, prior_path) = match status {
            "A" => (RepositoryFileObservationKind::Created, None),
            "D" => (RepositoryFileObservationKind::Deleted, None),
            "M" | "T" => (RepositoryFileObservationKind::Modified, None),
            "R100" => {
                let prior = fields
                    .get(index)
                    .ok_or(ProbeFailure::Failed("truncated_git_rename_status"))?;
                index += 1;
                (
                    RepositoryFileObservationKind::Renamed,
                    Some(parse_git_relative_path(prior)?),
                )
            }
            _ => return Err(ProbeFailure::Failed("unsupported_git_name_status")),
        };
        let path = fields
            .get(index)
            .ok_or(ProbeFailure::Failed("truncated_git_name_status"))?;
        index += 1;
        let path = parse_git_relative_path(path)?;
        if !seen.insert(path.clone()) || files.len() >= MAX_RESOLVED_COMMIT_FILES {
            return Err(ProbeFailure::Failed("git_commit_file_limit_or_duplicate"));
        }
        files.push(ResolvedCommitFile {
            path,
            prior_path,
            kind,
        });
    }
    Ok(files)
}

fn parse_git_relative_path(value: &[u8]) -> Result<String, ProbeFailure> {
    let value =
        std::str::from_utf8(value).map_err(|_| ProbeFailure::Unsafe("git_path_is_not_unicode"))?;
    let path = Path::new(value);
    if value.is_empty()
        || value.bytes().any(|byte| byte.is_ascii_control())
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ProbeFailure::Unsafe("git_path_is_not_bounded_relative"));
    }
    Ok(value.replace('\\', "/"))
}

pub(super) fn repository_head_branch(
    git_dir: &Path,
    format: GitObjectFormat,
) -> Result<Option<String>, ProbeFailure> {
    let head_path = git_dir.join("HEAD");
    let metadata = fs::symlink_metadata(&head_path)
        .map_err(|_| ProbeFailure::Failed("git_head_metadata_failed"))?;
    if metadata_is_link_like(&metadata) || !metadata.is_file() {
        return Err(ProbeFailure::Unsafe("git_head_is_not_regular_file"));
    }
    if metadata.len() > MAX_GIT_OUTPUT_BYTES as u64 {
        return Err(ProbeFailure::Failed("git_head_limit_exceeded"));
    }
    let value = fs::read(&head_path).map_err(|_| ProbeFailure::Failed("git_head_read_failed"))?;
    let line = parse_optional_line(&value)?.ok_or(ProbeFailure::Failed("git_head_is_empty"))?;
    if let Some(branch) = line.strip_prefix("ref: ") {
        if !canonical_symbolic_branch(branch) {
            return Err(ProbeFailure::Unsafe("git_branch_is_not_canonical"));
        }
        return Ok(Some(branch.to_owned()));
    }
    if parse_optional_oid(&value, format)?.is_some() {
        Ok(None)
    } else {
        Err(ProbeFailure::Failed("invalid_git_head"))
    }
}

pub(super) fn canonical_symbolic_branch(branch: &str) -> bool {
    branch.starts_with("refs/")
        && !branch
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        && !branch
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
}

pub(super) fn parse_aliases(value: &[u8]) -> Result<Vec<RepositoryAlias>, ProbeFailure> {
    let text = std::str::from_utf8(value)
        .map_err(|_| ProbeFailure::Unsafe("remote_output_is_not_unicode"))?;
    let mut aliases = Vec::new();
    for line in text.lines() {
        let Some((key, remote)) = line.split_once(char::is_whitespace) else {
            return Err(ProbeFailure::Failed("malformed_remote_config"));
        };
        let remote_name = key
            .strip_prefix("remote.")
            .and_then(|value| value.strip_suffix(".url"))
            .ok_or(ProbeFailure::Failed("malformed_remote_key"))?;
        if let Some(mut alias) = normalize_remote(remote.trim())? {
            alias.remote_name = Some(remote_name.to_owned());
            aliases.push(alias);
        }
        if aliases.len() > MAX_REMOTES {
            return Err(ProbeFailure::Failed("remote_limit_exceeded"));
        }
    }
    aliases.sort_by(|left, right| {
        (&left.host, &left.namespace, &left.name, &left.remote_name).cmp(&(
            &right.host,
            &right.namespace,
            &right.name,
            &right.remote_name,
        ))
    });
    aliases.dedup();
    Ok(aliases)
}

fn normalize_remote(remote: &str) -> Result<Option<RepositoryAlias>, ProbeFailure> {
    let (host, path) = if let Ok(url) = Url::parse(remote) {
        if !matches!(url.scheme(), "http" | "https" | "ssh" | "git") {
            return Ok(None);
        }
        if matches!(url.scheme(), "http" | "https")
            && (!url.username().is_empty() || url.password().is_some())
        {
            return Err(ProbeFailure::Unsafe("credential_bearing_remote"));
        }
        let host = url
            .host_str()
            .ok_or(ProbeFailure::Failed("remote_host_missing"))?;
        (
            host.to_ascii_lowercase(),
            url.path().trim_matches('/').to_owned(),
        )
    } else if let Some((authority, path)) = remote.split_once(':') {
        if authority.contains('/') || path.is_empty() {
            return Ok(None);
        }
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host.to_ascii_lowercase(), path.trim_matches('/').to_owned())
    } else {
        return Ok(None);
    };
    let path = path.strip_suffix(".git").unwrap_or(&path);
    let mut parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.len() < 2
        || parts.iter().any(|part| {
            part == "."
                || part == ".."
                || part
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || matches!(byte, b'@' | b'\\'))
        })
    {
        return Err(ProbeFailure::Failed("remote_path_invalid"));
    }
    let name = parts
        .pop()
        .ok_or(ProbeFailure::Failed("remote_name_missing"))?;
    Ok(Some(RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host,
        namespace: parts,
        name,
        remote_name: None,
    }))
}

pub(super) fn object_format_name(format: GitObjectFormat) -> &'static [u8] {
    match format {
        GitObjectFormat::Sha1 => b"sha1",
        GitObjectFormat::Sha256 => b"sha256",
    }
}

pub(super) fn digest_hex(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
        digest.update([0]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(dead_code)]
fn _os_str_is_bounded(value: &OsStr) -> bool {
    value.as_encoded_bytes().len() <= MAX_GIT_OUTPUT_BYTES
}
