use super::*;

#[test]
fn empty_generation_returns_empty_session_list() {
    let temp = tempdir().unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let page = index
        .list_sessions(&SessionListFilters::default(), DEFAULT_SESSION_LIST_LIMIT)
        .unwrap();
    assert_eq!(page.generation_id, index.generation_id());
    assert!(!page.truncated);
    assert!(page.sessions.is_empty());
}

#[test]
fn list_sessions_orders_by_newest_activity_and_reports_whole_session_metrics_under_since() {
    let temp = tempdir().unwrap();
    let codex_source = source_for_provider("codex", "codex_session_jsonl", "codex.jsonl");
    let claude_source = source_for_provider("claude", "claude_code_jsonl", "claude.jsonl");
    let custom_source = SourceKey::derive(
        "custom",
        "ctx_history_jsonl_v2",
        "v2",
        1,
        SourceAnchor::provider_native("custom-export", TypedKey::utf8("amp/threads").unwrap())
            .unwrap(),
    )
    .unwrap();

    // Session A: old start (1000), very new last activity (5000), 3 events.
    let mut a1 = document_for_session(&codex_source, "sess-a", 1, "first");
    a1.occurred_at_unix_ms = Some(1_700_000_001_000);
    add_literal_fact(&mut a1, LiteralFactKind::SessionCwd, "/work/alpha-repo");
    add_literal_fact(&mut a1, LiteralFactKind::Project, "alpha");
    add_literal_fact(&mut a1, LiteralFactKind::Branch, "main");

    let mut a2 = document_for_session(&codex_source, "sess-a", 2, "middle");
    a2.occurred_at_unix_ms = Some(1_700_000_002_000);

    let mut a3 = document_for_session(&codex_source, "sess-a", 3, "latest");
    a3.occurred_at_unix_ms = Some(1_700_000_005_000);

    // Session B: mid activity (3000), 2 events, Claude provider.
    let mut b1 = document_for_session(&claude_source, "sess-b", 1, "claude start");
    b1.occurred_at_unix_ms = Some(1_700_000_002_500);
    add_literal_fact(&mut b1, LiteralFactKind::Workspace, "/work/beta-workspace");

    let mut b2 = document_for_session(&claude_source, "sess-b", 2, "claude end");
    b2.occurred_at_unix_ms = Some(1_700_000_003_000);
    add_literal_fact(&mut b2, LiteralFactKind::Branch, "feat/beta");

    // Session C: untimestamped custom session (must sort after timestamped sessions).
    let mut c1 = document_for_session(&custom_source, "sess-c", 1, "untimestamped");
    c1.occurred_at_unix_ms = None;
    c1.native_event_id = Some(TypedKey::Composite(vec![
        TypedKey::utf8("amp").unwrap(),
        TypedKey::utf8("threads").unwrap(),
        TypedKey::utf8("event_id:c1").unwrap(),
    ]));
    add_literal_fact(&mut c1, LiteralFactKind::ToolWorkdir, "/work/gamma-dir");

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(codex_source.clone()).unwrap();
    for rec in [a1.clone(), a2, a3.clone()] {
        writer.add_core_record(rec).unwrap();
    }
    writer
        .certify_source(certificate(&codex_source, 1, 3))
        .unwrap();

    writer.begin_source(claude_source.clone()).unwrap();
    for rec in [b1.clone(), b2.clone()] {
        writer.add_core_record(rec).unwrap();
    }
    writer
        .certify_source(certificate(&claude_source, 1, 2))
        .unwrap();

    writer.begin_source(custom_source.clone()).unwrap();
    writer.add_core_record(c1.clone()).unwrap();
    writer
        .certify_source(certificate(&custom_source, 1, 1))
        .unwrap();

    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    // Unfiltered: newest activity first (A -> B -> C untimestamped).
    let all = index
        .list_sessions(&SessionListFilters::default(), 50)
        .unwrap();
    assert!(!all.truncated);
    assert_eq!(all.sessions.len(), 3);
    assert_eq!(all.sessions[0].session_id, a1.session_id);
    assert_eq!(all.sessions[0].provider, "codex");
    assert_eq!(all.sessions[0].cwd.as_deref(), Some("/work/alpha-repo"));
    assert_eq!(all.sessions[0].project.as_deref(), Some("alpha"));
    assert_eq!(all.sessions[0].branch.as_deref(), Some("main"));
    assert_eq!(all.sessions[0].started_at_unix_ms, Some(1_700_000_001_000));
    assert_eq!(
        all.sessions[0].last_activity_at_unix_ms,
        Some(1_700_000_005_000)
    );
    assert_eq!(all.sessions[0].events, 3);

    assert_eq!(all.sessions[1].session_id, b1.session_id);
    assert_eq!(all.sessions[1].provider, "claude");
    assert_eq!(all.sessions[1].cwd.as_deref(), Some("/work/beta-workspace"));
    assert_eq!(all.sessions[1].branch.as_deref(), Some("feat/beta"));
    assert_eq!(all.sessions[1].events, 2);

    assert_eq!(all.sessions[2].session_id, c1.session_id);
    assert_eq!(all.sessions[2].provider, "custom");
    assert_eq!(all.sessions[2].provider_key.as_deref(), Some("amp"));
    assert_eq!(all.sessions[2].source_id.as_deref(), Some("threads"));
    assert_eq!(all.sessions[2].cwd.as_deref(), Some("/work/gamma-dir"));
    assert_eq!(all.sessions[2].started_at_unix_ms, None);
    assert_eq!(all.sessions[2].last_activity_at_unix_ms, None);
    assert_eq!(all.sessions[2].events, 1);

    // --since filter excludes B and C and excludes A's earlier events,
    // yet A still reports whole-session started_at, events=3, cwd, project, branch!
    let since_only = index
        .list_sessions(
            &SessionListFilters {
                since_unix_ms: Some(1_700_000_004_000),
                ..SessionListFilters::default()
            },
            50,
        )
        .unwrap();
    assert_eq!(since_only.sessions.len(), 1);
    assert_eq!(since_only.sessions[0].session_id, a1.session_id);
    assert_eq!(since_only.sessions[0].events, 3);
    assert_eq!(
        since_only.sessions[0].started_at_unix_ms,
        Some(1_700_000_001_000)
    );
    assert_eq!(
        since_only.sessions[0].last_activity_at_unix_ms,
        Some(1_700_000_005_000)
    );
    assert_eq!(
        since_only.sessions[0].cwd.as_deref(),
        Some("/work/alpha-repo")
    );
    assert_eq!(since_only.sessions[0].project.as_deref(), Some("alpha"));
    assert_eq!(since_only.sessions[0].branch.as_deref(), Some("main"));

    // --provider filter.
    let claude_only = index
        .list_sessions(
            &SessionListFilters {
                provider: Some("claude".to_owned()),
                ..SessionListFilters::default()
            },
            50,
        )
        .unwrap();
    assert_eq!(claude_only.sessions.len(), 1);
    assert_eq!(claude_only.sessions[0].session_id, b1.session_id);

    // --workspace case-insensitive substring filter across cwd, workspace, tool_workdir, project.
    for (needle, expected_session) in [
        ("ALPHA-REPO", a1.session_id),
        ("alpha", a1.session_id),
        ("BETA-WORK", b1.session_id),
        ("gamma-dir", c1.session_id),
    ] {
        let matched = index
            .list_sessions(
                &SessionListFilters {
                    workspace: Some(needle.to_owned()),
                    ..SessionListFilters::default()
                },
                50,
            )
            .unwrap();
        assert_eq!(matched.sessions.len(), 1, "needle={needle}");
        assert_eq!(matched.sessions[0].session_id, expected_session);
    }
}

