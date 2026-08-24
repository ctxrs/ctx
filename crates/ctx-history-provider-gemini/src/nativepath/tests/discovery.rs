use super::*;

#[cfg(unix)]
fn authority_swap_fixture() -> (TempDir, PathBuf, PathBuf, PathBuf, GeminiTranscriptSource) {
    let temp = TempDir::new().unwrap();
    let ancestor = temp.path().join("authority");
    let root = ancestor.join(".gemini");
    let leaf = root.join("tmp/project/chats/session.jsonl");
    fs::create_dir_all(leaf.parent().unwrap()).unwrap();
    fs::write(&leaf, "{}\n").unwrap();
    let discovery = discover_gemini_transcripts(&root).unwrap();
    assert_eq!(discovery.transcripts.len(), 1);
    let source = discovery.transcripts[0].clone();
    (temp, ancestor, root, leaf, source)
}

#[cfg(unix)]
#[test]
fn gemini_discovery_rejects_root_swap_through_retained_source() {
    let (_temp, _ancestor, root, _leaf, source) = authority_swap_fixture();
    fs::rename(&root, root.with_file_name(".gemini-displaced")).unwrap();
    let replacement = root.join("tmp/project/chats/session.jsonl");
    fs::create_dir_all(replacement.parent().unwrap()).unwrap();
    fs::write(replacement, "{\"replacement\":true}\n").unwrap();

    assert!(source
        .open()
        .and_then(|opened| opened.revalidate())
        .is_err());
}

#[cfg(unix)]
#[test]
fn gemini_discovery_rejects_ancestor_swap_through_retained_source() {
    let (temp, ancestor, root, _leaf, source) = authority_swap_fixture();
    fs::rename(&ancestor, temp.path().join("authority-displaced")).unwrap();
    let replacement = root.join("tmp/project/chats/session.jsonl");
    fs::create_dir_all(replacement.parent().unwrap()).unwrap();
    fs::write(replacement, "{\"replacement\":true}\n").unwrap();

    assert!(source
        .open()
        .and_then(|opened| opened.revalidate())
        .is_err());
}

#[cfg(unix)]
#[test]
fn gemini_discovery_rejects_leaf_swap_through_retained_source() {
    let (_temp, _ancestor, _root, leaf, source) = authority_swap_fixture();
    fs::rename(&leaf, leaf.with_file_name("session-displaced.jsonl")).unwrap();
    fs::write(&leaf, "{\"replacement\":true}\n").unwrap();

    assert!(source
        .open()
        .and_then(|opened| opened.revalidate())
        .is_err());
}

#[test]
fn gemini_nativepath_discovers_only_exact_chat_layout_in_stable_order() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let chats = root.join("tmp/project/chats");
    fs::create_dir_all(chats.join("root-session")).unwrap();
    fs::create_dir_all(root.join("tmp/project/telemetry")).unwrap();
    fs::write(chats.join("z-primary.jsonl"), "{}\n").unwrap();
    fs::write(chats.join("root-session/a-child.jsonl"), "{}\n").unwrap();
    fs::write(root.join("tmp/project/telemetry/noise.log"), "{}\n").unwrap();

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert!(discovery.completed_inventory);
    assert_eq!(discovery.transcripts.len(), 2);
    assert!(discovery.transcripts[0].path < discovery.transcripts[1].path);
    assert!(matches!(
        discovery.transcripts[0].layout,
        GeminiTranscriptLayout::Subagent {
            ref parent_native_session_id_hint
        } if parent_native_session_id_hint == "root-session"
    ));
    assert_eq!(
        discovery.transcripts[1].layout,
        GeminiTranscriptLayout::Primary
    );
    assert_ne!(discovery.inventory_sha256, [0; 32]);
}
#[test]
fn gemini_nativepath_ignores_extra_nesting_and_layout_lookalikes() {
    for relative_path in [
        "tmp/project/extra/chats/session.jsonl",
        "tmp/project/chat/session.jsonl",
        "tmp/project/chatsx/session.jsonl",
        "tmp/project/chats/parent/extra/session.jsonl",
        "tmp/noise/.gemini/tmp/project/chats/ghost.jsonl",
    ] {
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}\n").unwrap();

        let discovery = discover_gemini_transcripts(&root).unwrap();
        assert!(
            discovery.transcripts.is_empty(),
            "unexpected transcript for {relative_path}"
        );
    }
}
#[test]
fn gemini_nativepath_normalizes_explicit_copied_tree_and_direct_file() {
    let temp = TempDir::new().unwrap();
    let copied = temp.path().join("copied-gemini-export");
    let chats = copied.join("tmp/project/chats");
    fs::create_dir_all(chats.join("parent-session")).unwrap();
    fs::create_dir_all(copied.join("tmp/project/telemetry")).unwrap();
    fs::write(chats.join("primary.jsonl"), "{}\n").unwrap();
    fs::write(chats.join("parent-session/subagent.jsonl"), "{}\n").unwrap();
    fs::write(copied.join("tmp/project/telemetry/unrelated.jsonl"), "{}\n").unwrap();

    let copied_discovery = discover_gemini_transcripts(&copied).unwrap();
    assert_eq!(copied_discovery.root, fs::canonicalize(&copied).unwrap());
    assert_eq!(copied_discovery.transcripts.len(), 2);
    assert!(copied_discovery
        .transcripts
        .iter()
        .any(|source| source.layout == GeminiTranscriptLayout::Primary));
    assert!(copied_discovery.transcripts.iter().any(|source| matches!(
        source.layout,
        GeminiTranscriptLayout::Subagent {
            ref parent_native_session_id_hint
        } if parent_native_session_id_hint == "parent-session"
    )));
    let selected_chat_tree = discover_gemini_transcripts(&chats).unwrap();
    assert_eq!(selected_chat_tree.transcripts.len(), 2);

    let direct = temp.path().join("standalone-session.jsonl");
    fs::write(&direct, "{}\n").unwrap();
    let direct_discovery = discover_gemini_transcripts(&direct).unwrap();
    assert_eq!(direct_discovery.transcripts.len(), 1);
    assert_eq!(
        direct_discovery.transcripts[0].path,
        fs::canonicalize(&direct).unwrap()
    );
    assert_eq!(
        direct_discovery.transcripts[0].layout,
        GeminiTranscriptLayout::Primary
    );
}

