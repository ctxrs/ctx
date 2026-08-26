use std::{io::Write as _, path::PathBuf};

use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderRouteRole, ProviderSource,
    ProviderSourceKind, ProviderSourceRouteProvenance,
};
use unicode_width::UnicodeWidthStr as _;

use super::*;
use ctx_terminal::{ColorMode, StreamKind, TestContext};

fn context(width: usize, color: ColorMode) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
}

fn assert_fits(document: &Document, context: &RenderContext) {
    let width = context.content_width().unwrap_or(1);
    for line in document.render_plain().lines() {
        let copyable_path = {
            let atom = line.trim_start();
            atom.starts_with("~/") || atom.starts_with('/')
        };
        assert!(
            line.width() <= width || copyable_path,
            "{line:?} exceeded {width} columns"
        );
    }
}

fn strip_ansi(rendered: &str) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}

fn source(status: ProviderSourceStatus, path: &str) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Codex,
        path: PathBuf::from(path),
        exists: status != ProviderSourceStatus::Missing,
        source_format: "codex_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

#[test]
fn source_merge_is_stable_and_keeps_configured_missing_sources_visible() {
    let automatic = source(ProviderSourceStatus::Available, "/tmp/shared-history");
    let configured_duplicate = automatic.clone();
    let mut configured_missing = source(ProviderSourceStatus::Missing, "/tmp/configured-missing");
    configured_missing.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: "configured".to_owned(),
        root_path: configured_missing.path.clone(),
        route_role: ProviderRouteRole::from_static("codex-sessions"),
        automatic_route_role: None,
    };
    let mut merged = vec![automatic];
    merge_sources(
        &mut merged,
        vec![configured_duplicate, configured_missing.clone()],
    );
    assert_eq!(
        merged
            .iter()
            .map(|source| source.path.as_path())
            .collect::<Vec<_>>(),
        [
            std::path::Path::new("/tmp/shared-history"),
            std::path::Path::new("/tmp/configured-missing"),
        ]
    );

    assert!(source_is_visible(&configured_missing, false, &[], &[]));
    let mut unknown_missing = source(ProviderSourceStatus::Missing, "/tmp/unknown-missing");
    unknown_missing.provider = CaptureProvider::Goose;
    assert!(!source_is_visible(&unknown_missing, false, &[], &[]));
}

#[test]
fn configured_source_selection_is_visible_and_automatic_disable_is_explicit() {
    let mut configured_source = source(ProviderSourceStatus::Available, "/tmp/claude/projects");
    configured_source.provider = CaptureProvider::Claude;
    configured_source.source_format = "claude_projects_jsonl_tree";
    configured_source.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: "personal-claude".to_owned(),
        root_path: PathBuf::from("/tmp/claude"),
        route_role: ProviderRouteRole::from_static("claude-projects"),
        automatic_route_role: None,
    };
    let root = ctx_history_capture::ProviderRootDefinition {
        id: "personal-claude".to_owned(),
        provider: CaptureProvider::Claude,
        path: PathBuf::from("/tmp/claude"),
        group: Some("personal".to_owned()),
        kind: None,
    };
    assert_eq!(
        configured_root_for_source(std::slice::from_ref(&root), &configured_source)
            .map(|root| root.id.as_str()),
        Some("personal-claude")
    );

    let context = context(100, ColorMode::Never);
    let rendered = render_sources_human(
        &context,
        SourcesHumanRenderInput::from_sources(&[configured_source])
            .with_automatic_provider_discovery(false)
            .with_provider_roots(&[root]),
    )
    .render_plain();
    assert!(
        rendered.contains("personal-claude (personal)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Automatic discovery is disabled"),
        "{rendered}"
    );
}

