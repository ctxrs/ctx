mod support;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use ctx_history_capture_model::ProviderRootDefinition;
use ctx_history_capture_model::ProviderRootKind;
use ctx_history_capture_model::ProviderRouteRole;
use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::*;

use support::{tempdir, TEST_PROVIDER_PROBES};

const CONFIGURED_ROOT_SYMLINK_REASON: &str =
    "the configured provider history root uses a symlink or other unsupported component";

const EXACT_CAPABILITIES: &[(CaptureProvider, ConfiguredRootPathKind, &str, &str)] = &[
    (
        CaptureProvider::GrokBuild,
        ConfiguredRootPathKind::Directory,
        "grok_build_session_updates_jsonl_tree",
        "grok-build-sessions",
    ),
    (
        CaptureProvider::DeepSeekHarness,
        ConfiguredRootPathKind::Directory,
        "deepseek_harness_session_jsonl_tree",
        "deepseek-harness-sessions",
    ),
    (
        CaptureProvider::Pi,
        ConfiguredRootPathKind::Directory,
        "pi_session_jsonl",
        "pi-sessions",
    ),
    (
        CaptureProvider::OpenCode,
        ConfiguredRootPathKind::File,
        "opencode_sqlite",
        "opencode-database",
    ),
    (
        CaptureProvider::Kilo,
        ConfiguredRootPathKind::File,
        "kilo_sqlite",
        "kilo-database",
    ),
    (
        CaptureProvider::MiMoCode,
        ConfiguredRootPathKind::File,
        "mimocode_sqlite",
        "mimocode-database",
    ),
    (
        CaptureProvider::KiroCli,
        ConfiguredRootPathKind::File,
        "kiro_cli_sqlite",
        "kiro-cli-database",
    ),
    (
        CaptureProvider::Crush,
        ConfiguredRootPathKind::File,
        "crush_sqlite",
        "crush-project-database",
    ),
    (
        CaptureProvider::Goose,
        ConfiguredRootPathKind::File,
        "goose_sessions_sqlite",
        "goose-sessions-database",
    ),
    (
        CaptureProvider::Antigravity,
        ConfiguredRootPathKind::Directory,
        "antigravity_cli_transcript_jsonl_tree",
        "antigravity-brain",
    ),
    (
        CaptureProvider::Gemini,
        ConfiguredRootPathKind::Directory,
        "gemini_cli_chat_recording_jsonl",
        "gemini-chats",
    ),
    (
        CaptureProvider::Tabnine,
        ConfiguredRootPathKind::Directory,
        "tabnine_cli_chat_recording_jsonl",
        "tabnine-agent-history",
    ),
    (
        CaptureProvider::Cursor,
        ConfiguredRootPathKind::Directory,
        "cursor_agent_transcript_jsonl_tree",
        "cursor-projects",
    ),
    (
        CaptureProvider::Zed,
        ConfiguredRootPathKind::File,
        "zed_threads_sqlite",
        "zed-threads-database",
    ),
    (
        CaptureProvider::CopilotCli,
        ConfiguredRootPathKind::Directory,
        "copilot_cli_session_events_jsonl",
        "copilot-session-state",
    ),
    (
        CaptureProvider::FactoryAiDroid,
        ConfiguredRootPathKind::Directory,
        "factory_ai_droid_sessions_jsonl",
        "factory-droid-sessions",
    ),
    (
        CaptureProvider::QwenCode,
        ConfiguredRootPathKind::Directory,
        "qwen_code_chat_jsonl_tree",
        "qwen-projects",
    ),
    (
        CaptureProvider::KimiCodeCli,
        ConfiguredRootPathKind::Directory,
        "kimi_code_cli_wire_jsonl_tree",
        "kimi-history",
    ),
    (
        CaptureProvider::Auggie,
        ConfiguredRootPathKind::Directory,
        "auggie_session_json",
        "auggie-sessions",
    ),
    (
        CaptureProvider::Junie,
        ConfiguredRootPathKind::Directory,
        "junie_session_events_jsonl_tree",
        "junie-sessions",
    ),
    (
        CaptureProvider::Firebender,
        ConfiguredRootPathKind::File,
        "firebender_chat_history_sqlite",
        "firebender-chat-history-database",
    ),
    (
        CaptureProvider::ForgeCode,
        ConfiguredRootPathKind::File,
        "forgecode_sqlite",
        "forgecode-database",
    ),
    (
        CaptureProvider::DeepAgents,
        ConfiguredRootPathKind::File,
        "deepagents_sessions_sqlite",
        "deepagents-sessions-database",
    ),
    (
        CaptureProvider::MistralVibe,
        ConfiguredRootPathKind::Directory,
        "mistral_vibe_session_jsonl_tree",
        "mistral-vibe-sessions",
    ),
    (
        CaptureProvider::Mux,
        ConfiguredRootPathKind::Directory,
        "mux_session_jsonl_tree",
        "mux-sessions",
    ),
    (
        CaptureProvider::RovoDev,
        ConfiguredRootPathKind::Directory,
        "rovodev_session_json_tree",
        "rovodev-sessions",
    ),
    (
        CaptureProvider::Hermes,
        ConfiguredRootPathKind::File,
        "hermes_state_sqlite",
        "hermes-profile-database",
    ),
    (
        CaptureProvider::AstrBot,
        ConfiguredRootPathKind::File,
        "astrbot_data_v4_sqlite",
        "astrbot-instance-database",
    ),
    (
        CaptureProvider::Continue,
        ConfiguredRootPathKind::Directory,
        "continue_cli_sessions_json",
        "continue-sessions",
    ),
    (
        CaptureProvider::RooCode,
        ConfiguredRootPathKind::Directory,
        "roo_task_directory_json",
        "roo-task-store",
    ),
    (
        CaptureProvider::Lingma,
        ConfiguredRootPathKind::File,
        "lingma_sqlite",
        "lingma-client-profile-database",
    ),
    (
        CaptureProvider::Qoder,
        ConfiguredRootPathKind::Directory,
        "qoder_transcript_jsonl_tree",
        "qoder-projects",
    ),
    (
        CaptureProvider::Warp,
        ConfiguredRootPathKind::File,
        "warp_sqlite",
        "warp-surface-database",
    ),
    (
        CaptureProvider::CodeBuddy,
        ConfiguredRootPathKind::Directory,
        "codebuddy_history_json",
        "codebuddy-history",
    ),
];

