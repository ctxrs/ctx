use ctx_history_core::{
    RepositoryBinding, RepositoryFileInvocationEvidence, RepositoryFileInvocationKind,
    CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION,
};
use ctx_history_index::CoreEventRecord;
use ctx_pro_host_protocol::{
    BlameResult, EvidenceCitation, NumberedEvidence, ResolvedBlameTarget, ResourceKind, ResourceRef,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const MAX_EVIDENCE_PREVIEW_CITATIONS: usize = 3;
pub(crate) const MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES: usize = 512;

/// Exact Core evidence whose generation and coordinates were verified by hydration and whose
/// stored Core bytes are digest-verified during construction.
///
/// Construction is deliberately fail-closed. A caller cannot pass a bare Core record to the
/// projector and accidentally bypass citation identity or current Core-contract checks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VerifiedEvidenceRecord<'a> {
    numbered: &'a NumberedEvidence,
    record: &'a CoreEventRecord,
}

impl<'a> VerifiedEvidenceRecord<'a> {
    #[must_use]
    pub(crate) fn new(
        numbered: &'a NumberedEvidence,
        hydrated_core_generation_id: &str,
        record: &'a CoreEventRecord,
    ) -> Option<Self> {
        let citation = &numbered.citation;
        let cited_digest = citation.evidence_sha256.as_deref()?;
        let encoded = record.core_record.encode_stored().ok()?;
        let actual_digest = format!("{:x}", Sha256::digest(encoded));
        if citation.byte_range.is_some()
            || hydrated_core_generation_id != citation.core_generation_id
            || !is_lower_sha256(hydrated_core_generation_id)
            || actual_digest != cited_digest
            || !is_lower_sha256(cited_digest)
            || !citation_matches_record(citation, record)
            || !validated_current_core_contract(record)
        {
            return None;
        }
        Some(Self { numbered, record })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EvidencePreviewModel {
    pub(crate) previews: Vec<EvidencePreview>,
}

/// Provider-neutral evidence for one exact provider-native file-operation request.
///
/// `operation` describes requested intent, not a successful filesystem effect. `excerpt` is an
/// exact UTF-8 byte range copied from `CoreContent::normalized_body`; presentation sanitization is
/// deliberately separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EvidencePreview {
    pub(crate) citation_numbers: Vec<u32>,
    pub(crate) operation: RepositoryFileInvocationKind,
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prior_path: Option<String>,
    pub(crate) tool_name: String,
    pub(crate) excerpt: String,
}

/// Projects bounded provider-neutral file invocation evidence without mutating the blame result
/// or Core data.
#[must_use]
pub(crate) fn project_evidence_previews(
    result: &BlameResult,
    verified: &[VerifiedEvidenceRecord<'_>],
) -> EvidencePreviewModel {
    let ResolvedBlameTarget::File {
        path, repository, ..
    } = &result.target
    else {
        return unavailable();
    };

    let mut citations = result.evidence.iter().collect::<Vec<_>>();
    citations.sort_by_key(|evidence| evidence.number);
    citations.truncate(MAX_EVIDENCE_PREVIEW_CITATIONS);

    let mut previews: Vec<EvidencePreview> = Vec::new();
    for numbered in citations {
        let mut matching = verified.iter().filter(|candidate| {
            candidate.numbered.number == numbered.number
                && candidate.numbered.citation == numbered.citation
        });
        let Some(candidate) = matching.next() else {
            continue;
        };
        if matching.next().is_some() {
            continue;
        }
        let Some(mut preview) = project_one(path, repository, candidate.record) else {
            continue;
        };

        // Replayed provider events can have distinct stable IDs while carrying the same exact
        // invocation. Keep one visible item and preserve every citation number deterministically.
        if let Some(existing) = previews
            .iter_mut()
            .find(|existing| same_item(existing, &preview))
        {
            existing.citation_numbers.push(numbered.number);
            continue;
        }
        preview.citation_numbers.push(numbered.number);
        previews.push(preview);
    }

    EvidencePreviewModel { previews }
}

fn unavailable() -> EvidencePreviewModel {
    EvidencePreviewModel {
        previews: Vec::new(),
    }
}

fn same_item(left: &EvidencePreview, right: &EvidencePreview) -> bool {
    left.operation == right.operation
        && left.path == right.path
        && left.prior_path == right.prior_path
        && left.tool_name == right.tool_name
        && left.excerpt == right.excerpt
}

fn citation_matches_record(citation: &EvidenceCitation, record: &CoreEventRecord) -> bool {
    let event = &record.event;
    let core = &record.core_record;
    citation.source.validate_contract().is_ok()
        && event.source.validate_contract().is_ok()
        && event.provider == event.source.provider()
        && event.provider == core.source.provider()
        && event.source_format == event.source.source_format()
        && event.source_format == core.source.source_format()
        && event.source.exact_descriptor_eq(&core.source)
        && citation.source.exact_descriptor_eq(&event.source)
        && citation.source.exact_descriptor_eq(&core.source)
        && citation.session_id == event.session_id
        && citation.session_id == core.session_id
        && citation.event_id == event.event_id
        && citation.event_id == core.event_id
        && citation.event_sequence == event.event_sequence
        && citation.event_sequence == core.event_sequence
}

fn validated_current_core_contract(record: &CoreEventRecord) -> bool {
    let core = &record.core_record;
    core.validate_contract().is_ok()
        && core.record_version == CORE_RECORD_VERSION
        && core.normalization_revision == CORE_NORMALIZATION_REVISION
        && core.content.policy_revision == CORE_CONTENT_POLICY_REVISION
        && core.event_type == "tool_call"
        && core.role.as_deref() == Some("assistant")
        && event_projection_matches_core(record)
}

fn event_projection_matches_core(record: &CoreEventRecord) -> bool {
    let event = &record.event;
    let core = &record.core_record;
    event.event_id == core.event_id
        && event.session_id == core.session_id
        && event.parent_session_id == core.parent_session_id
        && event.root_session_id == core.root_session_id
        && event.source.exact_descriptor_eq(&core.source)
        && event.provider_session_id == core.provider_session_id
        && event.native_event_id == core.native_event_id
        && event.branch == core.branch
        && event.agent_type == core.agent_type
        && event.is_primary == core.is_primary
        && event.event_sequence == core.event_sequence
        && event.occurred_at_unix_ms == core.occurred_at_unix_ms
        && event.event_type == core.event_type
        && event.role == core.role
        && event.workspace == core.workspace
        && event.cwd == core.cwd
        && event.touched_files == projected_touched_files(record)
}

fn projected_touched_files(record: &CoreEventRecord) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for observation in &record.core_record.repository_file_observations {
        paths.insert(observation.relative_path.clone());
        if let Some(prior_path) = &observation.prior_relative_path {
            paths.insert(prior_path.clone());
        }
    }
    paths.into_iter().collect()
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn project_one(
    target: &str,
    repository: &ResourceRef,
    record: &CoreEventRecord,
) -> Option<EvidencePreview> {
    let binding = exact_repository_binding(repository, record)?;
    let target_invocation = exact_target_invocation(target, binding, record)?;
    let invocation = target_invocation.invocation;
    if invocation.repository_binding_id != binding.binding_id
        || invocation_ranges_overlap(target_invocation.index, invocation, record)
    {
        return None;
    }
    let tool_name = invocation
        .tool_name
        .as_deref()
        .filter(|tool_name| !tool_name.trim().is_empty())?;
    let range = invocation.normalized_text_range?;
    let body = record.core_record.content.normalized_body.as_deref()?;
    let start = usize::try_from(range.start).ok()?;
    let end = usize::try_from(range.end).ok()?;
    let excerpt = body.get(start..end)?;
    if excerpt.len() > MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES {
        return None;
    }

    Some(EvidencePreview {
        citation_numbers: Vec::new(),
        operation: invocation.kind,
        path: invocation.relative_path.clone(),
        prior_path: invocation.prior_relative_path.clone(),
        tool_name: tool_name.to_owned(),
        excerpt: excerpt.to_owned(),
    })
}

fn exact_repository_binding<'a>(
    repository: &ResourceRef,
    record: &'a CoreEventRecord,
) -> Option<&'a RepositoryBinding> {
    if repository.kind != ResourceKind::Repository || repository.validate().is_err() {
        return None;
    }
    let mut matches = record
        .core_record
        .repository_bindings
        .iter()
        .filter(|binding| binding.logical_repository_id == repository.display);
    let binding = matches.next()?;
    matches.next().is_none().then_some(binding)
}