#[cfg(unix)]
#[test]
fn configured_source_selection_uses_provenance_across_physical_path_aliases() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let physical = temp.path().join("claude-physical");
    std::fs::create_dir_all(physical.join("projects")).unwrap();
    let alias = temp.path().join("claude-alias");
    symlink(&physical, &alias).unwrap();
    let mut source = source(
        ProviderSourceStatus::Available,
        &physical.join("projects").to_string_lossy(),
    );
    source.provider = CaptureProvider::Claude;
    source.source_format = "claude_projects_jsonl_tree";
    source.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: "personal-claude".to_owned(),
        root_path: alias.clone(),
        route_role: ProviderRouteRole::from_static("claude-projects"),
        automatic_route_role: None,
    };
    let root = ctx_history_capture::ProviderRootDefinition {
        id: "personal-claude".to_owned(),
        provider: CaptureProvider::Claude,
        path: alias,
        group: Some("personal".to_owned()),
        kind: None,
    };

    assert_eq!(
        configured_root_for_source(std::slice::from_ref(&root), &source)
            .map(|root| root.id.as_str()),
        Some("personal-claude")
    );
}

#[test]
fn sources_success_is_outcome_first_and_responsive() {
    let home = PathBuf::from("private-capture-root");
    let location = home.join(".codex/sessions/and/a/long/location");
    let sources = vec![source(
        ProviderSourceStatus::Available,
        &location.to_string_lossy(),
    )];
    let concise_prefix = Path::new("~").join(".codex").display().to_string();
    for width in [32, 48, 80, 100, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_sources_human(
            &context,
            SourcesHumanRenderInput::from_sources(&sources)
                .with_hidden_missing_sources(2)
                .with_home(Some(&home)),
        );
        let rendered = document.render_plain();
        assert!(rendered.starts_with("✓ 1 history source is ready\n"));
        assert!(rendered.contains("Locations\n"));
        assert!(rendered.contains(&concise_prefix), "{width}: {rendered}");
        assert!(
            !rendered.contains("private-capture-root"),
            "{width}: {rendered}"
        );
        assert!(rendered.contains("ctx sources --all"));
        for atom in ["codex", "available"] {
            assert_eq!(
                rendered
                    .split_whitespace()
                    .filter(|token| *token == atom)
                    .count(),
                1,
                "{atom:?} did not remain intact at {width} columns: {rendered}"
            );
        }
        assert!(rendered.contains("Session history"), "{width}: {rendered}");
        assert!(!rendered.contains("jsonl"), "{width}: {rendered}");
        if width < 100 {
            assert!(
                rendered.contains("Source\n  codex\nStatus\n  available\n"),
                "{width}: {rendered}"
            );
        } else {
            assert!(
                rendered.contains("Source  Status     Location"),
                "{width}: {rendered}"
            );
        }
        assert_fits(&document, &context);
    }
}

#[test]
fn human_paths_only_abbreviate_complete_home_prefixes() {
    let home = PathBuf::from("test-home/example");
    assert_eq!(human_path(&home, Some(&home)), "~");
    let nested = home.join(".codex/sessions");
    assert_eq!(
        human_path(&nested, Some(&home)),
        Path::new("~").join(".codex/sessions").display().to_string()
    );
    let sibling = PathBuf::from("test-home/example-other/history");
    assert_eq!(
        human_path(&sibling, Some(&home)),
        sibling.display().to_string()
    );
}

#[test]
fn sources_stack_when_fixed_columns_do_not_fit_and_keep_atoms_whole() {
    let home = PathBuf::from("test-home");
    let factory_path = home.join(".factory/sessions");
    let mut factory = source(
        ProviderSourceStatus::Available,
        &factory_path.to_string_lossy(),
    );
    factory.provider = CaptureProvider::FactoryAiDroid;
    factory.source_format = "factory_ai_droid_sessions_jsonl";
    let cursor_path = home.join(".cursor/projects/example/agent-transcripts");
    let mut cursor = source(
        ProviderSourceStatus::Available,
        &cursor_path.to_string_lossy(),
    );
    cursor.provider = CaptureProvider::Cursor;
    cursor.source_format = "cursor_agent_transcript_jsonl_tree";
    let sources = [factory, cursor];

    for width in [80, 100, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_sources_human(
            &context,
            SourcesHumanRenderInput::from_sources(&sources).with_home(Some(&home)),
        );
        let rendered = document.render_plain();
        for atom in ["factory-ai-droid", "available", "cursor"] {
            assert!(
                rendered.split_whitespace().any(|token| token == atom),
                "{atom:?} did not remain intact at {width} columns: {rendered}"
            );
        }
        assert!(rendered.contains("Session history"), "{width}: {rendered}");
        assert!(
            rendered.contains("Agent transcripts"),
            "{width}: {rendered}"
        );
        assert!(!rendered.contains("jsonl"), "{width}: {rendered}");
        if width < 120 {
            assert!(
                rendered.contains("Source\n  factory-ai-droid\nStatus\n  available\n"),
                "{rendered}"
            );
        } else {
            assert!(
                rendered.contains("Source            Status     Location"),
                "{width}: {rendered}"
            );
        }
        assert_fits(&document, &context);
    }
}