const INTENTIONAL_AUTOMATIC_EXACT: &[CaptureProvider] =
    &[CaptureProvider::NanoClaw, CaptureProvider::Shelley];

const COMPOUND_CAPABILITIES: &[(CaptureProvider, ConfiguredRootExpander)] = &[
    (CaptureProvider::Codex, ConfiguredRootExpander::CodexHomeV1),
    (
        CaptureProvider::Claude,
        ConfiguredRootExpander::ClaudeHomeV1,
    ),
    (
        CaptureProvider::OpenClaw,
        ConfiguredRootExpander::OpenClawStateRootV1,
    ),
    (
        CaptureProvider::Cline,
        ConfiguredRootExpander::ClineCommonDataRootV1,
    ),
    (
        CaptureProvider::OpenHands,
        ConfiguredRootExpander::OpenHandsKindV1,
    ),
];

fn context(temp: &tempfile::TempDir) -> DiscoveryContext {
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
}

fn root(id: &str, provider: CaptureProvider, path: PathBuf) -> ProviderRootDefinition {
    ProviderRootDefinition {
        id: id.to_owned(),
        provider,
        path,
        group: None,
        kind: (provider == CaptureProvider::OpenHands)
            .then_some(ProviderRootKind::OpenHandsCurrentConversations),
    }
}

fn openhands_root(id: &str, path: PathBuf, kind: ProviderRootKind) -> ProviderRootDefinition {
    ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::OpenHands,
        path,
        group: None,
        kind: Some(kind),
    }
}

fn provider_report(context: &DiscoveryContext, provider: CaptureProvider) -> DiscoveryReport {
    discover_provider_sources_for_provider_with_context(&TEST_PROVIDER_PROBES, context, provider)
}

fn configured_report(
    base: DiscoveryContext,
    roots: Vec<ProviderRootDefinition>,
    provider: CaptureProvider,
) -> DiscoveryReport {
    provider_report(
        &base
            .with_automatic_provider_discovery(false)
            .with_configured_provider_roots(roots),
        provider,
    )
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn route_role(source: &ProviderSource) -> &[u8] {
    source
        .route_provenance
        .route_role()
        .expect("configured route role")
        .as_bytes()
}

fn automatic_route_role(source: &ProviderSource) -> &[u8] {
    source
        .route_provenance
        .automatic_route_role()
        .expect("matching automatic route role")
        .as_bytes()
}

fn assert_configured(source: &ProviderSource, expected_id: &str, expected_root: &Path) {
    assert_eq!(
        source.route_provenance.configured_root(),
        Some((expected_id, expected_root))
    );
    assert!(source.route_provenance.route_role().is_some());
}

#[test]
fn capability_table_is_exhaustive_and_freezes_39_2_0_inventory() {
    assert_eq!(configured_root_capabilities().len(), 41);
    let actual = configured_root_capabilities()
        .iter()
        .map(|capability| capability.provider)
        .collect::<Vec<_>>();
    assert_eq!(actual.len(), 41);
    assert_eq!(
        actual,
        provider_source_specs()
            .iter()
            .map(|spec| spec.provider)
            .collect::<Vec<_>>()
    );

    let enabled = configured_root_capabilities()
        .iter()
        .filter(|capability| capability.state.is_enabled())
        .count();
    let intentional = configured_root_capabilities()
        .iter()
        .filter(|capability| {
            capability.state == ConfiguredRootCapabilityState::IntentionalAutomaticExact
        })
        .map(|capability| capability.provider)
        .collect::<HashSet<_>>();
    let pending = configured_root_capabilities()
        .iter()
        .filter(|capability| capability.state == ConfiguredRootCapabilityState::PendingNamedSupport)
        .map(|capability| capability.provider)
        .collect::<Vec<_>>();

    assert_eq!(enabled, 39);
    assert_eq!(intentional.len(), 2);
    assert_eq!(
        intentional,
        INTENTIONAL_AUTOMATIC_EXACT.iter().copied().collect()
    );
    assert!(pending.is_empty());
}

#[test]
fn exact_and_compound_capability_metadata_is_exhaustive() {
    assert_eq!(EXACT_CAPABILITIES.len(), 34);
    for &(provider, expected_path_kind, source_format, route_role) in EXACT_CAPABILITIES {
        assert_eq!(
            configured_root_capability(provider).map(|capability| capability.state),
            Some(ConfiguredRootCapabilityState::Enabled {
                expected_path_kind,
                expander: ConfiguredRootExpander::ExactSource {
                    source_format,
                    route_role,
                },
            })
        );
    }
    for &(provider, expander) in COMPOUND_CAPABILITIES {
        assert_eq!(
            configured_root_capability(provider).map(|capability| capability.state),
            Some(ConfiguredRootCapabilityState::Enabled {
                expected_path_kind: ConfiguredRootPathKind::Directory,
                expander,
            })
        );
    }
}

#[test]
fn configured_root_missing_route_inventory_is_exhaustive_when_automatic_is_false() {
    let temp = tempdir();
    let mut roots = Vec::new();
    let mut root_paths = HashMap::new();
    for spec in provider_source_specs() {
        let id = format!("{}-configured", spec.provider.as_str());
        let path = temp.path().join("configured").join(spec.provider.as_str());
        root_paths.insert(id.clone(), path.clone());
        roots.push(root(&id, spec.provider, path));
    }
    let context = context(&temp)
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(roots);
    let report = discover_provider_sources_with_context(&TEST_PROVIDER_PROBES, &context);

    assert_eq!(report.sources.len(), 41);
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].provider, CaptureProvider::OpenClaw);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::ConfiguredRootMissing
    );
    assert!(report
        .sources
        .iter()
        .all(|source| source.status == ProviderSourceStatus::Missing));
    for source in &report.sources {
        let (id, path) = source
            .route_provenance
            .configured_root()
            .expect("configured provenance");
        assert_eq!(Some(path), root_paths.get(id).map(PathBuf::as_path));
    }

    for capability in configured_root_capabilities() {
        let count = report
            .sources
            .iter()
            .filter(|source| source.provider == capability.provider)
            .count();
        let expected = match capability.state {
            ConfiguredRootCapabilityState::IntentionalAutomaticExact
            | ConfiguredRootCapabilityState::PendingNamedSupport => 0,
            ConfiguredRootCapabilityState::Enabled {
                expander: ConfiguredRootExpander::CodexHomeV1,
                ..
            } => 3,
            ConfiguredRootCapabilityState::Enabled {
                expander: ConfiguredRootExpander::OpenClawStateRootV1,
                ..
            } => 0,
            ConfiguredRootCapabilityState::Enabled {
                expander: ConfiguredRootExpander::ClineCommonDataRootV1,
                ..
            } => 2,
            ConfiguredRootCapabilityState::Enabled { .. } => 1,
        };
        assert_eq!(count, expected, "unexpected route count for {capability:?}");
    }
}