#[derive(Debug, Clone, Copy)]
struct TargetInvocation<'a> {
    index: usize,
    invocation: &'a RepositoryFileInvocationEvidence,
    matched_relative_path: &'a str,
}

fn exact_target_invocation<'a>(
    target: &str,
    selected_binding: &RepositoryBinding,
    record: &'a CoreEventRecord,
) -> Option<TargetInvocation<'a>> {
    let absolute_style = absolute_path_style(target);
    if absolute_style.is_none() && !looks_like_absolute_path(target) {
        let mut matches = record
            .core_record
            .repository_file_invocation_evidence
            .iter()
            .enumerate()
            .flat_map(|(index, invocation)| {
                invocation_paths(invocation)
                    .filter(move |relative_path| *relative_path == target)
                    .map(move |matched_relative_path| TargetInvocation {
                        index,
                        invocation,
                        matched_relative_path,
                    })
            });
        let matched = matches.next()?;
        return matches.next().is_none().then_some(matched);
    }

    absolute_style?;
    let mut selected_matches = record
        .core_record
        .repository_file_invocation_evidence
        .iter()
        .enumerate()
        .filter(|(_, invocation)| invocation.repository_binding_id == selected_binding.binding_id)
        .flat_map(|(index, invocation)| {
            invocation_paths(invocation).filter_map(move |relative_path| {
                (certified_absolute_path(selected_binding, relative_path).as_deref()
                    == Some(target))
                .then_some(TargetInvocation {
                    index,
                    invocation,
                    matched_relative_path: relative_path,
                })
            })
        });
    let matched = selected_matches.next()?;
    if selected_matches.next().is_some()
        || absolute_invocation_is_ambiguous(target, matched, selected_binding, record)
    {
        return None;
    }
    Some(matched)
}