#[test]
fn sources_empty_state_is_actionable() {
    let context = context(48, ColorMode::Never);
    let rendered =
        render_sources_human(&context, SourcesHumanRenderInput::from_sources(&[])).render_plain();
    assert!(rendered.starts_with("No history sources found\n"));
    assert!(rendered.contains("Next\n  ctx sources --all\n"));
}

#[test]
fn concise_sources_hide_automatic_empty_provider_but_preserve_configured_empty_roots() {
    let mut empty = source(ProviderSourceStatus::Empty, "/tmp/gemini");
    empty.provider = CaptureProvider::Gemini;
    empty.source_format = "gemini_session_json_tree";
    let mut configured_empty = empty.clone();
    configured_empty.path = PathBuf::from("/tmp/work-gemini");
    configured_empty.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: "work".to_owned(),
        root_path: configured_empty.path.clone(),
        route_role: ProviderRouteRole::from_static("gemini-sessions"),
        automatic_route_role: None,
    };
    let configured_root = ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Gemini,
        path: configured_empty.path.clone(),
        group: Some("team".to_owned()),
        kind: None,
    };
    let default_sources = std::slice::from_ref(&empty)
        .iter()
        .filter(|source| source_is_visible_for_output(source, false, OutputFormat::Text))
        .cloned()
        .collect::<Vec<_>>();
    let all_sources = std::slice::from_ref(&empty)
        .iter()
        .filter(|source| source_is_visible_for_output(source, true, OutputFormat::Text))
        .cloned()
        .collect::<Vec<_>>();
    let json_sources = std::slice::from_ref(&empty)
        .iter()
        .filter(|source| source_is_visible_for_output(source, false, OutputFormat::Json))
        .cloned()
        .collect::<Vec<_>>();
    let configured_sources = std::slice::from_ref(&configured_empty)
        .iter()
        .filter(|source| source_is_visible_for_output(source, false, OutputFormat::Text))
        .cloned()
        .collect::<Vec<_>>();
    let plain_context = context(80, ColorMode::Never);

    let default = render_sources_human(
        &plain_context,
        SourcesHumanRenderInput::from_sources(&default_sources),
    )
    .render_plain();
    assert!(
        default.starts_with("No history sources found\n"),
        "{default}"
    );
    assert!(!default.contains("gemini"), "{default}");
    assert_eq!(json_sources.len(), 1);
    assert_eq!(json_sources[0].status, ProviderSourceStatus::Empty);

    assert_eq!(configured_sources.len(), 1);
    assert_eq!(
        sources_discovery_observation(std::slice::from_ref(&empty), &[], &[]),
        SourcesDiscoveryObservation {
            providers_detected: 1,
            providers_existing: 1,
            providers_importable: 0,
        }
    );
    let configured_input = SourcesHumanRenderInput::from_sources(&configured_sources)
        .with_automatic_provider_discovery(false)
        .with_provider_roots(std::slice::from_ref(&configured_root));
    let configured = render_sources_human(&plain_context, configured_input).render_plain();
    assert!(configured.contains("gemini"), "{configured}");
    assert!(configured.contains("empty"), "{configured}");
    assert!(configured.contains("work (team)"), "{configured}");
    assert!(
        !configured.contains("no named roots are available"),
        "{configured}"
    );

    let styled = context(80, ColorMode::Always);
    let styled_document = render_sources_human(&styled, configured_input);
    assert_eq!(
        strip_ansi(&styled_document.render(&styled)),
        styled_document.render_plain()
    );

    let all = render_sources_human(
        &plain_context,
        SourcesHumanRenderInput::from_sources(&all_sources),
    )
    .render_plain();
    assert!(all.contains("gemini"), "{all}");
    assert!(all.contains("empty"), "{all}");
    assert!(all.contains("/tmp/gemini"), "{all}");
}

