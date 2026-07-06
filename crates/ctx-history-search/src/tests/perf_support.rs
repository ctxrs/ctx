use super::*;

pub(crate) struct PerfCorpus {
    pub(crate) records: usize,
    pub(crate) capture_sources: usize,
    pub(crate) sessions: usize,
    pub(crate) runs: usize,
    pub(crate) events: usize,
    pub(crate) summaries: usize,
    pub(crate) files_touched: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct PerfThresholds {
    pub(crate) import_min_events_per_sec: f64,
    pub(crate) search_p95_ms: f64,
    pub(crate) filtered_search_p95_ms: f64,
    pub(crate) max_db_bytes_per_event: u64,
}

pub(crate) struct PerfTimingStats {
    pub(crate) samples_ms: Vec<f64>,
    pub(crate) p50_ms: f64,
    pub(crate) p95_ms: f64,
    pub(crate) min_ms: f64,
    pub(crate) max_ms: f64,
}

impl PerfTimingStats {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "sample_count": self.samples_ms.len(),
            "samples_ms": self.samples_ms,
            "p50_ms": self.p50_ms,
            "p95_ms": self.p95_ms,
            "min_ms": self.min_ms,
            "max_ms": self.max_ms
        })
    }
}

pub(crate) fn synthetic_perf_archive(
    event_count: usize,
    events_per_record: usize,
) -> SessionHistoryArchive {
    let mut archive = SessionHistoryArchive::default();
    let record_count = event_count.div_ceil(events_per_record);
    let workspace_id = perf_uuid(0x5000, 0);
    archive.vcs_workspaces.push(VcsWorkspace {
        id: workspace_id,
        kind: VcsKind::Git,
        root_path: "/workspace/ctx".into(),
        repo_fingerprint: "git:ctx-search-perf".into(),
        primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
        host: VcsHost::Github,
        owner: Some("ctxrs".into()),
        name: Some("ctx".into()),
        monorepo_subpath: None,
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    });

    for record_index in 0..record_count {
        let record_id = perf_uuid(0x1000, record_index as u64);
        let source_id = perf_uuid(0x1100, record_index as u64);
        let session_id = perf_uuid(0x2000, record_index as u64);
        let run_id = perf_uuid(0x3000, record_index as u64);
        let summary_id = perf_uuid(0x4000, record_index as u64);
        let file_id = perf_uuid(0x4100, record_index as u64);
        let time = fixed_time() + chrono::Duration::seconds(record_index as i64);

        let mut record = HistoryRecord::new(
            format!("Synthetic perf profile {record_index:05}"),
            format!(
                "perfneedle import search retrieval profile record {record_index:05}; \
                 routing storage ranking citations threshold evidence {}",
                "detail ".repeat(8)
            ),
            vec![
                "perf".into(),
                "synthetic".into(),
                format!("bucket-{:02}", record_index % 32),
            ],
            "task",
            Some("/workspace/ctx".into()),
        );
        record.id = record_id;
        record.created_at = time;
        record.updated_at = time;
        archive.records.push(record);

        archive.capture_sources.push(CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Codex,
                machine_id: "synthetic-perf-host".into(),
                process_id: None,
                cwd: Some("/workspace/ctx".into()),
                raw_source_path: Some(format!(
                    "/workspace/ctx/.ctx/synthetic/perf-session-{record_index:05}.jsonl"
                )),
                external_session_id: Some(format!("perf-session-{record_index:05}")),
            },
            started_at: time,
            ended_at: Some(time + chrono::Duration::seconds(events_per_record as i64)),
            sync: SyncMetadata {
                metadata: serde_json::json!({
                    "source_format": "synthetic_perf_jsonl",
                    "cursor": {
                        "after": {
                            "stream": "provider:codex:synthetic_perf_jsonl",
                            "cursor": format!("line:{}", record_index * events_per_record),
                            "observed_at": time.to_rfc3339()
                        }
                    }
                }),
                ..sync_metadata()
            },
        });

        archive.sessions.push(Session {
            id: session_id,
            history_record_id: Some(record_id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Codex,
            external_session_id: Some(format!("perf-session-{record_index:05}")),
            external_agent_id: Some(format!("agent-{record_index:05}")),
            agent_type: AgentType::Primary,
            role_hint: Some("implementation-worker".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: time,
            ended_at: Some(time + chrono::Duration::seconds(events_per_record as i64)),
            timestamps: EntityTimestamps {
                created_at: time,
                updated_at: time,
            },
            sync: sync_metadata(),
        });

        archive.runs.push(Run {
            id: run_id,
            history_record_id: Some(record_id),
            session_id: Some(session_id),
            run_type: RunType::Command,
            status: RunStatus::Succeeded,
            started_at: time,
            ended_at: Some(time + chrono::Duration::seconds(1)),
            exit_code: Some(0),
            cwd: Some("/workspace/ctx".into()),
            command_preview: Some(format!(
                "ctx search perfneedle --refresh off --limit 5 # synthetic record {record_index:05}"
            )),
            input_blob_id: None,
            output_blob_id: None,
            timestamps: EntityTimestamps {
                created_at: time,
                updated_at: time,
            },
            source_id: Some(source_id),
            sync: sync_metadata(),
        });

        archive.summaries.push(Summary {
            id: summary_id,
            history_record_id: Some(record_id),
            session_id: Some(session_id),
            kind: SummaryKind::ImportedProviderSummary,
            model_or_source: Some("synthetic-perf".into()),
            text: format!(
                "perfneedle summary for import search retrieval record {record_index:05}; \
                 captures commands, files, and citations"
            ),
            citations: Vec::new(),
            timestamps: EntityTimestamps {
                created_at: time,
                updated_at: time,
            },
            source_id: Some(source_id),
            sync: sync_metadata(),
        });

        archive.files_touched.push(FileTouched {
            id: file_id,
            history_record_id: Some(record_id),
            run_id: Some(run_id),
            event_id: None,
            vcs_workspace_id: Some(workspace_id),
            path: format!(
                "crates/perf/profile_{:02}/perf_profile.rs",
                record_index % 24
            ),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some((record_index % 17) as i64 - 3),
            confidence: Confidence::Explicit,
            timestamps: EntityTimestamps {
                created_at: time,
                updated_at: time,
            },
            source_id: Some(source_id),
            sync: sync_metadata(),
        });

        let event_start = record_index * events_per_record;
        let event_end = event_count.min(event_start + events_per_record);
        for event_index in event_start..event_end {
            let local_index = event_index - event_start;
            let event_time = time + chrono::Duration::milliseconds(local_index as i64);
            let event_type = match local_index % 5 {
                0 => EventType::ToolCall,
                1 => EventType::ToolOutput,
                2 => EventType::Message,
                3 => EventType::CommandOutput,
                _ => EventType::Notice,
            };
            let role = match event_type {
                EventType::Message => Some(EventRole::User),
                EventType::ToolOutput | EventType::CommandOutput => Some(EventRole::Tool),
                EventType::ToolCall => Some(EventRole::Assistant),
                _ => Some(EventRole::System),
            };
            let event_id = perf_uuid(0x6000, event_index as u64);
            archive.events.push(Event {
                id: event_id,
                seq: (event_index + 1) as u64,
                history_record_id: Some(record_id),
                session_id: Some(session_id),
                run_id: Some(run_id),
                event_type,
                role,
                occurred_at: event_time,
                capture_source_id: Some(source_id),
                payload: serde_json::json!({
                    "cursor": format!("line:{}", local_index + 1),
                    "body": {
                        "text": format!(
                            "perfneedle import search retrieval profile record {record_index:05} event {local_index:02} indexed event {event_index:06}"
                        )
                    }
                }),
                payload_blob_id: None,
                dedupe_key: (local_index == 0).then(|| {
                    format!("provider:codex:s{record_index:05}:{local_index}:h{event_index:06}")
                }),
                redaction_state: RedactionState::SafePreview,
                sync: sync_metadata(),
            });
        }
    }

    archive
}