#[test]
fn corrected_exact_session_and_gemini_roots_probe_the_configured_path_itself() {
    let fixtures = [
        (
            CaptureProvider::GrokBuild,
            "grok",
            "workspace/session/updates.jsonl",
            "grok_build_session_updates_jsonl_tree",
        ),
        (
            CaptureProvider::DeepSeekHarness,
            "deepseek",
            "bucket/session/session.jsonl",
            "deepseek_harness_session_jsonl_tree",
        ),
        (
            CaptureProvider::Pi,
            "pi",
            "session.jsonl",
            "pi_session_jsonl",
        ),
        (
            CaptureProvider::Gemini,
            "gemini",
            "tmp/project/chats/session.jsonl",
            "gemini_cli_chat_recording_jsonl",
        ),
    ];
    for (provider, id, leaf, source_format) in fixtures {
        let temp = tempdir();
        let selected = temp.path().join(id);
        write(&selected.join(leaf), b"{}\n");
        let report = configured_report(
            context(&temp),
            vec![root(id, provider, selected.clone())],
            provider,
        );
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].path, selected);
        assert_eq!(report.sources[0].source_format, source_format);
        assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
        assert_configured(&report.sources[0], id, &report.sources[0].path);
    }

    let temp = tempdir();
    let database = temp.path().join("opencode.db");
    write(&database, b"database sentinel");
    let report = configured_report(
        context(&temp),
        vec![root(
            "opencode",
            CaptureProvider::OpenCode,
            database.clone(),
        )],
        CaptureProvider::OpenCode,
    );
    assert_eq!(report.sources[0].path, database);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
}

#[test]
fn configured_auggie_roots_report_only_adapter_visible_session_json() {
    for (id, leaf, expected_status) in [
        ("direct", "session.json", ProviderSourceStatus::Available),
        (
            "sessions-child",
            "sessions/session.json",
            ProviderSourceStatus::Available,
        ),
        (
            "nested-decoy",
            "archive/session.json",
            ProviderSourceStatus::Empty,
        ),
    ] {
        let temp = tempdir();
        let selected = temp.path().join(id);
        write(
            &selected.join(leaf),
            br#"{"sessionId":"configured-auggie","chatHistory":[]}"#,
        );
        let report = configured_report(
            context(&temp),
            vec![root(id, CaptureProvider::Auggie, selected.clone())],
            CaptureProvider::Auggie,
        );
        assert!(report.issues.is_empty(), "{id}: {:?}", report.issues);
        assert_eq!(report.sources.len(), 1, "{id}");
        assert_eq!(report.sources[0].path, selected, "{id}");
        assert_eq!(report.sources[0].status, expected_status, "{id}");
        assert_configured(&report.sources[0], id, &report.sources[0].path);
    }

    let temp = tempdir();
    let selected = temp.path().join("sessions-precedence");
    write(
        &selected.join("ignored.json"),
        br#"{"sessionId":"shadowed-auggie","chatHistory":[]}"#,
    );
    fs::create_dir(selected.join("sessions")).unwrap();
    let report = configured_report(
        context(&temp),
        vec![root(
            "sessions-precedence",
            CaptureProvider::Auggie,
            selected,
        )],
        CaptureProvider::Auggie,
    );
    assert!(report.issues.is_empty());
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Empty);
}

