use ctx_history_core::{
    GitObjectFormat, RepositoryBinding, RepositoryFileObservationKind, RepositoryOutcomeKind,
    RepositoryVcsObservationKind, StableEntityId, CORE_CONTENT_POLICY_REVISION,
    CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION,
};
use ctx_history_index::CoreEventRecord;
use ctx_pro_host_protocol::{
    BlameResult, EvidenceCitation, NumberedEvidence, ResolvedBlameTarget, ResourceKind, ResourceRef,
};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

pub(crate) const MAX_EVIDENCE_PREVIEW_CITATIONS: usize = 3;
pub(crate) const MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES: usize = 512;
pub(crate) const MAX_EVIDENCE_PREVIEW_BODY_BYTES: usize = 64 * 1_024;
pub(crate) const MAX_EVIDENCE_PREVIEW_BODY_LINES: usize = 4_096;

const VALIDATED_PROVIDER: &str = "codex";
const VALIDATED_SOURCE_FORMAT: &str = "codex_session_jsonl";
const VALIDATED_SCHEMA_VARIANT: &str = "codex-nativepath-jsonl-v0";
const VALIDATED_PROVIDER_IDENTITY_VERSION: u32 = 1;
const VALIDATED_PARSER_REVISION: &str = "codex-nativepath-core-record-v7";

