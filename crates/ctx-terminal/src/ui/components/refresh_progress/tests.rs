use super::*;
use crate::ui::{StreamKind, TestContext};

#[test]
fn positive_subsecond_duration_never_renders_as_zero() {
    assert_eq!(format_eta_duration_millis(1), "1s");
    assert_eq!(format_eta_duration_millis(999), "1s");
    assert_eq!(format_eta_duration_millis(1_000), "1s");
    assert_eq!(format_eta_duration_millis(1_001), "2s");
}

fn active_status(
    logical_phase: RefreshLogicalPhase,
    physical_phase: &str,
    known: bool,
    total: u64,
) -> RefreshProgressSnapshot {
    RefreshProgressSnapshot::new(
        Some("logical-request".to_owned()),
        RefreshStatusKind::Logical(RefreshLogicalStatus {
            request_state: RefreshRequestState::Running,
            logical_phase,
            physical_attempt_id: "physical-attempt".to_owned(),
            physical_attempt_state: RefreshRequestState::Running,
            progress_owner_request_id: "progress-owner".to_owned(),
            progress_owner_attempt_state: RefreshRequestState::Running,
            structured_outcome: None,
        }),
        RefreshProgress {
            phase: physical_phase.to_owned(),
            completed_sources: 0,
            total_sources: total,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            whole_run_stage: match physical_phase {
                "queued" | "pending" | "discovering" => RefreshWholeRunStage::Preparing,
                "committing" => RefreshWholeRunStage::Merging,
                "committed" | "publishing" => RefreshWholeRunStage::Activation,
                _ => RefreshWholeRunStage::Reading,
            },
            ..Default::default()
        },
        known,
    )
}

fn terminal_status(
    state: RefreshRequestState,
    code: &str,
    class: &str,
    presentation: RefreshTerminalPresentation,
) -> RefreshProgressSnapshot {
    RefreshProgressSnapshot::new(
        Some("logical-request".to_owned()),
        RefreshStatusKind::Logical(RefreshLogicalStatus {
            request_state: state,
            logical_phase: RefreshLogicalPhase::Terminal,
            physical_attempt_id: "physical-attempt".to_owned(),
            physical_attempt_state: state,
            progress_owner_request_id: "physical-attempt".to_owned(),
            progress_owner_attempt_state: state,
            structured_outcome: Some(Box::new(RefreshStructuredOutcome {
                code: code.to_owned(),
                class: class.to_owned(),
                retryable: false,
                affected_routes: Vec::new(),
                retryable_routes: Vec::new(),
                blocked_routes: Vec::new(),
                physical_attempt_id: "physical-attempt".to_owned(),
                retained_generation: None,
                published_generation: None,
                retry_advice: None,
                detail: None,
                presentation,
            })),
        }),
        RefreshProgress {
            phase: "committed".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            ..Default::default()
        },
        true,
    )
}

#[test]
fn full_status_adapter_preserves_logical_phases_and_physical_owner() {
    for (phase, expected) in [
        (RefreshLogicalPhase::Waiting, "History refresh is waiting"),
        (
            RefreshLogicalPhase::Attached,
            "Refreshing history with shared work",
        ),
        (
            RefreshLogicalPhase::CoverageCheck,
            "Checking refresh coverage",
        ),
        (
            RefreshLogicalPhase::ExactSuccessor,
            "Waiting for successor refresh",
        ),
    ] {
        let snapshot = active_status(phase, "committed", true, 2);
        assert_eq!(machine_refresh_label(&snapshot), expected);
        assert!(!snapshot.is_terminal(), "logical phase {phase:?}");
        let logical = match snapshot.kind() {
            RefreshStatusKind::Logical(logical) => logical,
            other => panic!("unexpected status kind: {other:?}"),
        };
        assert_eq!(logical.physical_attempt_id, "physical-attempt");
        assert_eq!(logical.progress_owner_request_id, "progress-owner");
    }
}