#[test]
fn sources_issue_is_safe_and_actionable() {
    let issue = DiscoveryIssue {
        provider: CaptureProvider::Codex,
        path: None,
        kind: DiscoveryIssueKind::SelectorUnreconstructible,
        reason: "selector contained \u{1b}[31mcontrol",
    };
    let context = context(48, ColorMode::Never);
    let document = render_sources_human(
        &context,
        SourcesHumanRenderInput::from_sources(&[]).with_issues(&[issue]),
    );
    let rendered = document.render_plain();
    assert!(rendered.contains("\\x1b[31mcontrol"));
    assert!(rendered.contains("ctx import --provider codex --path <path>"));
    assert!(!rendered.as_bytes().contains(&0x1b));
    assert_fits(&document, &context);
}

#[test]
fn configured_root_conflicts_name_paths_and_persistent_repairs() {
    let shared = PathBuf::from("/provider/claude");
    let roots = [
        ctx_history_capture::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: shared.clone(),
            group: None,
            kind: None,
        },
        ctx_history_capture::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: shared.clone(),
            group: None,
            kind: None,
        },
    ];
    let issue = DiscoveryIssue {
        provider: CaptureProvider::Claude,
        path: Some(shared),
        kind: DiscoveryIssueKind::ConfiguredRootConflict,
        reason: "distinct configured roots resolve to the same physical provider root",
    };
    let context = context(100, ColorMode::Never);
    let rendered = render_sources_human(
        &context,
        SourcesHumanRenderInput::from_sources(&[])
            .with_issues(&[issue])
            .with_provider_roots(&roots),
    )
    .render_plain();

    assert!(rendered.contains("configured/configured"), "{rendered}");
    assert!(
        rendered.contains("personal (/provider/claude)"),
        "{rendered}"
    );
    assert!(rendered.contains("work (/provider/claude)"), "{rendered}");
    assert!(
        rendered.contains("ctx sources add work --provider claude"),
        "{rendered}"
    );
    assert!(
        rendered.contains("--root <different-path> --replace"),
        "{rendered}"
    );
    assert!(rendered.contains("ctx sources remove work"), "{rendered}");
    assert!(!rendered.contains("ctx import"), "{rendered}");
}

#[test]
fn automatic_configured_conflict_recommends_persistent_policy() {
    let path = PathBuf::from("/provider/openhands/conversations");
    let roots = [ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::OpenHands,
        path: path.clone(),
        group: None,
        kind: Some(ctx_history_capture::ProviderRootKind::OpenHandsCurrentConversations),
    }];
    let issue = DiscoveryIssue {
        provider: CaptureProvider::OpenHands,
        path: Some(path),
        kind: DiscoveryIssueKind::ConfiguredRootConflict,
        reason: "configured root overlaps automatic root",
    };
    let context = context(100, ColorMode::Never);
    let rendered = render_sources_human(
        &context,
        SourcesHumanRenderInput::from_sources(&[])
            .with_issues(&[issue])
            .with_provider_roots(&roots),
    )
    .render_plain();

    assert!(rendered.contains("automatic/configured"), "{rendered}");
    assert!(
        rendered.contains("work (/provider/openhands/conversations)"),
        "{rendered}"
    );
    assert!(rendered.contains("automatic=false"), "{rendered}");
    assert!(!rendered.contains("ctx import"), "{rendered}");
}

#[test]
fn sources_plain_output_matches_ansi_stripped_output() {
    let sources = vec![source(ProviderSourceStatus::Available, "/tmp/codex")];
    let context = context(80, ColorMode::Always);
    let document = render_sources_human(&context, SourcesHumanRenderInput::from_sources(&sources));
    assert_eq!(
        strip_ansi(&document.render(&context)),
        document.render_plain()
    );
}
