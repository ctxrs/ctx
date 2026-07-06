use crate::*;

pub(crate) struct MatchAnalysis {
    pub(crate) score: f32,
    pub(crate) why_matched: Vec<String>,
    pub(crate) citations: Vec<ContextCitation>,
    pub(crate) primary_hit: Option<HitMetadata>,
}

pub(crate) fn analyze_record(
    record: &HistoryRecord,
    context: &RecordContext,
    terms: &[String],
    filters: &SearchFilters,
) -> MatchAnalysis {
    let mut score = 0.0_f32;
    let mut why = Vec::new();
    let mut citations = Vec::new();

    if terms.is_empty() {
        if filters
            .file
            .as_ref()
            .is_some_and(|file| !file.trim().is_empty())
        {
            let mut primary_hit = None;
            for section in search_sections(record, context, filters)
                .into_iter()
                .filter(|section| section.reason == "file_touched")
            {
                if primary_hit.is_none() {
                    primary_hit = Some(section.hit.clone());
                }
                score += section.weight;
                add_match(
                    &mut why,
                    &mut citations,
                    section.reason,
                    section.citation,
                    &section.hit,
                );
            }
            if !why.is_empty() {
                return MatchAnalysis {
                    score,
                    why_matched: why,
                    citations,
                    primary_hit,
                };
            }
        }
        add_match(
            &mut why,
            &mut citations,
            "recent_activity",
            ContextCitation {
                citation_type: ContextCitationType::HistoryRecord,
                id: record.id,
                label: "recent session".to_owned(),
                time: record.updated_at,
                provider: None,
                session_id: None,
                event_seq: None,
                raw_source_path: None,
                raw_source_exists: None,
                cursor: None,
            },
            &empty_hit(record.updated_at),
        );
        return MatchAnalysis {
            score: 1.0,
            why_matched: why,
            citations,
            primary_hit: None,
        };
    }

    let mut primary_hit = None;
    let mut primary_weight = f32::MIN;
    for section in search_sections(record, context, filters) {
        if hit_matches_excluded_provider_session(&section.hit, filters) {
            continue;
        }
        if matches_terms(&section.text, terms) {
            score += section.weight;
            if section.weight > primary_weight {
                primary_weight = section.weight;
                primary_hit = Some(section.hit.clone());
            }
            add_match(
                &mut why,
                &mut citations,
                section.reason,
                section.citation,
                &section.hit,
            );
        }
    }

    MatchAnalysis {
        score,
        why_matched: why,
        citations,
        primary_hit,
    }
}

pub(crate) fn add_match(
    why: &mut Vec<String>,
    citations: &mut Vec<ContextCitation>,
    reason: &str,
    mut citation: ContextCitation,
    hit: &HitMetadata,
) {
    if !why.iter().any(|value| value == reason) {
        why.push(reason.to_owned());
    }
    citation.provider = hit.provider;
    citation.session_id = hit.session_id;
    citation.event_seq = hit.event_seq;
    citation.raw_source_path = hit.raw_source_path.clone();
    citation.raw_source_exists = hit.raw_source_exists;
    citation.cursor = hit.cursor.clone().or_else(|| {
        hit.provider_session_id
            .as_ref()
            .map(|session_id| format!("session:{session_id}"))
    });
    if !citations.iter().any(|existing| {
        existing.citation_type == citation.citation_type && existing.id == citation.id
    }) {
        citations.push(citation);
    }
}