#[test]
fn claude_and_codex_retain_released_home_expansions_and_role_bytes() {
    let temp = tempdir();
    let claude = temp.path().join("claude-home");
    write(&claude.join("projects/session.jsonl"), b"{}\n");
    let report = configured_report(
        context(&temp),
        vec![root("claude", CaptureProvider::Claude, claude.clone())],
        CaptureProvider::Claude,
    );
    assert_eq!(report.sources[0].path, claude.join("projects"));
    assert_eq!(route_role(&report.sources[0]), b"claude-projects");

    let codex = temp.path().join("codex-home");
    write(&codex.join("sessions/active.jsonl"), b"{}\n");
    write(&codex.join("archived_sessions/archived.jsonl"), b"{}\n");
    write(&codex.join("history.jsonl"), b"{}\n");
    let report = configured_report(
        context(&temp),
        vec![root("codex", CaptureProvider::Codex, codex.clone())],
        CaptureProvider::Codex,
    );
    assert_eq!(report.sources.len(), 3);
    assert_eq!(route_role(&report.sources[0]), b"codex-sessions");
    assert_eq!(route_role(&report.sources[1]), b"codex-archived-sessions");
    assert_eq!(route_role(&report.sources[2]), b"codex-prompt-history");
    assert!(report
        .sources
        .iter()
        .all(|source| source.status == ProviderSourceStatus::Available));
}

fn write_openclaw_v17(path: &Path, owner: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(ctx_history_openclaw_schema::test_support::OPENCLAW_AGENT_V17_MINIMAL_SCHEMA)
        .unwrap();
    connection
        .execute(
            "INSERT INTO schema_meta\
               (meta_key, role, schema_version, agent_id, app_version, created_at, updated_at)\
             VALUES ('primary', 'agent', 17, ?1, 'test', 1, 1)",
            [owner],
        )
        .unwrap();
}

fn openclaw_agent_id(path: &Path) -> &str {
    let components = path.components().collect::<Vec<_>>();
    let index = components
        .iter()
        .position(|component| component.as_os_str() == "agents")
        .expect("agents component");
    components[index + 1].as_os_str().to_str().unwrap()
}

#[test]
fn openclaw_state_roots_expand_bounded_agents_with_precedence_and_stable_dynamic_roles() {
    let temp = tempdir();
    let state = temp.path().join("state");
    write(
        &state.join("openclaw.json"),
        b"{agents:{list:[{id:'Gamma'},{id:'Beta'},{id:'Alpha'}]}}",
    );
    let alpha_database = state.join("agents/alpha/agent/openclaw-agent.sqlite");
    write_openclaw_v17(&alpha_database, "alpha");
    write(
        &state.join("agents/alpha/sessions/suppressed.jsonl"),
        b"{}\n",
    );
    write(
        &state.join("agents/beta/agent/openclaw-agent.sqlite"),
        b"corrupt",
    );
    write(&state.join("agents/beta/sessions/fallback.jsonl"), b"{}\n");
    write(
        &state.join("agents/gamma/agent/openclaw-agent.sqlite"),
        b"corrupt",
    );

    let moved = temp.path().join("moved-state");
    write(
        &moved.join("openclaw.json"),
        b"{agents:{list:[{id:'Beta'},{id:'Alpha'}]}}",
    );
    write(&moved.join("agents/alpha/sessions/active.jsonl"), b"{}\n");
    write(&moved.join("agents/beta/sessions/active.jsonl"), b"{}\n");

    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(&data_root).unwrap();
    let report = configured_report(
        context(&temp).with_data_root(data_root),
        vec![
            root("state", CaptureProvider::OpenClaw, state.clone()),
            root("moved", CaptureProvider::OpenClaw, moved.clone()),
        ],
        CaptureProvider::OpenClaw,
    );
    assert_eq!(report.sources.len(), 5);

    let state_sources = report
        .sources
        .iter()
        .filter(|source| {
            source
                .route_provenance
                .configured_root()
                .is_some_and(|(id, _)| id == "state")
        })
        .collect::<Vec<_>>();
    assert_eq!(state_sources[0].path, alpha_database);
    assert_eq!(state_sources[0].source_format, "openclaw_agent_sqlite");
    assert_eq!(state_sources[1].path, state.join("agents/beta/sessions"));
    assert_eq!(
        state_sources[1].source_format,
        "openclaw_session_jsonl_tree"
    );
    assert_eq!(state_sources[2].source_format, "unsupported");
    assert_eq!(state_sources[2].status, ProviderSourceStatus::Unsupported);

    let mut roles = HashMap::<String, Vec<Vec<u8>>>::new();
    for source in &report.sources {
        roles
            .entry(openclaw_agent_id(&source.path).to_owned())
            .or_default()
            .push(route_role(source).to_vec());
    }
    for (agent_id, actual) in &roles {
        let expected =
            ProviderRouteRole::from_dynamic([b"openclaw-agent".as_slice(), agent_id.as_bytes()])
                .unwrap();
        assert!(actual.iter().all(|role| role == expected.as_bytes()));
    }
    assert_ne!(roles["alpha"][0], roles["beta"][0]);
    assert_ne!(roles["beta"][0], roles["gamma"][0]);
    assert_eq!(roles["alpha"].len(), 2);
    assert_eq!(roles["beta"].len(), 2);
}