#[test]
fn human_progress_never_exposes_route_counts() {
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let known = active_status(RefreshLogicalPhase::Direct, "discovering", true, 0);
    let unknown = active_status(RefreshLogicalPhase::Direct, "discovering", false, 0);
    for rendered in [
        refresh_progress(&context, &known).render_plain(),
        refresh_progress(&context, &unknown).render_plain(),
    ] {
        assert!(!rendered.contains("Sources"), "{rendered}");
        assert!(!rendered.contains("0 / 0"), "{rendered}");
        assert!(
            rendered.contains("Agent histories  discovering"),
            "{rendered}"
        );
    }
}

#[test]
fn setup_live_history_progress_is_stable_aligned_and_user_facing() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut snapshot = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 4);
    snapshot.progress.agent_histories =
        vec!["Codex".to_owned(), "Claude".to_owned(), "Gemini".to_owned()];
    snapshot.progress.processed_sessions = 1_123;
    snapshot.progress.processed_messages = 72_456;
    snapshot.progress.processed_tool_calls = 31_009;
    snapshot.progress.processed_bytes = 8_804_683_776;
    snapshot.progress.elapsed_millis = Some(125_000);
    snapshot.set_presentation_agent_histories(Some(snapshot.progress.agent_histories.clone()));
    snapshot.use_setup_live_presentation();
    let rendered = refresh_progress(&context, &snapshot).render_plain();
    assert_eq!(
        rendered,
        concat!(
            "Reading your agent history\n",
            "──────────────────────────────━━━━━━━━──────────\n",
            "\n",
            "Agent histories      Codex\n",
            "                     Claude\n",
            "                     Gemini\n",
            "\n",
            "Sessions             1,123\n",
            "Messages             72,456\n",
            "Tool calls           31,009\n",
            "Data scanned         8.2 GiB\n",
            "Elapsed              2m 05s\n",
            "Estimated remaining  Estimating\n",
        )
    );
    for internal in ["Logical", "Physical", "owner", "Source", "3 / 4"] {
        assert!(!rendered.contains(internal), "{rendered}");
    }
}

#[test]
fn setup_live_history_progress_is_responsive_at_supported_terminal_widths() {
    let mut snapshot = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 4);
    snapshot.progress.agent_histories =
        vec!["Codex".to_owned(), "Claude".to_owned(), "Gemini".to_owned()];
    snapshot.progress.processed_sessions = 1_123;
    snapshot.progress.processed_messages = 72_456;
    snapshot.progress.processed_tool_calls = 31_009;
    snapshot.progress.processed_bytes = 8_804_683_776;
    snapshot.progress.elapsed_millis = Some(125_000);
    snapshot.set_presentation_agent_histories(Some(snapshot.progress.agent_histories.clone()));
    snapshot.use_setup_live_presentation();

    for width in [32, 48, 80, 120] {
        let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, width));
        let rendered = refresh_progress(&context, &snapshot).render_plain();
        let lines = rendered.lines().collect::<Vec<_>>();

        assert!(
            lines.iter().all(|line| line.chars().count() <= width),
            "width={width} rendered={rendered:?}"
        );
        assert_eq!(
            lines[1].chars().count(),
            context
                .content_width()
                .unwrap_or(width)
                .min(MAX_PROGRESS_BAR_WIDTH),
            "width={width} rendered={rendered:?}"
        );
        for value in [
            "Codex",
            "Claude",
            "Gemini",
            "1,123",
            "72,456",
            "31,009",
            "8.2 GiB",
            "2m 05s",
            "Estimating",
        ] {
            assert_eq!(
                rendered.matches(value).count(),
                1,
                "width={width} value={value:?} rendered={rendered:?}"
            );
        }

        if width >= 48 {
            assert!(
                rendered.contains("Agent histories      Codex"),
                "{rendered}"
            );
            assert!(
                rendered.contains("Sessions             1,123"),
                "{rendered}"
            );
        } else {
            assert!(rendered.contains("Agent histories\n  Codex"), "{rendered}");
            assert!(rendered.contains("Sessions\n  1,123"), "{rendered}");
        }
    }
}

