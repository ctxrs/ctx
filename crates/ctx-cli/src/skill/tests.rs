use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use super::{
    agents::{picker_agents, SkillAgentArg},
    install::{status_target, write_skill_dir, SkillInstallStatus, SkillMetadata},
    paths::{ensure_path_inside, sanitize_skill_name, sha256_hex, PathContext},
    selection::{
        default_noninteractive_agents, default_picker_agents, detected_agents,
        install_agent_selection, parse_picker_selection, SkillSelectionSource,
    },
    target::resolve_targets,
    SkillArgs, SkillCommand, SkillInstallArgs, BUNDLED_SKILL_NAME, METADATA_FILE,
};
use crate::analytics;

#[test]
fn default_target_is_global_canonical_agents_dir() {
    let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
    let targets = resolve_targets(&[], false, false, &context).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].agent, SkillAgentArg::Universal);
    assert_eq!(
        targets[0].skill_dir,
        PathBuf::from("/home/tester/.agents/skills/ctx-agent-history-search")
    );
}

#[test]
fn agent_global_paths_preserve_env_and_xdg_rules() {
    let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"))
        .with_xdg_config_home(PathBuf::from("/xdg"))
        .with_env_override("CODEX_HOME", PathBuf::from("/codex-home"))
        .with_env_override("CLAUDE_CONFIG_DIR", PathBuf::from("/claude-home"));
    let targets = resolve_targets(
        &[
            SkillAgentArg::Codex,
            SkillAgentArg::ClaudeCode,
            SkillAgentArg::OpenCode,
            SkillAgentArg::Amp,
        ],
        false,
        false,
        &context,
    )
    .unwrap();
    let paths = targets
        .iter()
        .map(|target| (target.agent.id(), target.skill_dir.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        paths["codex"],
        PathBuf::from("/codex-home/skills/ctx-agent-history-search")
    );
    assert_eq!(
        paths["claude-code"],
        PathBuf::from("/claude-home/skills/ctx-agent-history-search")
    );
    assert_eq!(
        paths["opencode"],
        PathBuf::from("/xdg/opencode/skills/ctx-agent-history-search")
    );
    assert_eq!(
        paths["amp"],
        PathBuf::from("/xdg/agents/skills/ctx-agent-history-search")
    );
}

#[test]
fn project_paths_are_agent_specific_and_relative_to_cwd() {
    let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
    let targets = resolve_targets(
        &[SkillAgentArg::ClaudeCode, SkillAgentArg::Codex],
        false,
        true,
        &context,
    )
    .unwrap();
    let paths = targets
        .iter()
        .map(|target| (target.agent.id(), target.skill_dir.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        paths["claude-code"],
        PathBuf::from("/repo/.claude/skills/ctx-agent-history-search")
    );
    assert_eq!(
        paths["codex"],
        PathBuf::from("/repo/.agents/skills/ctx-agent-history-search")
    );
}

#[test]
fn default_selection_includes_universal_and_detected_agent_specific_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    let context = PathContext::for_tests(home, temp.path().join("repo"));

    assert_eq!(
        detected_agents(&context),
        vec![SkillAgentArg::ClaudeCode, SkillAgentArg::Codex]
    );

    let selection = install_agent_selection(
        &SkillInstallArgs {
            agent: Vec::new(),
            all_agents: false,
            project: false,
            json: true,
            force: false,
        },
        &context,
    )
    .unwrap();
    assert_eq!(selection.source, SkillSelectionSource::Detected);
    assert_eq!(
        selection.agents,
        vec![SkillAgentArg::Universal, SkillAgentArg::ClaudeCode]
    );
}

#[test]
fn picker_defaults_to_universal_when_nothing_detected() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"))
        .with_env_override("CODEX_HOME", temp.path().join("missing-codex"));
    assert_eq!(
        default_picker_agents(&context),
        vec![SkillAgentArg::Universal]
    );
    assert_eq!(
        default_noninteractive_agents(&context),
        (
            vec![SkillAgentArg::Universal],
            SkillSelectionSource::Fallback
        )
    );
}

#[test]
fn picker_selection_accepts_numbers_names_and_all() {
    let options = picker_agents();
    assert_eq!(
        parse_picker_selection("1,2 claude", options).unwrap(),
        vec![SkillAgentArg::Universal, SkillAgentArg::ClaudeCode]
    );
    assert_eq!(
        parse_picker_selection("cursor universal", options).unwrap(),
        vec![SkillAgentArg::Cursor, SkillAgentArg::Universal]
    );
    assert_eq!(parse_picker_selection("all", options).unwrap(), options);
    assert!(parse_picker_selection("99", options).is_err());
    assert!(parse_picker_selection("not-an-agent", options).is_err());
}

#[test]
fn sanitize_blocks_path_traversal_shapes() {
    assert_eq!(
        sanitize_skill_name("../Ctx Agent History Search!!").unwrap(),
        "ctx-agent-history-search"
    );
    assert!(sanitize_skill_name("..").is_err());
    assert!(ensure_path_inside(Path::new("/base"), Path::new("/base/../evil")).is_err());
}

#[test]
fn status_distinguishes_current_stale_modified_and_missing() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = resolve_targets(&[], false, false, &context)
        .unwrap()
        .remove(0);

    assert_eq!(
        status_target(&target).unwrap().status,
        SkillInstallStatus::Missing
    );

    write_skill_dir(&target).unwrap();
    assert_eq!(
        status_target(&target).unwrap().status,
        SkillInstallStatus::Current
    );

    fs::write(target.skill_dir.join("SKILL.md"), "old bundled content\n").unwrap();
    let old_hash = sha256_hex(b"old bundled content\n");
    let mut metadata = SkillMetadata::current();
    metadata.skill_hash = old_hash;
    fs::write(
        target.skill_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    assert_eq!(
        status_target(&target).unwrap().status,
        SkillInstallStatus::Stale
    );

    fs::write(target.skill_dir.join("SKILL.md"), "local edits\n").unwrap();
    assert_eq!(
        status_target(&target).unwrap().status,
        SkillInstallStatus::Modified
    );
}

#[test]
fn analytics_properties_are_coarse_and_path_free() {
    let args = SkillArgs {
        command: SkillCommand::Install(SkillInstallArgs {
            agent: vec![SkillAgentArg::Codex, SkillAgentArg::ClaudeCode],
            all_agents: false,
            project: true,
            json: true,
            force: false,
        }),
    };
    let mut properties = analytics::empty_properties();
    args.add_initial_analytics(&mut properties);

    assert_eq!(properties["skill_action"], "install");
    assert_eq!(properties["skill_name"], BUNDLED_SKILL_NAME);
    assert_eq!(properties["skill_scope"], "project");
    assert_eq!(properties["target_agent_group"], "explicit");
    for key in properties.keys() {
        assert!(
            !key.contains("path") && !key.contains("home") && !key.contains("dir"),
            "unexpected path-like analytics key: {key}"
        );
    }
}
