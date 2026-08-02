use ctx_history_core::{
    RepositoryFileObservationKind, RepositoryOutcomeKind, RepositoryVcsObservationKind,
    StableEntityId,
};
use ctx_history_index::CoreEventRecord;
use ctx_pro_host_protocol::{BlameResult, EvidenceCitation, NumberedEvidence, ResolvedBlameTarget};

pub(crate) const MAX_EVIDENCE_PREVIEW_CITATIONS: usize = 3;
pub(crate) const MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES: usize = 512;
pub(crate) const MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES: usize = 4_096;

const VALIDATED_PROVIDER: &str = "codex";

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
        hydrated_evidence_sha256: &str,
        record: &'a CoreEventRecord,
    ) -> Option<Self> {
        let citation = &numbered.citation;
        let cited_digest = citation.evidence_sha256.as_deref()?;
        if citation.byte_range.is_some()
            || hydrated_core_generation_id != citation.core_generation_id
            || hydrated_evidence_sha256 != cited_digest
            || !is_lower_sha256(hydrated_evidence_sha256)
            || !citation_matches_record(citation, record)
            || record.core_record.validate_contract().is_err()
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
    let mut aggregate_bytes = 0usize;
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
        let Some(next_aggregate) = aggregate_bytes.checked_add(excerpt.len()) else {
            continue;
        };
        if next_aggregate > MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES {
            continue;
        }
        aggregate_bytes = next_aggregate;
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
    let (kind, excerpt) = match target {
        ResolvedBlameTarget::File { path, .. } => {
            let (kind, range) = file_unit(path, record, body)?;
            (
                EvidencePreviewKind::File(kind),
                &body[range.start..range.end],
            )
        }
        ResolvedBlameTarget::Commit { commit, .. } => {
            let range = commit_unit(&commit.display, record, body)?;
            (EvidencePreviewKind::Commit, &body[range.start..range.end])
        }
        ResolvedBlameTarget::PullRequest { .. } => return None,
    };
    (excerpt.len() <= MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES).then_some((kind, excerpt))
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

fn body_lines(body: &str) -> Vec<BodyLine<'_>> {
    let mut lines = Vec::new();
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
    lines
}

#[derive(Debug, Clone, Copy)]
struct FileUnit {
    kind: RepositoryFileObservationKind,
    span: ByteSpan,
}

fn file_unit(
    target: &str,
    record: &CoreEventRecord,
    body: &str,
) -> Option<(RepositoryFileObservationKind, ByteSpan)> {
    let mut observations = record
        .core_record
        .repository_file_observations
        .iter()
        .filter(|observation| {
            observation.relative_path == target
                || observation.prior_relative_path.as_deref() == Some(target)
        });
    let observation = observations.next()?;
    if observations.next().is_some() || observation.kind == RepositoryFileObservationKind::Unknown {
        return None;
    }

    let lines = body_lines(body);
    let units = if has_apply_patch_syntax(&lines) {
        apply_patch_units(target, &lines)
    } else if lines
        .iter()
        .any(|line| line.text.starts_with("diff --git "))
    {
        diff_units(target, &lines)
    } else {
        narrow_result_units(target, &lines)
    };
    if units.len() != 1 || units[0].kind != observation.kind {
        return None;
    }
    Some((observation.kind, units[0].span))
}

fn has_apply_patch_syntax(lines: &[BodyLine<'_>]) -> bool {
    lines.iter().any(|line| {
        let text = line.text.trim();
        text.contains("*** Begin Patch")
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

fn apply_patch_units(target: &str, lines: &[BodyLine<'_>]) -> Vec<FileUnit> {
    let mut units = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let text = lines[index].text.trim();
        if let Some(path) = text.strip_prefix("*** Add File: ") {
            push_path_unit(
                &mut units,
                target,
                path,
                RepositoryFileObservationKind::Created,
                lines[index].span,
            );
        } else if let Some(path) = text.strip_prefix("*** Delete File: ") {
            push_path_unit(
                &mut units,
                target,
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
                if path_matches(old_path, target) {
                    units.push(FileUnit {
                        kind: RepositoryFileObservationKind::Renamed,
                        span: ByteSpan {
                            start: lines[index].span.start,
                            end: move_line.span.end,
                        },
                    });
                } else if path_matches(new_path, target) {
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
            && path_matches(old_header, rename_from[0].1)
            && path_matches(new_header, rename_to[0].1)
        {
            if path_matches(rename_from[0].1, target) {
                units.push(FileUnit {
                    kind: RepositoryFileObservationKind::Renamed,
                    span: rename_from[0].0.span,
                });
            } else if path_matches(rename_to[0].1, target) {
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
        let header_matches = path_matches(old_header, target) && path_matches(new_header, target);
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

fn prefixed_lines<'a>(lines: &'a [BodyLine<'a>], prefix: &str) -> Vec<(BodyLine<'a>, &'a str)> {
    lines
        .iter()
        .filter_map(|line| line.text.strip_prefix(prefix).map(|value| (*line, value)))
        .collect()
}

fn narrow_result_units(target: &str, lines: &[BodyLine<'_>]) -> Vec<FileUnit> {
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
                push_path_unit(&mut units, target, path, kind, line.span);
            }
        }
        if let Some(paths) = text.strip_prefix("renamed: ") {
            if let Some((old, new)) = paths.split_once(" -> ") {
                if path_matches(old, target) || path_matches(new, target) {
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
    candidate: &str,
    kind: RepositoryFileObservationKind,
    span: ByteSpan,
) {
    if path_matches(candidate, target) {
        units.push(FileUnit { kind, span });
    }
}

fn path_matches(candidate: &str, target: &str) -> bool {
    candidate == target
        || (!target.starts_with('/')
            && candidate
                .strip_suffix(target)
                .is_some_and(|prefix| prefix.ends_with('/')))
}

fn commit_unit(target: &str, record: &CoreEventRecord, body: &str) -> Option<ByteSpan> {
    if !is_canonical_oid(target) {
        return None;
    }
    let mut outcomes = record
        .core_record
        .repository_vcs_observations
        .iter()
        .filter_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::Outcome(outcome)
                if outcome.kind == RepositoryOutcomeKind::Commit
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

    let lines = body_lines(body);
    let output_start = successful_output_start(&lines)?;
    let mut matched_span = None;
    let mut occurrences = 0usize;
    for line in lines.into_iter().skip(output_start) {
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