#[test]
fn provider_discovery_changes_height_once_then_keeps_it_stable() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut discovery = active_status(RefreshLogicalPhase::Direct, "discovering", true, 4);
    discovery.progress.agent_histories = vec!["Codex".to_owned()];
    discovery.set_presentation_agent_histories(None);
    discovery.use_setup_live_presentation();
    let discovery_height = refresh_progress(&context, &discovery)
        .render_plain()
        .lines()
        .count();

    let mut active = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 4);
    let frozen = vec!["Codex".to_owned(), "Claude".to_owned(), "Gemini".to_owned()];
    active.progress.agent_histories = frozen.clone();
    active.set_presentation_agent_histories(Some(frozen.clone()));
    active.use_setup_live_presentation();
    let active_height = refresh_progress(&context, &active)
        .render_plain()
        .lines()
        .count();

    active
        .progress
        .agent_histories
        .push("Late provider".to_owned());
    active.progress.processed_sessions = 12_345;
    active.set_presentation_agent_histories(Some(frozen));
    let updated_height = refresh_progress(&context, &active)
        .render_plain()
        .lines()
        .count();

    assert_eq!(discovery_height, 2);
    assert!(active_height > discovery_height);
    assert_eq!(updated_height, active_height);
}

#[test]
fn local_elapsed_changes_bar_and_elapsed_without_changing_backend_counters() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut snapshot = active_status(RefreshLogicalPhase::Direct, "verifying", true, 4);
    snapshot.progress.agent_histories = vec!["Codex".to_owned()];
    snapshot.progress.processed_sessions = 7;
    snapshot.set_presentation_agent_histories(Some(vec!["Codex".to_owned()]));
    snapshot.use_setup_live_presentation();
    snapshot.advance_presentation_clock(900);
    let first = refresh_progress(&context, &snapshot).render_plain();
    snapshot.advance_presentation_clock(1_100);
    let second = refresh_progress(&context, &snapshot).render_plain();

    assert_ne!(first.lines().nth(1), second.lines().nth(1));
    assert!(first.contains("Elapsed              0s"), "{first}");
    assert!(second.contains("Elapsed              1s"), "{second}");
    assert!(second.contains("Sessions             7"), "{second}");
}

#[test]
fn presentation_eta_honors_usefulness_floor_and_terminal_state() {
    for (remaining, expected) in [(2_101, Some(2_001)), (2_100, None), (2_099, None)] {
        let mut snapshot = active_status(RefreshLogicalPhase::Direct, "verifying", true, 4);
        snapshot.progress.elapsed_millis = Some(1_000);
        snapshot.progress.estimated_remaining_millis = Some(remaining);

        snapshot.advance_presentation_clock(1_100);

        assert_eq!(snapshot.estimated_remaining_millis(), expected);
    }

    let mut terminal = terminal_status(
        RefreshRequestState::Published,
        "completed",
        "completed",
        RefreshTerminalPresentation::Complete,
    );
    terminal.progress.elapsed_millis = Some(1_000);
    terminal.progress.estimated_remaining_millis = Some(2_100);

    terminal.advance_presentation_clock(1_100);

    assert_eq!(terminal.progress.elapsed_millis, Some(1_000));
    assert_eq!(terminal.estimated_remaining_millis(), Some(2_100));
}

#[test]
fn setup_terminal_reconciles_empty_refresh_counters_with_committed_history() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut terminal = terminal_status(
        RefreshRequestState::Published,
        "completed",
        "completed",
        RefreshTerminalPresentation::Complete,
    );
    terminal.set_presentation_agent_histories(Some(vec!["Codex".to_owned()]));
    terminal.set_terminal_history_totals(2, 5, 1, 2_346);
    terminal.use_setup_live_presentation();

    let rendered = refresh_progress(&context, &terminal).render_plain();
    assert!(rendered.contains("History refresh complete"), "{rendered}");
    assert!(rendered.contains("Sessions             2"), "{rendered}");
    assert!(rendered.contains("Messages             5"), "{rendered}");
    assert!(rendered.contains("Tool calls           1"), "{rendered}");
    assert!(
        rendered.contains("Data scanned         2.3 KiB"),
        "{rendered}"
    );
}

