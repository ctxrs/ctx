use crate::*;

pub(crate) fn ranked_candidates(
    store: &Store,
    query: Option<&str>,
    options: &PacketOptions,
    file_scope: Option<&FileTouchScope>,
) -> Result<CandidateSearch> {
    let target_candidates = options.limit.saturating_add(1);
    let terms = query_terms(query.unwrap_or_default());
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::<Uuid>::new();
    let mut scan_budget_exhausted = false;
    let file_only = terms.is_empty() && file_scope.is_some();
    if terms.is_empty() && !file_only {
        return Ok(CandidateSearch {
            candidates,
            scan_budget_exhausted,
        });
    }

    if file_only {
        let Some(scope) = file_scope else {
            return Ok(CandidateSearch {
                candidates,
                scan_budget_exhausted,
            });
        };
        for record_id in &scope.history_record_ids {
            if !seen.insert(*record_id) {
                continue;
            }
            let record = store.get_record(*record_id)?;
            if let Some(candidate) =
                candidate_for_record(store, record, &terms, &options.filters, file_scope)?
            {
                candidates.push(candidate);
            }
        }
        normalize_scores(&mut candidates);
        candidates.sort_by(compare_candidates);
        if candidates.len() > target_candidates {
            candidates.truncate(target_candidates);
        }
        return Ok(CandidateSearch {
            candidates,
            scan_budget_exhausted,
        });
    }

    let filtered = has_filters(&options.filters);
    if filtered {
        let page_size = FILTERED_SEARCH_PAGE_SIZE.max(target_candidates);
        let mut offset = 0_usize;
        let mut pages_scanned = 0_usize;
        loop {
            pages_scanned = pages_scanned.saturating_add(1);
            let records = match query {
                Some(query) if !query.trim().is_empty() => {
                    store.search_records_page(query, page_size, offset)?
                }
                _ => Vec::new(),
            };
            let page_len = records.len();

            for record in records {
                if !seen.insert(record.id) {
                    continue;
                }
                if let Some(scope) = file_scope {
                    if !scope.history_record_ids.is_empty()
                        && !scope.history_record_ids.contains(&record.id)
                    {
                        continue;
                    }
                }
                if let Some(candidate) =
                    candidate_for_record(store, record, &terms, &options.filters, file_scope)?
                {
                    candidates.push(candidate);
                }
            }

            if candidates.len() >= target_candidates || page_len < page_size {
                break;
            }
            if pages_scanned >= FILTERED_SEARCH_MAX_PAGES {
                scan_budget_exhausted = true;
                break;
            }
            let next_offset = offset.saturating_add(page_size);
            if next_offset == offset {
                break;
            }
            offset = next_offset;
        }
    } else {
        let fetch_limit = target_candidates;
        let records = match query {
            Some(query) if !query.trim().is_empty() => store.search_records(query, fetch_limit)?,
            _ => Vec::new(),
        };
        for record in records {
            if !seen.insert(record.id) {
                continue;
            }
            if file_scope.is_some_and(|scope| !scope.history_record_ids.contains(&record.id)) {
                continue;
            }
            if let Some(candidate) =
                candidate_for_record(store, record, &terms, &options.filters, file_scope)?
            {
                candidates.push(candidate);
            }
        }
    }

    normalize_scores(&mut candidates);
    candidates.sort_by(compare_candidates);
    if candidates.len() > target_candidates {
        candidates.truncate(target_candidates);
    }
    Ok(CandidateSearch {
        candidates,
        scan_budget_exhausted,
    })
}

pub(crate) fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| right.record.updated_at.cmp(&left.record.updated_at))
        .then_with(|| left.record.title.cmp(&right.record.title))
        .then_with(|| left.record.id.cmp(&right.record.id))
}

pub(crate) fn candidate_for_record(
    store: &Store,
    record: HistoryRecord,
    terms: &[String],
    filters: &SearchFilters,
    file_scope: Option<&FileTouchScope>,
) -> Result<Option<Candidate>> {
    let context = hydrate_record_context(store, record.id, filters.file.as_deref())?;
    if !record_matches_filters(&record, &context, filters, file_scope) {
        return Ok(None);
    }
    let analysis = analyze_record(&record, &context, terms, filters);
    if terms.is_empty() || analysis.score > 0.0 {
        Ok(Some(Candidate {
            record,
            context,
            score: analysis.score,
            why_matched: analysis.why_matched,
            citations: analysis.citations,
            primary_hit: analysis.primary_hit,
        }))
    } else {
        Ok(None)
    }
}

pub(crate) fn hydrate_record_context(
    store: &Store,
    record_id: Uuid,
    file_filter: Option<&str>,
) -> Result<RecordContext> {
    let sessions = store.sessions_for_record(record_id)?;
    let runs = store.runs_for_record(record_id)?;
    let events = store.events_for_record(record_id)?;
    let artifacts = store.artifacts_for_record(record_id)?;
    let files_touched =
        if let Some(file) = file_filter.map(str::trim).filter(|value| !value.is_empty()) {
            store.files_touched_for_record_matching(record_id, file)?
        } else {
            store.files_touched_for_record(record_id)?
        };
    let vcs_changes = store.vcs_changes_for_record(record_id)?;
    let summaries = store.summaries_for_record(record_id)?;
    let mut source_ids = BTreeSet::new();
    for session in &sessions {
        if let Some(id) = session.capture_source_id {
            source_ids.insert(id);
        }
    }
    for run in &runs {
        if let Some(id) = run.source_id {
            source_ids.insert(id);
        }
    }
    for event in &events {
        if let Some(id) = event.capture_source_id {
            source_ids.insert(id);
        }
    }
    for artifact in &artifacts {
        if let Some(id) = artifact.source_id {
            source_ids.insert(id);
        }
    }
    for file in &files_touched {
        if let Some(id) = file.source_id {
            source_ids.insert(id);
        }
    }
    for change in &vcs_changes {
        if let Some(id) = change.source_id {
            source_ids.insert(id);
        }
    }
    for summary in &summaries {
        if let Some(id) = summary.source_id {
            source_ids.insert(id);
        }
    }
    let mut sources = BTreeMap::new();
    for source_id in source_ids {
        if let Ok(source) = store.get_capture_source(source_id) {
            sources.insert(source_id, source);
        }
    }

    Ok(RecordContext {
        sessions,
        runs,
        events,
        artifacts,
        files_touched,
        vcs_changes,
        summaries,
        sources,
    })
}

pub(crate) fn normalize_scores(candidates: &mut [Candidate]) {
    let max_score = candidates
        .iter()
        .map(|candidate| candidate.score)
        .fold(0.0_f32, f32::max);
    if max_score <= 0.0 {
        return;
    }
    for candidate in candidates {
        candidate.score = (candidate.score / max_score).clamp(0.0, 1.0);
    }
}