pub(crate) fn search_sections(
    record: &HistoryRecord,
    context: &RecordContext,
    filters: &SearchFilters,
) -> Vec<SearchSection> {
    let mut sections = Vec::new();
    let record_hit = record_context_display_hit(context, filters, record.updated_at);
    let include_record_bookkeeping_text = !is_agent_history_bookkeeping_record(record);
    if include_record_bookkeeping_text {
        sections.push(SearchSection {
            reason: "title",
            weight: 8.0,
            text: record.title.clone(),
            citation: citation(
                ContextCitationType::HistoryRecord,
                record.id,
                "session title",
                record.updated_at,
            ),
            hit: record_hit.clone(),
        });
    }
    let include_record_text = include_record_bookkeeping_text
        && record_text_matches_agent_scope(context, filters)
        && !context_has_excluded_provider_session(context, filters);
    if include_record_text {
        sections.push(SearchSection {
            reason: "primary_user_message",
            weight: 5.0,
            text: record.body.clone(),
            citation: citation(
                ContextCitationType::HistoryRecord,
                record.id,
                "session text",
                record.updated_at,
            ),
            hit: record_hit.clone(),
        });
    }
    if include_record_text {
        for tag in &record.tags {
            sections.push(SearchSection {
                reason: "tag",
                weight: 3.0,
                text: tag.clone(),
                citation: citation(
                    ContextCitationType::HistoryRecord,
                    record.id,
                    "session tag",
                    record.updated_at,
                ),
                hit: record_hit.clone(),
            });
        }
    }
    for session in &context.sessions {
        if !session_matches_agent_scope(session, filters)
            || !source_id_matches_history_source_filter(session.capture_source_id, context, filters)
        {
            continue;
        }
        let hit = session_hit(session, context);
        sections.push(SearchSection {
            reason: "session_metadata",
            weight: 2.5,
            text: joined([
                session.provider.as_str(),
                session.agent_type.as_str(),
                session.status.as_str(),
                session.external_session_id.as_deref().unwrap_or_default(),
                session.external_agent_id.as_deref().unwrap_or_default(),
                session.role_hint.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::Session,
                session.id,
                "session",
                session.started_at,
            ),
            hit,
        });
    }

    for run in &context.runs {
        if !item_matches_agent_scope(run.session_id, run.source_id, context, filters) {
            continue;
        }
        let hit = run_hit(run, context);
        sections.push(SearchSection {
            reason: "run_command",
            weight: if run.exit_code.unwrap_or(0) == 0 {
                3.0
            } else {
                4.0
            },
            text: joined([
                run.run_type.as_str(),
                run.status.as_str(),
                run.cwd.as_deref().unwrap_or_default(),
                run.command_preview.as_deref().unwrap_or_default(),
            ]),
            citation: citation(
                ContextCitationType::Run,
                run.id,
                "run command",
                run.started_at,
            ),
            hit,
        });
    }

    for event in &context.events {
        if !item_matches_agent_scope(event.session_id, event.capture_source_id, context, filters) {
            continue;
        }
        let event_text = event_text(event);
        let hit = event_hit(event, context);
        sections.push(SearchSection {
            reason: match event.event_type {
                ctx_history_core::EventType::Message => "message",
                ctx_history_core::EventType::ToolCall => "tool_call",
                ctx_history_core::EventType::ToolOutput => "tool_output",
                ctx_history_core::EventType::CommandStarted
                | ctx_history_core::EventType::CommandOutput
                | ctx_history_core::EventType::CommandFinished => "command_event",
                _ => "event",
            },
            weight: event_weight(event),
            text: event_text,
            citation: citation(
                ContextCitationType::Event,
                event.id,
                "event",
                event.occurred_at,
            ),
            hit,
        });
    }

    for artifact in &context.artifacts {
        if !item_matches_agent_scope(None, artifact.source_id, context, filters) {
            continue;
        }
        let hit = artifact_hit(artifact, context);
        sections.push(SearchSection {
            reason: "artifact",
            weight: 2.5,
            text: joined([
                artifact.kind.as_str(),
                artifact.media_type.as_deref().unwrap_or_default(),
                artifact.preview_text.as_deref().unwrap_or_default(),
                artifact.blob_path.as_str(),
            ]),
            citation: citation(
                ContextCitationType::Artifact,
                artifact.id,
                "artifact",
                artifact.timestamps.updated_at,
            ),
            hit,
        });
    }

    for file in &context.files_touched {
        let session_id = file.event_id.and_then(|id| {
            context
                .events
                .iter()
                .find(|event| event.id == id)
                .and_then(|event| event.session_id)
        });
        if !item_matches_agent_scope(session_id, file.source_id, context, filters) {
            continue;
        }
        let hit = file_hit(file, context);
        sections.push(SearchSection {
            reason: "file_touched",
            weight: 3.0,
            text: file_touched_search_text(file),
            citation: citation(
                ContextCitationType::File,
                file.id,
                "file touched",
                file.timestamps.updated_at,
            ),
            hit,
        });
    }

    for change in &context.vcs_changes {
        if !item_matches_agent_scope(None, change.source_id, context, filters) {
            continue;
        }
        let parent_change_ids = change.parent_change_ids.join(" ");
        let hit = source_hit(
            change.source_id,
            change.author_time.unwrap_or(change.timestamps.updated_at),
            context,
        );
        sections.push(SearchSection {
            reason: "vcs_change",
            weight: 3.0,
            text: joined([
                change.kind.as_str(),
                change.change_id.as_str(),
                change.branch_or_bookmark.as_deref().unwrap_or_default(),
                change.tree_hash.as_deref().unwrap_or_default(),
                parent_change_ids.as_str(),
            ]),
            citation: citation(
                ContextCitationType::VcsChange,
                change.id,
                "vcs change",
                change.author_time.unwrap_or(change.timestamps.updated_at),
            ),
            hit,
        });
    }

    for summary in &context.summaries {
        if !item_matches_agent_scope(None, summary.source_id, context, filters) {
            continue;
        }
        let hit = source_hit(summary.source_id, summary.timestamps.updated_at, context);
        sections.push(SearchSection {
            reason: "summary",
            weight: 4.0,
            text: summary.text.clone(),
            citation: citation(
                ContextCitationType::Summary,
                summary.id,
                "summary",
                summary.timestamps.updated_at,
            ),
            hit,
        });
    }

    sections
}

pub(crate) fn is_agent_history_bookkeeping_record(record: &HistoryRecord) -> bool {
    record.kind == "agent_history"
        || record.tags.iter().any(|tag| tag == "agent-history")
        || record
            .body
            .trim_start()
            .starts_with("Indexed local agent history from ")
        || record
            .body
            .trim_start()
            .starts_with("Indexed custom agent history from ")
}

pub(crate) fn file_touched_search_text(file: &FileTouched) -> String {
    let path = file.path.as_str();
    let old_path = file.old_path.as_deref().unwrap_or_default();
    joined([
        path,
        old_path,
        file.change_kind
            .map(|kind| kind.as_str())
            .unwrap_or_default(),
    ])
}

pub(crate) fn citation(
    citation_type: ContextCitationType,
    id: Uuid,
    label: &str,
    time: chrono::DateTime<Utc>,
) -> ContextCitation {
    ContextCitation {
        citation_type,
        id,
        label: label.to_owned(),
        time,
        provider: None,
        session_id: None,
        event_seq: None,
        raw_source_path: None,
        raw_source_exists: None,
        cursor: None,
    }
}
