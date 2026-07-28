use super::reader::ScanState;
use super::*;

#[cfg(test)]
pub(crate) fn read_gemini_transcript_pages<'a>(
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    read_gemini_transcript_pages_with_profile(source, previous, GeminiNativePathProfile::CoreOnly)
}

pub(crate) fn read_gemini_transcript_pages_with_profile<'a>(
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
    profile: GeminiNativePathProfile,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    read_gemini_transcript_pages_from(source, previous, profile, None)
}

/// Reopens a source at a previously emitted safe page frontier. This is the
/// retry seam for a lagging Core or Pro consumer: the prefix digest and parser
/// revisions must still match, and growth is accepted only from an
/// append-safe boundary.
#[cfg(test)]
pub(crate) fn read_gemini_transcript_pages_from_frontier<'a>(
    source: &'a GeminiTranscriptSource,
    frontier: &GeminiPageFrontier,
    profile: GeminiNativePathProfile,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    read_gemini_transcript_pages_from(source, None, profile, Some(frontier))
}

fn read_gemini_transcript_pages_from<'a>(
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
    profile: GeminiNativePathProfile,
    resume_frontier: Option<&GeminiPageFrontier>,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    let initial_observation = GeminiFileObservation::from_metadata(source.source_file.metadata())?;
    if initial_observation != source.observation {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }

    let mut file = open_gemini_transcript(source)?;
    if GeminiFileObservation::from_metadata(&file.metadata()?)? != initial_observation {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }

    let mut prefix_hasher = new_prefix_hasher();
    let mut source_hasher = prefix_hasher.clone();
    let mut resumed_prefix = false;
    let mut skip_scan = false;
    let mut resume_boundary_safe = true;
    let mut terminal = true;
    let mut scan_start = 0_u64;
    let mut next_raw_ordinal = 0_u64;
    let mut retained_event_count = 0_u64;
    let mut rejected_records = 0_u64;
    let mut session = None;

    if let Some(frontier) = resume_frontier {
        if frontier.parser_revision != GEMINI_NATIVEPATH_PARSER_REVISION
            || frontier.policy_revision != GEMINI_NATIVEPATH_POLICY_REVISION
            || initial_observation.length < frontier.complete_prefix_end
            || (frontier.complete_prefix_end > 0 && frontier.session.is_none())
            || (initial_observation.length > frontier.complete_prefix_end
                && !frontier.append_boundary_safe)
            || !frontier_file_identity_matches(frontier, &initial_observation)
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let observed_prefix = hash_gemini_prefix(&mut file, frontier.complete_prefix_end)?;
        if prefix_digest(&observed_prefix) != frontier.complete_prefix_sha256 {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        prefix_hasher = observed_prefix;
        source_hasher = prefix_hasher.clone();
        resumed_prefix = true;
        resume_boundary_safe = frontier.append_boundary_safe;
        scan_start = frontier.complete_prefix_end;
        next_raw_ordinal = frontier.next_raw_ordinal;
        retained_event_count = frontier.retained_event_count;
        rejected_records = frontier.rejected_records;
        session.clone_from(&frontier.session);
    } else if let Some(previous) = previous.filter(|previous| {
        previous.checkpoint.source_path == source.path
            && previous.checkpoint.parser_revision == GEMINI_NATIVEPATH_PARSER_REVISION
            && previous.checkpoint.policy_revision == GEMINI_NATIVEPATH_POLICY_REVISION
            && initial_observation.length >= previous.checkpoint.complete_prefix_end
            && previous.checkpoint.session.is_some()
    }) {
        let checkpoint = &previous.checkpoint;
        let exact_observation = initial_observation == checkpoint.source_observation;
        let append_observation = initial_observation.length > checkpoint.source_observation.length
            && checkpoint.append_boundary_safe
            && same_physical_file(&checkpoint.source_observation, &initial_observation);
        let observed_prefix = hash_gemini_prefix(&mut file, checkpoint.complete_prefix_end)?;
        if (exact_observation || append_observation)
            && prefix_digest(&observed_prefix) == checkpoint.complete_prefix_sha256
        {
            prefix_hasher = observed_prefix;
            source_hasher = prefix_hasher.clone();
            resumed_prefix = true;
            skip_scan = exact_observation
                && checkpoint.terminal
                && initial_observation.length == checkpoint.complete_prefix_end;
            resume_boundary_safe = checkpoint.append_boundary_safe;
            terminal = if skip_scan { checkpoint.terminal } else { true };
            scan_start = checkpoint.complete_prefix_end;
            next_raw_ordinal = checkpoint.next_raw_ordinal;
            retained_event_count = checkpoint.retained_event_count;
            rejected_records = checkpoint.rejected_records;
            session.clone_from(&checkpoint.session);
        }
    }

    if !resumed_prefix {
        file.seek(SeekFrom::Start(0))?;
        prefix_hasher = new_prefix_hasher();
        source_hasher = prefix_hasher.clone();
        scan_start = 0;
        next_raw_ordinal = 0;
        retained_event_count = 0;
        rejected_records = 0;
        session = None;
    } else {
        file.seek(SeekFrom::Start(scan_start))?;
    }

    let state = ScanState {
        source,
        session,
        metrics: GeminiParserMetrics::default(),
        rejected_records,
        rejections: Vec::new(),
        retained_rows_this_scan: 0,
        emitted_rows_this_scan: 0,
    };
    Ok(GeminiNativePageReader {
        source,
        previous,
        initial_observation,
        source_hasher,
        resumed_prefix,
        skip_scan,
        reader: BufReader::new(file),
        prefix_hasher,
        offset: scan_start,
        raw_ordinal: next_raw_ordinal,
        complete_prefix_end: scan_start,
        append_boundary_safe: if resumed_prefix {
            resume_boundary_safe
        } else {
            true
        },
        terminal,
        retained_event_count,
        state,
        profile,
        outcome: None,
    })
}