/// Exact Core evidence whose generation, digest, and coordinates were verified by hydration.
///
/// Construction is deliberately fail-closed. A caller cannot pass a bare Core record to the
/// projector and accidentally bypass the citation identity checks.
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
            || actual_digest != cited_digest
            || !is_lower_sha256(cited_digest)
            || !citation_matches_record(citation, record)
            || !validated_codex_contract(record)
        {
            return None;
        }
        Some(Self { numbered, record })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidencePreviewModel {
    pub(crate) previews: Vec<EvidencePreview>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidencePreview {
    pub(crate) evidence_numbers: Vec<u32>,
    pub(crate) event_id: StableEntityId,
    pub(crate) event_sequence: u64,
    pub(crate) kind: EvidencePreviewKind,
    /// An exact, complete UTF-8 unit copied from `CoreContent::normalized_body`.
    pub(crate) excerpt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidencePreviewKind {
    File(RepositoryFileObservationKind),
    Commit,
}

/// Projects bounded human-only evidence previews without mutating the blame result or Core data.
#[must_use]
pub(crate) fn project_evidence_previews(
    result: &BlameResult,
    verified: &[VerifiedEvidenceRecord<'_>],
) -> EvidencePreviewModel {
    if matches!(result.target, ResolvedBlameTarget::PullRequest { .. }) {
        return EvidencePreviewModel {
            previews: Vec::new(),
        };
    }

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
        let Some((kind, excerpt)) = project_one(&result.target, candidate.record) else {
            continue;
        };

        if let Some(existing) = previews.iter_mut().find(|preview| {
            preview.event_id == candidate.record.event_id && preview.excerpt == excerpt
        }) {
            existing.evidence_numbers.push(numbered.number);
            continue;
        }
        previews.push(EvidencePreview {
            evidence_numbers: vec![numbered.number],
            event_id: candidate.record.event_id,
            event_sequence: candidate.record.event_sequence,
            kind,
            excerpt: excerpt.to_owned(),
        });
    }

    EvidencePreviewModel { previews }
}

fn citation_matches_record(citation: &EvidenceCitation, record: &CoreEventRecord) -> bool {
    let event = &record.event;
    let core = &record.core_record;
    event.provider == VALIDATED_PROVIDER
        && event.source.provider() == VALIDATED_PROVIDER
        && core.source.provider() == VALIDATED_PROVIDER
        && citation.source.exact_descriptor_eq(&event.source)
        && citation.source.exact_descriptor_eq(&core.source)
        && citation.session_id == event.session_id
        && citation.session_id == core.session_id
        && citation.event_id == event.event_id
        && citation.event_id == core.event_id
        && citation.event_sequence == event.event_sequence
        && citation.event_sequence == core.event_sequence
}

fn validated_codex_contract(record: &CoreEventRecord) -> bool {
    let event = &record.event;
    let core = &record.core_record;
    core.record_version == CORE_RECORD_VERSION
        && core.normalization_revision == CORE_NORMALIZATION_REVISION
        && core.content.policy_revision == CORE_CONTENT_POLICY_REVISION
        && core.parser_revision == VALIDATED_PARSER_REVISION
        && core.source.source_format() == VALIDATED_SOURCE_FORMAT
        && core.source.schema_variant() == VALIDATED_SCHEMA_VARIANT
        && core.source.provider_identity_version() == VALIDATED_PROVIDER_IDENTITY_VERSION
        && event.source_format == VALIDATED_SOURCE_FORMAT
        && event.event_type == core.event_type
        && event.role == core.role
        && matches!(
            (core.event_type.as_str(), core.role.as_deref()),
            ("tool_call", Some("assistant")) | ("command_output", Some("tool"))
        )
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn project_one<'a>(
    target: &ResolvedBlameTarget,
    record: &'a CoreEventRecord,
) -> Option<(EvidencePreviewKind, &'a str)> {
    let body = record.core_record.content.normalized_body.as_deref()?;
    let lines = body_lines(body)?;
    let (kind, excerpt) = match target {
        ResolvedBlameTarget::File {
            path, repository, ..
        } => {
            validated_file_event_shape(record)?;
            let binding = exact_repository_binding(repository, record)?;
            let (kind, range) = file_unit(path, binding, record, &lines)?;
            (
                EvidencePreviewKind::File(kind),
                &body[range.start..range.end],
            )
        }
        ResolvedBlameTarget::Commit { commit, repository } => {
            validated_commit_event_shape(record)?;
            let binding = exact_repository_binding(repository, record)?;
            commit_oid_matches_binding(&commit.display, binding)?;
            let range = commit_unit(&commit.display, &binding.binding_id, record, &lines)?;
            (EvidencePreviewKind::Commit, &body[range.start..range.end])
        }
        ResolvedBlameTarget::PullRequest { .. } => return None,
    };
    (excerpt.len() <= MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES).then_some((kind, excerpt))
}

fn validated_file_event_shape(record: &CoreEventRecord) -> Option<()> {
    (record.core_record.event_type == "tool_call"
        && record.core_record.role.as_deref() == Some("assistant"))
    .then_some(())
}

fn validated_commit_event_shape(record: &CoreEventRecord) -> Option<()> {
    (record.core_record.event_type == "command_output"
        && record.core_record.role.as_deref() == Some("tool"))
    .then_some(())
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

fn commit_oid_matches_binding(target: &str, binding: &RepositoryBinding) -> Option<()> {
    let format = match target.len() {
        40 => GitObjectFormat::Sha1,
        64 => GitObjectFormat::Sha256,
        _ => return None,
    };
    (binding.git_object_format == Some(format)).then_some(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteSpan {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
struct BodyLine<'a> {
    span: ByteSpan,
    text: &'a str,
}

fn body_lines(body: &str) -> Option<Vec<BodyLine<'_>>> {
    if body.len() > MAX_EVIDENCE_PREVIEW_BODY_BYTES {
        return None;
    }
    let line_count = body
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .checked_add(usize::from(!body.is_empty() && !body.ends_with('\n')))?;
    if line_count > MAX_EVIDENCE_PREVIEW_BODY_LINES {
        return None;
    }
    let mut lines = Vec::with_capacity(line_count);
    let mut start = 0usize;
    for segment in body.split_inclusive('\n') {
        let end = start + segment.len() - usize::from(segment.ends_with('\n'));
        let raw = &body[start..end];
        lines.push(BodyLine {
            span: ByteSpan { start, end },
            text: raw.strip_suffix('\r').unwrap_or(raw),
        });
        start += segment.len();
    }
    if start < body.len() {
        lines.push(BodyLine {
            span: ByteSpan {
                start,
                end: body.len(),
            },
            text: &body[start..],
        });
    }
    Some(lines)
}

#[derive(Debug, Clone, Copy)]
struct FileUnit {
    kind: RepositoryFileObservationKind,
    span: ByteSpan,
}

fn file_unit(
    target: &str,
    repository_binding: &RepositoryBinding,
    record: &CoreEventRecord,
    lines: &[BodyLine<'_>],
) -> Option<(RepositoryFileObservationKind, ByteSpan)> {
    let mut observations = record
        .core_record
        .repository_file_observations
        .iter()
        .filter(|observation| {
            observation.repository_binding_id == repository_binding.binding_id
                && (observation.relative_path == target
                    || observation.prior_relative_path.as_deref() == Some(target))
        });
    let observation = observations.next()?;
    if observations.next().is_some() || observation.kind == RepositoryFileObservationKind::Unknown {
        return None;
    }

    let units = match file_grammar(lines)? {
        FileGrammar::ApplyPatch => apply_patch_units(target, repository_binding, lines),
        FileGrammar::Diff => diff_units(target, lines),
        FileGrammar::NarrowResult => narrow_result_units(target, repository_binding, lines),
    };
    if units.len() != 1 || units[0].kind != observation.kind {
        return None;
    }
    Some((observation.kind, units[0].span))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileGrammar {
    ApplyPatch,
    Diff,
    NarrowResult,
}

fn file_grammar(lines: &[BodyLine<'_>]) -> Option<FileGrammar> {
    let apply = has_apply_patch_syntax(lines);
    let diff = lines
        .iter()
        .any(|line| line.text.starts_with("diff --git "));
    let narrow = lines.iter().any(|line| {
        let text = line.text.trim();
        [
            "created: ",
            "modified: ",
            "deleted: ",
            "read: ",
            "renamed: ",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
    });
    match (apply, diff, narrow) {
        (true, false, false) => Some(FileGrammar::ApplyPatch),
        (false, true, false) => Some(FileGrammar::Diff),
        (false, false, true) => Some(FileGrammar::NarrowResult),
        _ => None,
    }
}

fn has_apply_patch_syntax(lines: &[BodyLine<'_>]) -> bool {
    lines.iter().any(|line| {
        let text = line.text.trim();
        text == "*** Begin Patch"
            || text == "*** End Patch"
            || text == "apply_patch: *** Begin Patch"
            || [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ]
            .iter()
            .any(|prefix| text.starts_with(prefix))
    })
}

fn apply_patch_units(
    target: &str,
    repository_binding: &RepositoryBinding,
    lines: &[BodyLine<'_>],
) -> Vec<FileUnit> {
    let mut units = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let text = lines[index].text.trim();
        if let Some(path) = text.strip_prefix("*** Add File: ") {
            push_path_unit(
                &mut units,
                target,
                repository_binding,
                path,
                RepositoryFileObservationKind::Created,
                lines[index].span,
            );
        } else if let Some(path) = text.strip_prefix("*** Delete File: ") {
            push_path_unit(
                &mut units,
                target,
                repository_binding,
                path,
                RepositoryFileObservationKind::Deleted,
                lines[index].span,
            );
        } else if let Some(old_path) = text.strip_prefix("*** Update File: ") {
            let move_to = lines.get(index + 1).and_then(|line| {
                line.text
                    .trim()
                    .strip_prefix("*** Move to: ")
                    .map(|path| (line, path))
            });
            if let Some((move_line, new_path)) = move_to {
                if authorized_path_matches(old_path, target, repository_binding) {
                    units.push(FileUnit {
                        kind: RepositoryFileObservationKind::Renamed,
                        span: ByteSpan {
                            start: lines[index].span.start,
                            end: move_line.span.end,
                        },
                    });
                } else if authorized_path_matches(new_path, target, repository_binding) {
                    units.push(FileUnit {
                        kind: RepositoryFileObservationKind::Renamed,
                        span: move_line.span,
                    });
                }
                index += 1;
            } else {
                push_path_unit(
                    &mut units,
                    target,
                    repository_binding,
                    old_path,
                    RepositoryFileObservationKind::Modified,
                    lines[index].span,
                );
            }
        }
        index += 1;
    }
    units
}

fn diff_units(target: &str, lines: &[BodyLine<'_>]) -> Vec<FileUnit> {
    let starts = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.text.starts_with("diff --git ").then_some(index))
        .collect::<Vec<_>>();
    let mut units = Vec::new();
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(lines.len());
        let section = &lines[start..end];
        let Some((old_header, new_header)) = diff_header_paths(section[0].text) else {
            continue;
        };
        let new_modes = prefixed_lines(section, "new file mode ");
        let deleted_modes = prefixed_lines(section, "deleted file mode ");
        let rename_from = prefixed_lines(section, "rename from ");
        let rename_to = prefixed_lines(section, "rename to ");

        if rename_from.len() == 1
            && rename_to.len() == 1
            && new_modes.is_empty()
            && deleted_modes.is_empty()
            && diff_path_matches(old_header, "a/", rename_from[0].1)
            && diff_path_matches(new_header, "b/", rename_to[0].1)
        {
            if rename_from[0].1 == target {
                units.push(FileUnit {
                    kind: RepositoryFileObservationKind::Renamed,
                    span: rename_from[0].0.span,
                });
            } else if rename_to[0].1 == target {
                units.push(FileUnit {
                    kind: RepositoryFileObservationKind::Renamed,
                    span: rename_to[0].0.span,
                });
            }
            continue;
        }

        if !rename_from.is_empty() || !rename_to.is_empty() {
            continue;
        }
        let header_matches = diff_path_matches(old_header, "a/", target)
            && diff_path_matches(new_header, "b/", target);
        if !header_matches {
            continue;
        }
        match (new_modes.as_slice(), deleted_modes.as_slice()) {
            ([(mode_line, _)], []) if mode_line.span.start == section[0].span.end + 1 => {
                units.push(FileUnit {
                    kind: RepositoryFileObservationKind::Created,
                    span: ByteSpan {
                        start: section[0].span.start,
                        end: mode_line.span.end,
                    },
                });
            }
            ([], [(mode_line, _)]) if mode_line.span.start == section[0].span.end + 1 => {
                units.push(FileUnit {
                    kind: RepositoryFileObservationKind::Deleted,
                    span: ByteSpan {
                        start: section[0].span.start,
                        end: mode_line.span.end,
                    },
                });
            }
            ([], []) => units.push(FileUnit {
                kind: RepositoryFileObservationKind::Modified,
                span: section[0].span,
            }),
            _ => {}
        }
    }
    units
}

fn diff_header_paths(line: &str) -> Option<(&str, &str)> {
    let mut fields = line.split_ascii_whitespace();
    (fields.next() == Some("diff") && fields.next() == Some("--git")).then_some(())?;
    let old = fields.next()?;
    let new = fields.next()?;
    fields.next().is_none().then_some((old, new))
}

fn diff_path_matches(candidate: &str, side_prefix: &str, relative_path: &str) -> bool {
    candidate.strip_prefix(side_prefix) == Some(relative_path)
}

fn prefixed_lines<'a>(lines: &'a [BodyLine<'a>], prefix: &str) -> Vec<(BodyLine<'a>, &'a str)> {
    lines
        .iter()
        .filter_map(|line| line.text.strip_prefix(prefix).map(|value| (*line, value)))
        .collect()
}

fn narrow_result_units(
    target: &str,
    repository_binding: &RepositoryBinding,
    lines: &[BodyLine<'_>],
) -> Vec<FileUnit> {
    let mut units = Vec::new();
    for line in lines {
        let text = line.text.trim();
        for (prefix, kind) in [
            ("created: ", RepositoryFileObservationKind::Created),
            ("modified: ", RepositoryFileObservationKind::Modified),
            ("deleted: ", RepositoryFileObservationKind::Deleted),
            ("read: ", RepositoryFileObservationKind::Read),
        ] {
            if let Some(path) = text.strip_prefix(prefix) {
                push_path_unit(
                    &mut units,
                    target,
                    repository_binding,
                    path,
                    kind,
                    line.span,
                );
            }
        }
        if let Some(paths) = text.strip_prefix("renamed: ") {
            if let Some((old, new)) = paths.split_once(" -> ") {
                if authorized_path_matches(old, target, repository_binding)
                    || authorized_path_matches(new, target, repository_binding)
                {
                    units.push(FileUnit {
                        kind: RepositoryFileObservationKind::Renamed,
                        span: line.span,
                    });
                }
            }
        }
    }
    units
}

fn push_path_unit(
    units: &mut Vec<FileUnit>,
    target: &str,
    repository_binding: &RepositoryBinding,
    candidate: &str,
    kind: RepositoryFileObservationKind,
    span: ByteSpan,
) {
    if authorized_path_matches(candidate, target, repository_binding) {
        units.push(FileUnit { kind, span });
    }
}

fn authorized_path_matches(
    candidate: &str,
    target: &str,
    repository_binding: &RepositoryBinding,
) -> bool {
    if candidate == target {
        return true;
    }
    let target_path = Path::new(target);
    if target_path.is_absolute()
        || target_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return false;
    }
    let Some(authorization) = &repository_binding.local_root_authorization else {
        return false;
    };
    let local_root = Path::new(&authorization.local_root);
    if !local_root.is_absolute()
        || local_root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return false;
    }
    local_root.join(target_path).to_str() == Some(candidate)
}

fn commit_unit(
    target: &str,
    repository_binding_id: &str,
    record: &CoreEventRecord,
    lines: &[BodyLine<'_>],
) -> Option<ByteSpan> {
    if !is_canonical_oid(target) {
        return None;
    }
    let mut outcomes = record
        .core_record
        .repository_vcs_observations
        .iter()
        .filter_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::Outcome(outcome)
                if observation.repository_binding_id == repository_binding_id
                    && outcome.kind == RepositoryOutcomeKind::Commit
                    && outcome
                        .produced_object_ids
                        .iter()
                        .any(|object_id| object_id.hex == target) =>
            {
                Some(outcome)
            }
            _ => None,
        });
    outcomes.next()?;
    if outcomes.next().is_some() {
        return None;
    }

    let output_start = successful_output_start(lines)?;
    let mut matched_span = None;
    let mut occurrences = 0usize;
    for line in lines.iter().copied().skip(output_start) {
        let count = exact_oid_occurrences(line.text, target);
        if count > 0 {
            occurrences = occurrences.checked_add(count)?;
            matched_span = Some(line.span);
        }
    }
    (occurrences == 1).then_some(matched_span?)
}

fn is_canonical_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exact_oid_occurrences(line: &str, target: &str) -> usize {
    line.match_indices(target)
        .filter(|(start, _)| {
            let before = (*start > 0).then(|| line.as_bytes()[*start - 1]);
            let end = *start + target.len();
            let after = (end < line.len()).then(|| line.as_bytes()[end]);
            before.is_none_or(|byte| !byte.is_ascii_hexdigit())
                && after.is_none_or(|byte| !byte.is_ascii_hexdigit())
        })
        .count()
}

fn successful_output_start(lines: &[BodyLine<'_>]) -> Option<usize> {
    let statuses = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.text
                .trim()
                .strip_prefix("Process exited with code ")
                .map(|code| (index, code))
        })
        .collect::<Vec<_>>();
    let outputs = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line.text.trim() == "Output:").then_some(index))
        .collect::<Vec<_>>();
    match (statuses.as_slice(), outputs.as_slice()) {
        ([(status_index, "0")], [output_index]) if status_index < output_index => {
            output_index.checked_add(1)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