#[test]
fn openclaw_compound_root_is_route_less_while_missing_and_restores_exact_agents() {
    let temp = tempdir();
    let state = temp.path().join("openclaw-state");
    write(
        &state.join("openclaw.json"),
        b"{agents:{list:[{id:'Beta'},{id:'Alpha'}]}}",
    );
    write(&state.join("agents/alpha/sessions/alpha.jsonl"), b"{}\n");
    write(&state.join("agents/beta/sessions/beta.jsonl"), b"{}\n");
    let base = context(&temp);
    let discover = || {
        configured_report(
            base.clone(),
            vec![root(
                "configured-state",
                CaptureProvider::OpenClaw,
                state.clone(),
            )],
            CaptureProvider::OpenClaw,
        )
    };

    let initial = discover();
    assert!(initial.issues.is_empty());
    assert_eq!(initial.sources.len(), 2);
    assert!(initial
        .sources
        .iter()
        .all(|source| source.status == ProviderSourceStatus::Available));
    let initial_routes = initial
        .sources
        .iter()
        .map(|source| {
            assert_configured(source, "configured-state", &state);
            (
                source.path.clone(),
                source.source_format,
                route_role(source).to_vec(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        initial
            .sources
            .iter()
            .map(|source| openclaw_agent_id(&source.path))
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );

    let displaced = temp.path().join("openclaw-state-displaced");
    fs::rename(&state, &displaced).unwrap();
    let missing = discover();
    assert!(missing.sources.is_empty());
    assert_eq!(missing.issues.len(), 1);
    assert_eq!(
        missing.issues[0].kind,
        DiscoveryIssueKind::ConfiguredRootMissing
    );
    assert_eq!(missing.issues[0].path.as_deref(), Some(state.as_path()));

    let cold_missing_path = temp.path().join("cold-missing-openclaw-state");
    let cold_missing = configured_report(
        base.clone(),
        vec![root(
            "cold-missing",
            CaptureProvider::OpenClaw,
            cold_missing_path,
        )],
        CaptureProvider::OpenClaw,
    );
    assert!(cold_missing.sources.is_empty());
    assert_eq!(cold_missing.issues.len(), 1);
    assert_eq!(
        cold_missing.issues[0].kind,
        DiscoveryIssueKind::ConfiguredRootMissing
    );

    fs::rename(&displaced, &state).unwrap();
    let restored = discover();
    assert!(restored.issues.is_empty());
    assert_eq!(restored.sources.len(), 2);
    assert_eq!(
        restored
            .sources
            .iter()
            .map(|source| (
                source.path.clone(),
                source.source_format,
                route_role(source).to_vec(),
            ))
            .collect::<Vec<_>>(),
        initial_routes
    );
}

#[test]
fn configured_openclaw_truncated_agent_inventory_is_route_less() {
    let temp = tempdir();
    let state = temp.path().join("openclaw-state");
    let agents = (0..129)
        .map(|index| serde_json::json!({"id": format!("agent-{index:03}")}))
        .collect::<Vec<_>>();
    write(
        &state.join("openclaw.json"),
        &serde_json::to_vec(&serde_json::json!({"agents": {"list": agents}})).unwrap(),
    );
    write(
        &state.join("agents/agent-000/sessions/first.jsonl"),
        b"{}\n",
    );

    let report = configured_report(
        context(&temp),
        vec![root(
            "configured-state",
            CaptureProvider::OpenClaw,
            state.clone(),
        )],
        CaptureProvider::OpenClaw,
    );

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
    assert_eq!(report.issues[0].path.as_deref(), Some(state.as_path()));
}

#[test]
fn configured_openclaw_route_matching_automatic_keeps_automatic_role_bytes() {
    let temp = tempdir();
    let state = temp.path().join("openclaw-state");
    write(
        &state.join("openclaw.json"),
        b"{agents:{list:[{id:'alpha'}]}}",
    );
    write(&state.join("agents/alpha/sessions/session.jsonl"), b"{}\n");
    let context = context(&temp)
        .with_env("OPENCLAW_STATE_DIR", state.as_os_str())
        .with_configured_provider_roots(vec![root(
            "configured-state",
            CaptureProvider::OpenClaw,
            state,
        )]);

    let report = provider_report(&context, CaptureProvider::OpenClaw);
    let configured = report
        .sources
        .iter()
        .filter(|source| source.route_provenance.configured_root().is_some())
        .collect::<Vec<_>>();
    assert_eq!(configured.len(), 1);
    assert_eq!(
        route_role(configured[0]),
        ProviderRouteRole::from_dynamic([b"openclaw-agent".as_slice(), b"alpha".as_slice(),])
            .unwrap()
            .as_bytes()
    );
    assert_eq!(
        automatic_route_role(configured[0]),
        ProviderRouteRole::from_dynamic([b"agent".as_slice(), b"alpha".as_slice()])
            .unwrap()
            .as_bytes()
    );
}

#[test]
fn cline_common_data_root_emits_distinct_task_and_sdk_roles() {
    let temp = tempdir();
    let data = temp.path().join("cline-data");
    write(
        &data.join("tasks/legacy/api_conversation_history.json"),
        b"[]",
    );
    write(
        &data.join("sessions/sessions.index.json"),
        br#"{"version":1,"sessions":{}}"#,
    );
    let report = configured_report(
        context(&temp),
        vec![root("cline", CaptureProvider::Cline, data.clone())],
        CaptureProvider::Cline,
    );
    assert_eq!(report.sources.len(), 2);
    assert_eq!(report.sources[0].path, data);
    assert_eq!(report.sources[1].path, report.sources[0].path);
    assert_eq!(report.sources[0].source_format, "cline_task_directory_json");
    assert_eq!(report.sources[1].source_format, "cline_sdk_session_store");
    assert_eq!(route_role(&report.sources[0]), b"cline-tasks");
    assert_eq!(route_role(&report.sources[1]), b"cline-sdk");
    assert!(report
        .sources
        .iter()
        .all(|source| source.status == ProviderSourceStatus::Available));
}

#[test]
fn every_enabled_root_rejects_the_wrong_no_follow_file_kind() {
    for capability in configured_root_capabilities()
        .iter()
        .filter(|capability| capability.state.is_enabled())
    {
        let temp = tempdir();
        let selected = temp.path().join("selected");
        match capability.state.expected_path_kind().unwrap() {
            ConfiguredRootPathKind::Directory => write(&selected, b"file"),
            ConfiguredRootPathKind::File => fs::create_dir_all(&selected).unwrap(),
        }
        let report = configured_report(
            context(&temp),
            vec![root("wrong-kind", capability.provider, selected)],
            capability.provider,
        );
        assert!(report.sources.is_empty(), "{capability:?}");
        assert_eq!(report.issues.len(), 1, "{capability:?}");
    }
}

#[cfg(unix)]
#[test]
fn every_expander_family_rejects_symlinked_configured_roots() {
    use std::os::unix::fs::symlink;

    for provider in [
        CaptureProvider::Pi,
        CaptureProvider::OpenCode,
        CaptureProvider::Claude,
        CaptureProvider::Codex,
        CaptureProvider::OpenClaw,
        CaptureProvider::Cline,
        CaptureProvider::OpenHands,
    ] {
        let temp = tempdir();
        let target = temp.path().join("target");
        match configured_root_capability(provider)
            .unwrap()
            .state
            .expected_path_kind()
            .unwrap()
        {
            ConfiguredRootPathKind::Directory => fs::create_dir_all(&target).unwrap(),
            ConfiguredRootPathKind::File => write(&target, b"file"),
        }
        let selected = temp.path().join("selected");
        symlink(&target, &selected).unwrap();
        let report = configured_report(
            context(&temp),
            vec![root("linked", provider, selected)],
            provider,
        );
        assert!(report.sources.is_empty(), "{provider:?}");
        assert_eq!(report.issues.len(), 1, "{provider:?}");
        assert_eq!(report.issues[0].reason, CONFIGURED_ROOT_SYMLINK_REASON);
    }
}

#[test]
fn empty_openhands_current_root_is_accepted_without_content_inference() {
    let temp = tempdir();
    let selected = temp.path().join("openhands-current");
    fs::create_dir_all(&selected).unwrap();

    let report = configured_report(
        context(&temp),
        vec![openhands_root(
            "current",
            selected.clone(),
            ProviderRootKind::OpenHandsCurrentConversations,
        )],
        CaptureProvider::OpenHands,
    );

    assert!(report.issues.is_empty());
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, selected);
    assert_eq!(report.sources[0].source_format, "openhands_cli_file_events");
    assert_eq!(
        route_role(&report.sources[0]),
        b"openhands-current-conversations"
    );
}

#[cfg(unix)]
#[test]
fn every_expander_family_preserves_unavailable_candidates_with_provenance() {
    use std::os::unix::fs::PermissionsExt;

    for (provider, expected_sources) in [
        (CaptureProvider::Pi, 1),
        (CaptureProvider::OpenCode, 1),
        (CaptureProvider::Claude, 1),
        (CaptureProvider::Codex, 3),
        // Root-level OpenClaw unavailability has no safe agent-membership
        // witness. It remains route-less so capture can retain the exact
        // prior alpha/beta membership rather than inventing `main`.
        (CaptureProvider::OpenClaw, 0),
        (CaptureProvider::Cline, 2),
    ] {
        let temp = tempdir();
        let locked = temp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        let original = fs::metadata(&locked).unwrap().permissions();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let selected = locked.join("selected");
        let report = configured_report(
            context(&temp),
            vec![root("unavailable", provider, selected.clone())],
            provider,
        );
        fs::set_permissions(&locked, original).unwrap();

        assert_eq!(report.sources.len(), expected_sources, "{provider:?}");
        assert!(report
            .sources
            .iter()
            .all(|source| source.status == ProviderSourceStatus::Unknown));
        for source in &report.sources {
            assert_configured(source, "unavailable", &selected);
        }
        assert!(!report.issues.is_empty());
    }
}

#[test]
fn automatic_and_configured_equal_paths_adopt_configured_provenance_per_expander() {
    let temp = tempdir();
    let base = context(&temp);
    let home = base.home();
    let fixtures = [
        (
            "grok",
            CaptureProvider::GrokBuild,
            home.join(".grok/sessions"),
            "session/updates.jsonl",
        ),
        (
            "deepseek",
            CaptureProvider::DeepSeekHarness,
            home.join(".dsh/sessions"),
            "a/b/session.jsonl",
        ),
        (
            "pi",
            CaptureProvider::Pi,
            home.join(".pi/agent/sessions"),
            "session.jsonl",
        ),
    ];
    let mut roots = Vec::new();
    for (id, provider, selected, leaf) in fixtures {
        write(&selected.join(leaf), b"{}\n");
        roots.push(root(id, provider, selected));
    }

    let opencode = home.join(".local/share/opencode/opencode.db");
    write(&opencode, b"database");
    roots.push(root("opencode", CaptureProvider::OpenCode, opencode));

    let claude = home.join(".claude");
    write(&claude.join("projects/session.jsonl"), b"{}\n");
    roots.push(root("claude", CaptureProvider::Claude, claude));

    let codex = home.join(".codex");
    write(&codex.join("sessions/session.jsonl"), b"{}\n");
    write(&codex.join("archived_sessions/session.jsonl"), b"{}\n");
    write(&codex.join("history.jsonl"), b"{}\n");
    roots.push(root("codex", CaptureProvider::Codex, codex));

    let openclaw = home.join(".openclaw");
    write(
        &openclaw.join("agents/main/sessions/session.jsonl"),
        b"{}\n",
    );
    roots.push(root("openclaw", CaptureProvider::OpenClaw, openclaw));

    let cline = home.join(".cline/data");
    write(
        &cline.join("tasks/task/api_conversation_history.json"),
        b"[]",
    );
    write(
        &cline.join("sessions/sessions.index.json"),
        br#"{"version":1,"sessions":{}}"#,
    );
    roots.push(root("cline", CaptureProvider::Cline, cline));

    let context = base.with_configured_provider_roots(roots);
    for (provider, expected_sources) in [
        (CaptureProvider::GrokBuild, 1),
        (CaptureProvider::DeepSeekHarness, 1),
        (CaptureProvider::Pi, 1),
        (CaptureProvider::OpenCode, 1),
        (CaptureProvider::Claude, 1),
        (CaptureProvider::Codex, 3),
        (CaptureProvider::OpenClaw, 1),
        (CaptureProvider::Cline, 2),
    ] {
        let report = provider_report(&context, provider);
        assert_eq!(report.sources.len(), expected_sources, "{provider:?}");
        assert!(report
            .sources
            .iter()
            .all(|source| source.route_provenance.configured_root().is_some()));
    }
}

#[test]
fn codex_child_kinds_are_checked_independently_without_hiding_valid_peers() {
    let temp = tempdir();
    let home = temp.path().join("codex");
    write(&home.join("sessions"), b"wrong kind");
    fs::create_dir_all(home.join("archived_sessions")).unwrap();
    fs::create_dir_all(home.join("history.jsonl")).unwrap();
    let report = configured_report(
        context(&temp),
        vec![root("codex", CaptureProvider::Codex, home.clone())],
        CaptureProvider::Codex,
    );
    assert_eq!(report.sources.len(), 3);
    assert_eq!(report.sources[0].path, home.join("sessions"));
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unknown);
    assert_eq!(report.sources[1].path, home.join("archived_sessions"));
    assert_eq!(report.sources[1].status, ProviderSourceStatus::Empty);
    assert_eq!(report.sources[2].path, home.join("history.jsonl"));
    assert_eq!(report.sources[2].status, ProviderSourceStatus::Unknown);
    assert!(report.sources.iter().all(|source| {
        source
            .route_provenance
            .configured_root()
            .is_some_and(|(id, path)| id == "codex" && path == home)
    }));
    assert_eq!(report.issues.len(), 2);
}

#[test]
fn intentional_rows_ignore_configured_roots_even_when_layouts_exist() {
    let temp = tempdir();
    for &provider in INTENTIONAL_AUTOMATIC_EXACT {
        let selected = temp.path().join(provider.as_str());
        fs::create_dir_all(&selected).unwrap();
        let report = configured_report(
            context(&temp),
            vec![root("disabled", provider, selected)],
            provider,
        );
        assert!(report.sources.is_empty(), "{provider:?}");
    }

    let legacy = temp.path().join("legacy");
    write(&legacy.join("v1_conversations/id/event.json"), b"{}");
    let current = temp.path().join("current");
    write(&current.join("id/events/event-00001.json"), b"{}");
    let report = configured_report(
        context(&temp),
        vec![
            openhands_root(
                "legacy",
                legacy,
                ProviderRootKind::OpenHandsLegacyPersistence,
            ),
            openhands_root(
                "current",
                current,
                ProviderRootKind::OpenHandsCurrentConversations,
            ),
        ],
        CaptureProvider::OpenHands,
    );
    assert_eq!(report.sources.len(), 2);
    assert_eq!(report.sources[0].source_format, "openhands_cli_file_events");
    assert_eq!(
        route_role(&report.sources[0]),
        b"openhands-current-conversations"
    );
    assert_eq!(report.sources[1].source_format, "openhands_file_events");
    assert_eq!(
        route_role(&report.sources[1]),
        b"openhands-legacy-persistence"
    );
}

#[test]
fn openhands_configured_legacy_and_nested_current_roots_fail_closed() {
    let temp = tempdir();
    let legacy = temp.path().join("legacy");
    let current = legacy.join("current");
    fs::create_dir_all(&current).unwrap();
    let report = configured_report(
        context(&temp),
        vec![
            openhands_root(
                "legacy",
                legacy,
                ProviderRootKind::OpenHandsLegacyPersistence,
            ),
            openhands_root(
                "current",
                current,
                ProviderRootKind::OpenHandsCurrentConversations,
            ),
        ],
        CaptureProvider::OpenHands,
    );
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::ConfiguredRootConflict
    );

    let current_parent = temp.path().join("current-parent");
    let legacy_child = current_parent.join("legacy-child");
    fs::create_dir_all(&legacy_child).unwrap();
    let report = configured_report(
        context(&temp),
        vec![
            openhands_root(
                "current-parent",
                current_parent,
                ProviderRootKind::OpenHandsCurrentConversations,
            ),
            openhands_root(
                "legacy-child",
                legacy_child,
                ProviderRootKind::OpenHandsLegacyPersistence,
            ),
        ],
        CaptureProvider::OpenHands,
    );
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::ConfiguredRootConflict
    );
}