#[test]
fn indeterminate_bar_moves_one_cell_per_tick_and_reverses_at_edges() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    assert_eq!(indeterminate_position(&context, 0), 0);
    assert_eq!(indeterminate_position(&context, 100), 1);
    assert_eq!(indeterminate_position(&context, 4_000), 40);
    assert_eq!(indeterminate_position(&context, 4_100), 39);
    assert_eq!(indeterminate_position(&context, 8_000), 0);
}

#[test]
fn setup_live_maps_every_whole_run_stage_truthfully() {
    for (stage, expected) in [
        (RefreshWholeRunStage::Preparing, "Preparing your history"),
        (RefreshWholeRunStage::Reading, "Reading your agent history"),
        (RefreshWholeRunStage::Merging, "Merging search index"),
        (RefreshWholeRunStage::Syncing, "Syncing search index"),
        (
            RefreshWholeRunStage::PhysicalVerification,
            "Verifying search index files",
        ),
        (
            RefreshWholeRunStage::LogicalVerification,
            "Verifying indexed history",
        ),
        (RefreshWholeRunStage::Activation, "Activating search index"),
        (RefreshWholeRunStage::Complete, "History refresh complete"),
        (RefreshWholeRunStage::Failed, "History refresh failed"),
    ] {
        let mut snapshot = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 1);
        snapshot.progress.whole_run_stage = stage;
        assert_eq!(human_refresh_label(&snapshot), expected);
    }
}

#[test]
fn transient_ingestion_activity_overrides_only_the_live_reading_label() {
    let mut snapshot = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 1);
    for (stage, expected) in [
        (
            RefreshCurrentSourceProgressStage::Parsing,
            "Parsing your agent history",
        ),
        (
            RefreshCurrentSourceProgressStage::IndexWriting,
            "Writing search index",
        ),
    ] {
        snapshot.progress.current_source_progress = Some(RefreshCurrentSourceProgress {
            stage,
            snapshot_pages_completed: None,
            snapshot_pages_total: None,
            snapshot_bytes_completed: None,
            snapshot_bytes_total: None,
            logical_rows_scanned: None,
            logical_certified_bytes: None,
        });
        assert_eq!(snapshot.phase(), stage.as_str());
        assert_eq!(shared_refresh_label(&snapshot), expected);
        assert_eq!(human_refresh_label(&snapshot), expected);
        assert_eq!(snapshot.progress.processed_sessions, 0);
        assert_eq!(snapshot.progress.processed_messages, 0);
        assert_eq!(snapshot.progress.processed_tool_calls, 0);
    }
}

#[test]
fn terminal_state_ignores_stale_activity_and_preserves_authoritative_counters() {
    let mut snapshot = terminal_status(
        RefreshRequestState::Published,
        "completed",
        "completed",
        RefreshTerminalPresentation::Complete,
    );
    snapshot.progress.current_source_progress = Some(RefreshCurrentSourceProgress {
        stage: RefreshCurrentSourceProgressStage::IndexWriting,
        snapshot_pages_completed: None,
        snapshot_pages_total: None,
        snapshot_bytes_completed: None,
        snapshot_bytes_total: None,
        logical_rows_scanned: None,
        logical_certified_bytes: None,
    });
    snapshot.set_terminal_history_totals(7, 11, 13, 17);

    assert_eq!(snapshot.phase(), "published");
    assert_eq!(shared_refresh_label(&snapshot), "History refresh complete");
    assert_eq!(human_refresh_label(&snapshot), "History refresh complete");
    assert_eq!(snapshot.progress.processed_sessions, 7);
    assert_eq!(snapshot.progress.processed_messages, 11);
    assert_eq!(snapshot.progress.processed_tool_calls, 13);
    assert_eq!(snapshot.progress.processed_bytes, 17);
}