fn invocation_paths(invocation: &RepositoryFileInvocationEvidence) -> impl Iterator<Item = &str> {
    std::iter::once(invocation.relative_path.as_str())
        .chain(invocation.prior_relative_path.as_deref())
}

fn absolute_invocation_is_ambiguous(
    target: &str,
    selected: TargetInvocation<'_>,
    selected_binding: &RepositoryBinding,
    record: &CoreEventRecord,
) -> bool {
    record
        .core_record
        .repository_file_invocation_evidence
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != selected.index)
        .any(|(_, invocation)| {
            let Some(binding) = unique_binding(&invocation.repository_binding_id, record) else {
                return true;
            };
            invocation_paths(invocation).any(|relative_path| {
                absolute_competitor_is_ambiguous(
                    target,
                    selected.matched_relative_path,
                    relative_path,
                    binding,
                    selected_binding,
                )
            })
        })
}

fn absolute_competitor_is_ambiguous(
    target: &str,
    matched_relative_path: &str,
    competitor_relative_path: &str,
    competitor_binding: &RepositoryBinding,
    selected_binding: &RepositoryBinding,
) -> bool {
    if competitor_binding.binding_id == selected_binding.binding_id {
        return certified_absolute_path(competitor_binding, competitor_relative_path).as_deref()
            == Some(target);
    }
    match certified_absolute_path(competitor_binding, competitor_relative_path) {
        Some(path) => path == target,
        None => competitor_relative_path == matched_relative_path,
    }
}

fn unique_binding<'a>(
    binding_id: &str,
    record: &'a CoreEventRecord,
) -> Option<&'a RepositoryBinding> {
    let mut matches = record
        .core_record
        .repository_bindings
        .iter()
        .filter(|binding| binding.binding_id == binding_id);
    let binding = matches.next()?;
    matches.next().is_none().then_some(binding)
}

fn certified_absolute_path(binding: &RepositoryBinding, relative_path: &str) -> Option<String> {
    if relative_path.is_empty()
        || relative_path.starts_with('/')
        || relative_path.contains('\\')
        || relative_path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        || relative_path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return None;
    }
    let root = &binding.local_root_authorization.as_ref()?.local_root;
    let style = absolute_path_style(root)?;
    let separator = style.separator();
    let relative_path = if separator == '/' {
        relative_path.to_owned()
    } else {
        relative_path.replace('/', "\\")
    };
    Some(if root.ends_with(separator) {
        format!("{root}{relative_path}")
    } else {
        format!("{root}{separator}{relative_path}")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbsolutePathStyle {
    Posix,
    WindowsDrive { separator: char },
    WindowsUnc,
}

impl AbsolutePathStyle {
    const fn separator(self) -> char {
        match self {
            Self::Posix | Self::WindowsDrive { separator: '/' } => '/',
            Self::WindowsDrive { .. } | Self::WindowsUnc => '\\',
        }
    }
}

fn looks_like_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn absolute_path_style(path: &str) -> Option<AbsolutePathStyle> {
    if path
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return None;
    }

    if let Some(remainder) = path.strip_prefix("\\\\") {
        if path.contains('/') || !valid_absolute_components(remainder, '\\', 2) {
            return None;
        }
        return Some(AbsolutePathStyle::WindowsUnc);
    }

    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        let separator = char::from(bytes[2]);
        let other_separator = if separator == '/' { '\\' } else { '/' };
        let remainder = &path[3..];
        if remainder.contains(other_separator)
            || (!remainder.is_empty() && !valid_absolute_components(remainder, separator, 0))
        {
            return None;
        }
        return Some(AbsolutePathStyle::WindowsDrive { separator });
    }

    let remainder = path.strip_prefix('/')?;
    if path.contains('\\')
        || (!remainder.is_empty() && !valid_absolute_components(remainder, '/', 0))
    {
        return None;
    }
    Some(AbsolutePathStyle::Posix)
}

fn valid_absolute_components(remainder: &str, separator: char, minimum: usize) -> bool {
    let mut count = 0usize;
    for component in remainder.split(separator) {
        if component.is_empty() || matches!(component, "." | "..") {
            return false;
        }
        count += 1;
    }
    count >= minimum
}

fn invocation_ranges_overlap(
    selected_index: usize,
    selected: &RepositoryFileInvocationEvidence,
    record: &CoreEventRecord,
) -> bool {
    let Some(selected_range) = selected.normalized_text_range else {
        return false;
    };
    record
        .core_record
        .repository_file_invocation_evidence
        .iter()
        .enumerate()
        .any(|(index, invocation)| {
            index != selected_index
                && invocation.normalized_text_range.is_some_and(|range| {
                    selected_range.start < range.end && range.start < selected_range.end
                })
        })
}

#[cfg(test)]
mod tests;