#[test]
fn gemini_nativepath_discovery_budgets_fail_at_the_exact_count_and_byte_boundaries() {
    let mut count_budget = DiscoveryBudget::with_limits(2, 1_024);
    count_budget.observe(Path::new("a")).unwrap();
    count_budget.observe(Path::new("b")).unwrap();
    let count_error = count_budget.observe(Path::new("c")).unwrap_err();
    assert!(count_error.to_string().contains("exceeds 2 entries"));

    let mut byte_budget = DiscoveryBudget::with_limits(10, 5);
    byte_budget.observe(Path::new("12345")).unwrap();
    let byte_error = byte_budget.observe(Path::new("x")).unwrap_err();
    assert!(byte_error.to_string().contains("exceeds 5 path bytes"));

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    write_transcript(&root, &[header("budget-session", "main")]);
    let integrated_error =
        discover_gemini_transcripts_with_limits(&root, 3, usize::MAX).unwrap_err();
    assert!(integrated_error.to_string().contains("exceeds 3 entries"));
}

#[test]
fn gemini_nativepath_discovery_handles_large_bounded_directories() {
    const TRANSCRIPTS: usize = 2_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let chats = root.join("tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    for index in 0..TRANSCRIPTS {
        fs::write(chats.join(format!("{index:04}.jsonl")), b"{}\n").unwrap();
    }
    #[cfg(target_os = "linux")]
    let descriptors_before = descriptors_open_under(&root);

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert_eq!(discovery.transcripts.len(), TRANSCRIPTS);
    #[cfg(target_os = "linux")]
    assert!(
        descriptors_open_under(&root) <= descriptors_before + 4,
        "the resident Gemini catalog must not retain one descriptor per transcript"
    );
}

#[cfg(target_os = "linux")]
fn descriptors_open_under(root: &Path) -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| target.starts_with(root))
        .count()
}

#[test]
fn gemini_catalog_accepts_hardlink_aliases() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let chats = root.join("tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    let first = chats.join("first.jsonl");
    let second = chats.join("second.jsonl");
    fs::write(&first, b"{}\n").unwrap();
    fs::hard_link(&first, &second).unwrap();

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert_eq!(discovery.transcripts.len(), 2);
    for source in discovery.transcripts {
        source.open().unwrap().revalidate().unwrap();
    }
}

#[test]
fn gemini_nativepath_completed_empty_inventory_is_an_explicit_zero_source_signal() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    fs::create_dir_all(root.join("tmp/project/chats")).unwrap();

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert!(discovery.completed_inventory);
    assert!(discovery.transcripts.is_empty());
    assert_ne!(discovery.inventory_sha256, [0; 32]);
}

#[test]
fn gemini_nativepath_preserves_nested_parent_hint_without_inventing_a_parent_recording() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = root.join("tmp/project/chats/root-session/child-session.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        jsonl(&[
            header("child-session", "subagent"),
            json!({
                "id": "child-user",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "child request"
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    let session = outcome.checkpoint.session.unwrap();
    assert_eq!(session.native_session_id, "child-session");
    assert_eq!(
        session.native_start_time.as_deref(),
        Some("2026-01-01T00:00:00.000Z")
    );
    assert_eq!(
        session.project_hash.as_deref(),
        Some("synthetic-project-hash")
    );
    assert_eq!(session.native_kind.as_deref(), Some("subagent"));
    assert_eq!(
        session.parent_native_session_id.as_deref(),
        Some("root-session")
    );
    assert_eq!(session.agent_scope, AgentScope::Subagent);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 1);
    assert_eq!(outcome.metrics.header_records, 1);
    let records = project_gemini_test_events(&source, rows).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].parent_session_id, None);
    assert_eq!(records[0].session_relationship, None);
    assert_eq!(records[0].agent_scope, Some(AgentScope::Subagent));
    assert_eq!(records[0].content.meaningful_text(), "child request");
    assert_eq!(
        records[0].native_event_id,
        Some(TypedKey::utf8("child-user").unwrap())
    );
}

#[cfg(unix)]
#[test]
fn gemini_nativepath_rejects_linked_inventory_components() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(root.join("tmp/project")).unwrap();
    symlink(&outside, root.join("tmp/project/chats")).unwrap();

    let error = discover_gemini_transcripts(&root).unwrap_err();
    assert!(error.to_string().contains("linked Gemini transcript"));
}