#[test]
fn setup_live_never_substitutes_source_byte_progress_for_whole_run_eta() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut snapshot = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 1);
    snapshot.progress.current_source_progress = Some(RefreshCurrentSourceProgress {
        stage: RefreshCurrentSourceProgressStage::OnlineBackup,
        snapshot_pages_completed: None,
        snapshot_pages_total: None,
        snapshot_bytes_completed: Some(1),
        snapshot_bytes_total: Some(2),
        logical_rows_scanned: None,
        logical_certified_bytes: None,
    });
    snapshot.progress.estimated_remaining_millis = None;
    snapshot.set_presentation_agent_histories(Some(vec!["Codex".to_owned()]));
    snapshot.use_setup_live_presentation();

    let rendered = refresh_progress(&context, &snapshot).render_plain();
    assert!(
        rendered.contains("Estimated remaining  Estimating"),
        "{rendered}"
    );
    assert!(!rendered.contains("50%"), "{rendered}");
}

#[test]
fn setup_live_uses_whole_run_eta_for_determinate_progress() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut snapshot = active_status(RefreshLogicalPhase::Direct, "refreshing", true, 1);
    snapshot.progress.elapsed_millis = Some(30_000);
    snapshot.progress.estimated_remaining_millis = Some(90_000);
    snapshot.set_presentation_agent_histories(Some(vec!["Codex".to_owned()]));
    snapshot.use_setup_live_presentation();

    let rendered = refresh_progress(&context, &snapshot).render_plain();
    assert!(
        rendered.lines().nth(1).unwrap_or_default().ends_with("25%"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Estimated remaining  1m 30s"),
        "{rendered}"
    );
}

#[test]
fn setup_terminal_omits_stale_estimated_remaining_row() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 80));
    let mut snapshot = terminal_status(
        RefreshRequestState::Published,
        "completed",
        "completed",
        RefreshTerminalPresentation::Complete,
    );
    snapshot.progress.estimated_remaining_millis = Some(5_000);
    snapshot.set_presentation_agent_histories(Some(vec!["Codex".to_owned()]));
    snapshot.use_setup_live_presentation();

    let rendered = refresh_progress(&context, &snapshot).render_plain();
    assert!(
        rendered
            .lines()
            .nth(1)
            .unwrap_or_default()
            .ends_with("100%"),
        "{rendered}"
    );
    assert!(!rendered.contains("Estimated remaining"), "{rendered}");
    assert!(!rendered.contains("Complete"), "{rendered}");
}

#[test]
fn byte_progress_requires_one_complete_engine_snapshot_pair() {
    let mut paired = active_status(RefreshLogicalPhase::Attached, "copying", true, 2);
    paired.progress.current_source_progress = Some(RefreshCurrentSourceProgress {
        stage: RefreshCurrentSourceProgressStage::SourceFamilyCopy,
        snapshot_pages_completed: None,
        snapshot_pages_total: None,
        snapshot_bytes_completed: Some(256),
        snapshot_bytes_total: Some(512),
        logical_rows_scanned: None,
        logical_certified_bytes: None,
    });
    assert_eq!(paired.byte_progress(), (256, 512));

    paired
        .progress
        .current_source_progress
        .as_mut()
        .unwrap()
        .snapshot_bytes_total = None;
    assert_eq!(paired.byte_progress(), (0, 0));
}

#[test]
fn structured_terminal_outcome_alone_decides_done() {
    let cases = [
        (
            RefreshRequestState::Published,
            "completed",
            "completed",
            RefreshTerminalPresentation::Complete,
            "History refresh complete",
        ),
        (
            RefreshRequestState::Published,
            "completed_with_rejections",
            "completed_with_diagnostics",
            RefreshTerminalPresentation::Complete,
            "History refresh complete",
        ),
        (
            RefreshRequestState::Published,
            "completed_with_source_failures",
            "completed_with_diagnostics",
            RefreshTerminalPresentation::CompleteWithIssues,
            "History refresh complete with issues",
        ),
        (
            RefreshRequestState::Failed,
            "source_refresh_failed",
            "internal",
            RefreshTerminalPresentation::Failed,
            "History refresh failed",
        ),
    ];
    for (state, code, class, presentation, label) in cases {
        let snapshot = terminal_status(state, code, class, presentation);
        assert!(snapshot.is_terminal());
        assert_eq!(machine_refresh_label(&snapshot), label);
    }

    let physically_committed = active_status(RefreshLogicalPhase::Direct, "committed", true, 0);
    assert!(!physically_committed.is_terminal());
}