#[test]
fn openhands_active_automatic_and_configured_nested_roots_fail_closed() {
    let temp = tempdir();
    let base = context(&temp);
    let automatic_legacy = base.home().join(".openhands");
    let configured_current = automatic_legacy.join("conversations");
    write(
        &automatic_legacy.join("v1_conversations/legacy/event.json"),
        b"{}",
    );
    write(
        &configured_current.join("current/events/event-00001.json"),
        b"{}",
    );
    let parent_report = provider_report(
        &base
            .clone()
            .with_configured_provider_roots(vec![openhands_root(
                "configured-current",
                configured_current,
                ProviderRootKind::OpenHandsCurrentConversations,
            )]),
        CaptureProvider::OpenHands,
    );
    assert!(parent_report
        .issues
        .iter()
        .any(|issue| issue.kind == DiscoveryIssueKind::ConfiguredRootConflict));
    assert!(parent_report
        .sources
        .iter()
        .all(|source| source.route_provenance.configured_root().is_none()));

    let automatic_current = temp.path().join("automatic-current");
    let configured_legacy = automatic_current.join("nested-legacy");
    write(
        &automatic_current.join("current/events/event-00001.json"),
        b"{}",
    );
    write(
        &configured_legacy.join("v1_conversations/legacy/event.json"),
        b"{}",
    );
    let child_report = provider_report(
        &base
            .with_env(
                "OPENHANDS_CONVERSATIONS_DIR",
                automatic_current.as_os_str().to_owned(),
            )
            .with_configured_provider_roots(vec![openhands_root(
                "configured-legacy",
                configured_legacy,
                ProviderRootKind::OpenHandsLegacyPersistence,
            )]),
        CaptureProvider::OpenHands,
    );
    assert!(child_report
        .issues
        .iter()
        .any(|issue| issue.kind == DiscoveryIssueKind::ConfiguredRootConflict));
    assert!(child_report
        .sources
        .iter()
        .all(|source| source.route_provenance.configured_root().is_none()));
}