pub(crate) fn perf_uuid(namespace: u16, index: u64) -> Uuid {
    Uuid::parse_str(&format!("018f45d0-{namespace:04x}-7000-8000-{index:012x}")).unwrap()
}

pub(crate) fn perf_event_count() -> usize {
    let requested = env_usize("CTX_SEARCH_PERF_EVENTS").unwrap_or_else(|| {
        if env_flag("CTX_SEARCH_PERF_SLOW") {
            100_000
        } else {
            10_000
        }
    });
    requested.max(10_000)
}

pub(crate) fn perf_events_per_record() -> usize {
    env_usize("CTX_SEARCH_PERF_EVENTS_PER_RECORD")
        .unwrap_or(50)
        .clamp(1, 50)
}

pub(crate) fn perf_repeats(name: &str, default: usize) -> usize {
    env_usize(name).unwrap_or(default).clamp(1, 50)
}

pub(crate) fn perf_thresholds(event_count: usize) -> PerfThresholds {
    let slow = event_count >= 100_000;
    PerfThresholds {
        import_min_events_per_sec: env_f64("CTX_SEARCH_PERF_IMPORT_MIN_EVENTS_PER_SEC")
            .unwrap_or(if slow { 25.0 } else { 40.0 }),
        search_p95_ms: env_f64("CTX_SEARCH_PERF_SEARCH_P95_MS").unwrap_or(if slow {
            2_500.0
        } else {
            1_500.0
        }),
        filtered_search_p95_ms: env_f64("CTX_SEARCH_PERF_FILTERED_SEARCH_P95_MS")
            .unwrap_or(if slow { 8_000.0 } else { 5_000.0 }),
        max_db_bytes_per_event: env_u64("CTX_SEARCH_PERF_MAX_DB_BYTES_PER_EVENT")
            .unwrap_or(if slow { 10_240 } else { 12_288 }),
    }
}

pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "slow"
        )
    })
}

pub(crate) fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse().ok()
}

pub(crate) fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

pub(crate) fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.parse().ok()
}

pub(crate) fn assert_perf_results(label: &str, result_count: usize) {
    assert!(result_count > 0, "{label} returned no results");
}

pub(crate) fn elapsed_ms(duration: std::time::Duration) -> f64 {
    rounded(duration.as_secs_f64() * 1000.0)
}

pub(crate) fn timing_stats(samples: &[f64]) -> PerfTimingStats {
    assert!(!samples.is_empty(), "perf timing samples must not be empty");
    let mut sorted = samples.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    PerfTimingStats {
        samples_ms: samples.iter().copied().map(rounded).collect(),
        p50_ms: percentile_sorted(&sorted, 50.0),
        p95_ms: percentile_sorted(&sorted, 95.0),
        min_ms: rounded(*sorted.first().unwrap()),
        max_ms: rounded(*sorted.last().unwrap()),
    }
}

pub(crate) fn percentile_sorted(sorted: &[f64], percentile: f64) -> f64 {
    let rank = ((percentile / 100.0) * (sorted.len().saturating_sub(1) as f64)).ceil();
    rounded(sorted[rank as usize])
}

pub(crate) fn rounded(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub(crate) fn sqlite_footprint_bytes(path: &Path) -> u64 {
    let main = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    main + sqlite_sidecar_bytes(path, "-wal") + sqlite_sidecar_bytes(path, "-shm")
}

pub(crate) fn sqlite_sidecar_bytes(path: &Path, suffix: &str) -> u64 {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return 0;
    };
    let sidecar = path.with_file_name(format!("{file_name}{suffix}"));
    std::fs::metadata(sidecar)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}
