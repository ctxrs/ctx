use super::*;

#[test]
fn retained_rows_stream_in_pages_bounded_by_64_units_and_8_mib() {
    let mut contents = session_meta("paged-owner");
    for index in 0..5_001 {
        contents.push_str(&message("assistant", &format!("bounded-row-{index}")));
    }
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "paged-owner"), None);

    assert_eq!(sink.rows.len(), 5_001);
    assert_eq!(sink.pages.len(), 79);
    assert!(sink
        .pages
        .iter()
        .all(|(units, bytes)| *units <= 64 && *bytes <= MAX_CODEX_PAGE_BYTES));
    assert_eq!(scan.counters.retained_records, 5_001);
    assert_eq!(scan.counters.emitted_pages, 79);
    assert_eq!(scan.counters.peak_page_rows, MAX_CODEX_PAGE_ROWS);
    assert!(scan.counters.peak_page_bytes <= MAX_CODEX_PAGE_BYTES);
    assert_eq!(scan.next_raw_ordinal, 5_002);
}

#[test]
fn records_over_16_mib_are_stream_skipped_without_losing_physical_ordinals() {
    let mut contents = session_meta("oversized-owner");
    contents.push_str(
        r#"{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"big-output","output":""#,
    );
    contents.push_str(&"x".repeat(MAX_CODEX_RECORD_BYTES));
    contents.push_str("\"}}\n");
    contents.push('{');
    contents.push_str(&"y".repeat(MAX_CODEX_RECORD_BYTES));
    contents.push_str("}\n");
    contents.push_str(&message("assistant", "survives oversized records"));

    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "oversized-owner"), None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].raw_ordinal, 3);
    assert_eq!(scan.next_raw_ordinal, 4);
    assert_eq!(scan.counters.complete_records, 4);
    assert_eq!(scan.counters.oversized_records, 2);
    assert_eq!(scan.counters.rejected_complete_records, 2);
    assert_eq!(scan.counters.peak_line_buffer_bytes, MAX_CODEX_RECORD_BYTES);
    assert_eq!(scan.counters.bytes_read, contents.len() as u64);
}

#[test]
#[ignore = "diagnostic release-mode benchmark over the 154 MB Codex fixture"]
fn source_backed_quickbench_guards_the_nativepath_parser_hot_path() {
    const EXPECTED_FILES: usize = 6_000;
    const EXPECTED_BYTES: u64 = 154_299_600;
    const EXPECTED_SHA256: &str =
        "b8558416ccb9719c5c8e0e3e1821ea94bef1e5c413a3070b9982fa759493e82b";
    const EXPECTED_ROWS: u64 = 24_000;
    const EXPECTED_RESULTS: u64 = 6_000;
    const EXPECTED_MALFORMED: u64 = 60;
    const EXPECTED_INCOMPLETE_TAILS: u64 = 60;

    let fixture_root = std::env::var_os("CTX_CODEX_QUICKBENCH_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/ctx-codex-nativepath-quickbench-v1"));
    let paths = quickbench_fixture_paths(&fixture_root);
    assert_eq!(paths.len(), EXPECTED_FILES);
    let (fixture_bytes, fixture_sha256) = quickbench_fixture_hash(&fixture_root, &paths);
    assert_eq!(fixture_bytes, EXPECTED_BYTES);
    assert_eq!(fixture_sha256, EXPECTED_SHA256);

    let catalog = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let mut session = catalog_session(path, &format!("quickbench-{index:06}"));
            session.source_root = fixture_root.display().to_string();
            session.agent_type = if index.is_multiple_of(10) {
                AgentType::Subagent
            } else {
                AgentType::Primary
            };
            session
        })
        .collect::<Vec<_>>();
    let discovery = discover_codex_catalog_sources(&catalog);
    assert!(discovery.rejections.is_empty(), "{discovery:?}");
    assert_eq!(discovery.sources.len(), EXPECTED_FILES);
    let sources = discovery.sources;

    let scan_once = || {
        let mut rows = 0_u64;
        let mut results = 0_u64;
        let mut malformed = 0_u64;
        let mut incomplete_tails = 0_u64;
        let mut structural_parses = 0_u64;
        let mut typed_parses = 0_u64;
        let mut structural_output_probes = 0_u64;
        for source in &sources {
            let mut scanner =
                CodexNativeScanner::new_source_backed_v0(source.clone(), None).unwrap();
            while let Some(page) = scanner.next_page().unwrap() {
                let CodexNativeOwnedPage::Core(page) = &page;
                rows = rows.saturating_add(page.source_backed_rows.len() as u64);
                black_box(page);
            }
            let scan = scanner.finish().unwrap();
            results = results.saturating_add(scan.counters.native_result_records);
            malformed = malformed.saturating_add(scan.counters.malformed_records);
            incomplete_tails =
                incomplete_tails.saturating_add(u64::from(scan.incomplete_tail.is_some()));
            structural_parses =
                structural_parses.saturating_add(scan.counters.structural_json_parses);
            typed_parses = typed_parses.saturating_add(scan.counters.typed_json_parses);
            structural_output_probes =
                structural_output_probes.saturating_add(scan.counters.structural_output_probes);
        }
        assert_eq!(rows, EXPECTED_ROWS);
        assert_eq!(results, EXPECTED_RESULTS);
        assert_eq!(malformed, EXPECTED_MALFORMED);
        assert_eq!(incomplete_tails, EXPECTED_INCOMPLETE_TAILS);
        assert_eq!(structural_parses, 36_060);
        assert_eq!(typed_parses, 30_000);
        assert_eq!(structural_output_probes, EXPECTED_RESULTS);
        black_box((
            rows,
            results,
            malformed,
            incomplete_tails,
            structural_parses,
            typed_parses,
        ));
    };

    scan_once();
    let mut samples = Vec::with_capacity(3);
    for _ in 0..3 {
        let started = Instant::now();
        scan_once();
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let median = samples[1];
    println!(
        "Codex source-backed NativePath median over {} bytes and {} sources: {:.3}s",
        EXPECTED_BYTES,
        EXPECTED_FILES,
        median.as_secs_f64()
    );
    assert!(
        median.as_secs_f64() < 1.0,
        "obvious NativePath parser regression from the recorded 0.468s behavior: {median:?}"
    );
}
