use super::*;

#[test]
#[ignore = "manual perf benchmark; private release gates run scripts/public-ctx/perf-smoke.sh from ctx-private"]
fn synthetic_search_perf_records_thresholded_evidence() {
    let out_dir = std::env::var_os("CTX_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("target/ctx-artifacts/synthetic_search_perf")
        });
    std::fs::create_dir_all(&out_dir).unwrap();
    let artifact_path = out_dir.join("synthetic-search-perf.json");

    let event_count = perf_event_count();
    let events_per_record = perf_events_per_record();
    let search_repeats = perf_repeats("CTX_SEARCH_PERF_SEARCH_REPEATS", 9);
    let filtered_search_repeats = perf_repeats("CTX_SEARCH_PERF_FILTERED_SEARCH_REPEATS", 5);
    let thresholds = perf_thresholds(event_count);

    let generation_started = std::time::Instant::now();
    let archive = synthetic_perf_archive(event_count, events_per_record);
    let generation_ms = elapsed_ms(generation_started.elapsed());
    let corpus = PerfCorpus {
        records: archive.records.len(),
        capture_sources: archive.capture_sources.len(),
        sessions: archive.sessions.len(),
        runs: archive.runs.len(),
        events: archive.events.len(),
        summaries: archive.summaries.len(),
        files_touched: archive.files_touched.len(),
    };

    let (_temp, mut store) = test_store();
    let import_started = std::time::Instant::now();
    store.import_archive(&archive, false).unwrap();
    let import_ms = elapsed_ms(import_started.elapsed());
    let import_secs = (import_ms / 1000.0).max(0.001);
    let import_events_per_sec = corpus.events as f64 / import_secs;

    let search_options = PacketOptions {
        limit: 24,
        snippet_chars: 320,
        filters: SearchFilters::default(),
        result_mode: SearchResultMode::Sessions,
    };
    let filtered_search_options = PacketOptions {
        limit: 24,
        snippet_chars: 320,
        filters: SearchFilters {
            provider: Some(CaptureProvider::Codex),
            repo: Some("ctx".into()),
            event_type: Some(EventType::ToolCall),
            file: Some("perf_profile.rs".into()),
            ..SearchFilters::default()
        },
        result_mode: SearchResultMode::Sessions,
    };

    let search_warmup = search_packet(&store, "perfneedle", &search_options).unwrap();
    assert_perf_results("search warmup", search_warmup.results.len());
    let filtered_search_warmup =
        search_packet(&store, "perfneedle", &filtered_search_options).unwrap();
    assert_perf_results(
        "filtered search warmup",
        filtered_search_warmup.results.len(),
    );

    let mut search_samples = Vec::new();
    let mut last_search_results = 0;
    let mut last_search_citations = 0;
    for _ in 0..search_repeats {
        let started = std::time::Instant::now();
        let packet = search_packet(&store, "perfneedle", &search_options).unwrap();
        let elapsed = elapsed_ms(started.elapsed());
        assert_perf_results("search sample", packet.results.len());
        last_search_results = packet.results.len();
        last_search_citations = packet
            .results
            .iter()
            .map(|result| result.citations.len())
            .sum();
        search_samples.push(elapsed);
    }

    let mut filtered_search_samples = Vec::new();
    let mut last_filtered_search_results = 0;
    let mut last_filtered_search_citations = 0;
    for _ in 0..filtered_search_repeats {
        let started = std::time::Instant::now();
        let packet = search_packet(&store, "perfneedle", &filtered_search_options).unwrap();
        let elapsed = elapsed_ms(started.elapsed());
        assert_perf_results("filtered search sample", packet.results.len());
        last_filtered_search_results = packet.results.len();
        last_filtered_search_citations = packet
            .results
            .iter()
            .map(|result| result.citations.len())
            .sum();
        filtered_search_samples.push(elapsed);
    }

    let db_path = store.path().to_path_buf();
    drop(store);
    let db_bytes = sqlite_footprint_bytes(&db_path);
    let main_db_bytes = std::fs::metadata(&db_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    let import_stats = timing_stats(&[import_ms]);
    let search_stats = timing_stats(&search_samples);
    let filtered_search_stats = timing_stats(&filtered_search_samples);
    let max_db_bytes = thresholds.max_db_bytes_per_event * corpus.events as u64;
    let checks = vec![
        serde_json::json!({
            "name": "corpus_events_at_least_10000",
            "passed": corpus.events >= 10_000,
            "actual": corpus.events,
            "threshold": 10_000
        }),
        serde_json::json!({
            "name": "import_events_per_sec",
            "passed": import_events_per_sec >= thresholds.import_min_events_per_sec,
            "actual": rounded(import_events_per_sec),
            "threshold": thresholds.import_min_events_per_sec
        }),
        serde_json::json!({
            "name": "search_p95_ms",
            "passed": search_stats.p95_ms <= thresholds.search_p95_ms,
            "actual": search_stats.p95_ms,
            "threshold": thresholds.search_p95_ms
        }),
        serde_json::json!({
            "name": "filtered_search_p95_ms",
            "passed": filtered_search_stats.p95_ms <= thresholds.filtered_search_p95_ms,
            "actual": filtered_search_stats.p95_ms,
            "threshold": thresholds.filtered_search_p95_ms
        }),
        serde_json::json!({
            "name": "db_footprint_bytes",
            "passed": db_bytes <= max_db_bytes,
            "actual": db_bytes,
            "threshold": max_db_bytes
        }),
    ];
    let passed = checks
        .iter()
        .all(|check| check["passed"].as_bool().unwrap_or(false));

    let artifact = serde_json::json!({
        "schema_version": 1,
        "profile": "synthetic-search-perf",
        "mode": if event_count >= 100_000 { "slow" } else { "standard" },
        "status": if passed { "passed" } else { "failed" },
        "corpus": {
            "records": corpus.records,
            "capture_sources": corpus.capture_sources,
            "sessions": corpus.sessions,
            "runs": corpus.runs,
            "events": corpus.events,
            "summaries": corpus.summaries,
            "files_touched": corpus.files_touched,
            "events_per_record": events_per_record,
            "query": "perfneedle"
        },
        "thresholds": {
            "import_min_events_per_sec": thresholds.import_min_events_per_sec,
            "search_p95_ms": thresholds.search_p95_ms,
            "filtered_search_p95_ms": thresholds.filtered_search_p95_ms,
            "max_db_bytes_per_event": thresholds.max_db_bytes_per_event,
            "env_overrides": [
                "CTX_SEARCH_PERF_IMPORT_MIN_EVENTS_PER_SEC",
                "CTX_SEARCH_PERF_SEARCH_P95_MS",
                "CTX_SEARCH_PERF_FILTERED_SEARCH_P95_MS",
                "CTX_SEARCH_PERF_MAX_DB_BYTES_PER_EVENT"
            ]
        },
        "profiles": {
            "generation": {
                "duration_ms": generation_ms
            },
            "import": {
                "timings": import_stats.to_json(),
                "events_per_sec": rounded(import_events_per_sec)
            },
            "search": {
                "timings": search_stats.to_json(),
                "result_count": last_search_results,
                "citation_count": last_search_citations,
                "repeats": search_repeats
            },
            "filtered_search": {
                "timings": filtered_search_stats.to_json(),
                "result_count": last_filtered_search_results,
                "citation_count": last_filtered_search_citations,
                "repeats": filtered_search_repeats
            }
        },
        "storage": {
            "main_db_bytes": main_db_bytes,
            "db_footprint_bytes": db_bytes,
            "db_bytes_per_event": rounded(db_bytes as f64 / corpus.events as f64)
        },
        "checks": checks
    });

    std::fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    )
    .unwrap();
    println!(
        "synthetic search perf artifact: {}",
        artifact_path.display()
    );

    assert!(
        passed,
        "synthetic search perf thresholds failed; see {}",
        artifact_path.display()
    );
}