#[test]
fn openhands_equal_automatic_and_configured_paths_reject_opposite_layout_kinds() {
    for (configured_kind, include_legacy, expected_automatic_format) in [
        (
            ProviderRootKind::OpenHandsCurrentConversations,
            true,
            "openhands_file_events",
        ),
        (
            ProviderRootKind::OpenHandsLegacyPersistence,
            false,
            "openhands_cli_file_events",
        ),
    ] {
        let temp = tempdir();
        let selected = temp.path().join("shared-openhands-root");
        if include_legacy {
            write(&selected.join("v1_conversations/legacy/event.json"), b"{}");
        }
        write(&selected.join("current/events/event-00001.json"), b"{}");
        let report = provider_report(
            &context(&temp)
                .with_env("OH_PERSISTENCE_DIR", selected.as_os_str())
                .with_env("OPENHANDS_CONVERSATIONS_DIR", selected.as_os_str())
                .with_configured_provider_roots(vec![openhands_root(
                    "opposite-kind",
                    selected.clone(),
                    configured_kind,
                )]),
            CaptureProvider::OpenHands,
        );

        assert_eq!(report.sources.len(), 1, "{configured_kind:?}");
        assert_eq!(
            report.sources[0].source_format, expected_automatic_format,
            "{configured_kind:?}"
        );
        assert!(report.sources[0]
            .route_provenance
            .configured_root()
            .is_none());
        assert!(report.issues.iter().any(|issue| {
            issue.kind == DiscoveryIssueKind::ConfiguredRootConflict
                && issue.path.as_deref() == Some(selected.as_path())
        }));
    }
}