#[test]
fn no_subagents_excludes_parented_and_subagent_scoped_while_preserving_unscoped_roots() {
    let temp = tempdir().unwrap();
    let src = source("subagent-filter.jsonl");

    // 1. Primary root session.
    let mut primary = document_for_session(&src, "primary-root", 1, "primary");
    primary.agent_scope = Some(CoreAgentScope::Primary);
    primary.occurred_at_unix_ms = Some(1_700_000_010_000);

    // 2. Unscoped root session (agent_scope = None, no parent — e.g. OpenClaw / custom).
    let mut unscoped_root = document_for_session(&src, "unscoped-root", 1, "unscoped");
    unscoped_root.agent_scope = None;
    unscoped_root.occurred_at_unix_ms = Some(1_700_000_020_000);

    // 3. Explicit subagent scope (without parent link).
    let mut scoped_sub = document_for_session(&src, "scoped-sub", 1, "subagent");
    scoped_sub.agent_scope = Some(CoreAgentScope::Subagent);
    scoped_sub.occurred_at_unix_ms = Some(1_700_000_030_000);

    // 4. Parent-linked child with agent_scope = None (must STILL be excluded by --no-subagents).
    let mut parented_unscoped_1 = document_for_session(&src, "parented-child", 1, "child 1");
    parented_unscoped_1.agent_scope = None;
    parented_unscoped_1.parent_session_id = Some(primary.session_id);
    parented_unscoped_1.root_session_id = Some(primary.session_id);
    parented_unscoped_1.occurred_at_unix_ms = Some(1_700_000_040_000);

    let mut parented_unscoped_2 = document_for_session(&src, "parented-child", 2, "child 2");
    parented_unscoped_2.agent_scope = None;
    parented_unscoped_2.occurred_at_unix_ms = Some(1_700_000_050_000);

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(src.clone()).unwrap();
    for rec in [
        primary.clone(),
        unscoped_root.clone(),
        scoped_sub.clone(),
        parented_unscoped_1.clone(),
        parented_unscoped_2,
    ] {
        writer.add_core_record(rec).unwrap();
    }
    writer.certify_source(certificate(&src, 1, 5)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    // Without no_subagents: all 4 sessions returned, and parented-child carries coalesced lineage.
    let all = index
        .list_sessions(&SessionListFilters::default(), 50)
        .unwrap();
    assert_eq!(all.sessions.len(), 4);
    assert_eq!(all.sessions[0].session_id, parented_unscoped_1.session_id);
    assert_eq!(all.sessions[0].parent_session_id, Some(primary.session_id));
    assert_eq!(all.sessions[0].root_session_id, Some(primary.session_id));

    // With no_subagents: only primary and unscoped_root remain.
    let roots = index
        .list_sessions(
            &SessionListFilters {
                no_subagents: true,
                ..SessionListFilters::default()
            },
            50,
        )
        .unwrap();
    assert_eq!(roots.sessions.len(), 2);
    assert_eq!(roots.sessions[0].session_id, unscoped_root.session_id);
    assert_eq!(roots.sessions[1].session_id, primary.session_id);
}

#[test]
fn list_sessions_bounds_stored_core_decodes_to_winning_limit_only() {
    const SESSION_COUNT: usize = 60;
    const EVENTS_PER_SESSION: u64 = 10;
    const LIMIT: usize = 3;

    let temp = tempdir().unwrap();
    let src = source("bounded-session-list.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(src.clone()).unwrap();
    let mut expected_top = Vec::new();
    for session_idx in 0..SESSION_COUNT {
        let native_id = format!("session-{session_idx:03}");
        for seq in 1..=EVENTS_PER_SESSION {
            let mut rec = document_for_session(&src, &native_id, seq, "event body");
            rec.occurred_at_unix_ms =
                Some(1_700_000_000_000 + (session_idx as i64) * 100_000 + (seq as i64) * 1_000);
            if seq == 1 {
                add_literal_fact(
                    &mut rec,
                    LiteralFactKind::SessionCwd,
                    format!("/work/repo-{session_idx:03}"),
                );
            }
            if seq == EVENTS_PER_SESSION && session_idx >= SESSION_COUNT - LIMIT {
                expected_top.push(rec.session_id);
            }
            writer.add_core_record(rec).unwrap();
        }
    }
    writer
        .certify_source(certificate(
            &src,
            1,
            (SESSION_COUNT as u64) * EVENTS_PER_SESSION,
        ))
        .unwrap();
    writer.commit(|_| true).unwrap();
    expected_top.reverse();

    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    // Limit validation.
    assert!(matches!(
        index.list_sessions(&SessionListFilters::default(), 0),
        Err(SessionListError::InvalidLimit { requested: 0, .. })
    ));
    assert!(matches!(
        index.list_sessions(&SessionListFilters::default(), MAX_SESSION_LIST_LIMIT + 1),
        Err(SessionListError::InvalidLimit { .. })
    ));

    // Exact boundedness check: ONLY winning sessions may decode stored Core records.
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let page = index
        .list_sessions(&SessionListFilters::default(), LIMIT)
        .unwrap();
    let decodes = ctx_history_index_query::stored_core_event_record_materializations();

    assert!(page.truncated);
    assert_eq!(page.sessions.len(), LIMIT);
    let returned_ids: Vec<_> = page.sessions.iter().map(|s| s.session_id).collect();
    assert_eq!(returned_ids, expected_top);
    for (idx, session) in page.sessions.iter().enumerate() {
        let expected_idx = SESSION_COUNT - 1 - idx;
        assert_eq!(session.events, EVENTS_PER_SESSION);
        assert_eq!(
            session.cwd.as_deref(),
            Some(format!("/work/repo-{expected_idx:03}").as_str())
        );
    }
    assert!(
        decodes <= LIMIT * 3,
        "list_sessions decoded {decodes} Core records for limit={LIMIT} across {SESSION_COUNT} sessions ({} total events); must never decode non-winners",
        SESSION_COUNT * (EVENTS_PER_SESSION as usize)
    );

    // Exact non-truncated boundary: limit == SESSION_COUNT -> truncated = false.
    let exact = index
        .list_sessions(&SessionListFilters::default(), SESSION_COUNT)
        .unwrap();
    assert!(!exact.truncated);
    assert_eq!(exact.sessions.len(), SESSION_COUNT);

    // Limit == SESSION_COUNT - 1 -> truncated = true.
    let one_less = index
        .list_sessions(&SessionListFilters::default(), SESSION_COUNT - 1)
        .unwrap();
    assert!(one_less.truncated);
    assert_eq!(one_less.sessions.len(), SESSION_COUNT - 1);
}