#[test]
fn openhands_automatic_overlap_suppresses_only_conflicting_configured_root_ids() {
    let temp = tempdir();
    let base = context(&temp);
    let automatic_legacy = base.home().join(".openhands");
    write(
        &automatic_legacy.join("v1_conversations/legacy/event.json"),
        b"{}",
    );
    let conflicting_a = automatic_legacy.join("configured-current-a");
    let conflicting_b = automatic_legacy.join("configured-current-b");
    let healthy = temp.path().join("healthy-disjoint-current");
    for path in [&conflicting_a, &conflicting_b, &healthy] {
        write(&path.join("conversation/events/event-00001.json"), b"{}");
    }

    let report = provider_report(
        &base.with_configured_provider_roots(vec![
            openhands_root(
                "conflicting-a",
                conflicting_a.clone(),
                ProviderRootKind::OpenHandsCurrentConversations,
            ),
            openhands_root(
                "healthy",
                healthy.clone(),
                ProviderRootKind::OpenHandsCurrentConversations,
            ),
            openhands_root(
                "conflicting-b",
                conflicting_b.clone(),
                ProviderRootKind::OpenHandsCurrentConversations,
            ),
        ]),
        CaptureProvider::OpenHands,
    );

    let configured = report
        .sources
        .iter()
        .filter_map(|source| {
            source
                .route_provenance
                .configured_root()
                .map(|(id, root)| (id, root, source.path.as_path()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        configured,
        [("healthy", healthy.as_path(), healthy.as_path())]
    );

    let mut conflict_paths = report
        .issues
        .iter()
        .filter(|issue| issue.kind == DiscoveryIssueKind::ConfiguredRootConflict)
        .map(|issue| issue.path.clone().expect("conflict path"))
        .collect::<Vec<_>>();
    conflict_paths.sort();
    let mut expected_conflict_paths = vec![conflicting_a, conflicting_b];
    expected_conflict_paths.sort();
    assert_eq!(conflict_paths, expected_conflict_paths);
}
