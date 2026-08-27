use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use ctx_agent_integrations::skill::{
    default_noninteractive_agents, detected_agents, explicit_agent_selection, install_target,
    parse_picker_selection, picker_agents, resolve_targets_for_agents, sanitize_skill_name,
    sha256_hex, status_target, PathContext, SkillAgentArg, SkillInstallStatus, SkillMetadata,
    SkillSelectionSource,
};

use super::{install, plan_install_selection, SkillInstallSelectionPlan, SkillSelectionRequest};
use crate::{IntegrationResultFact, ProductIdentity, TargetSelectionFact};

const PRODUCT: ProductIdentity<'static> = ProductIdentity {
    name: "ctx",
    version: "1.0.0-test",
};
const METADATA_FILE: &str = ".ctx-skill.json";

fn resolve_targets(
    agents: &[SkillAgentArg],
    all_agents: bool,
    project: bool,
    context: &PathContext,
) -> anyhow::Result<Vec<ctx_agent_integrations::skill::SkillTarget>> {
    let selected = explicit_agent_selection(agents, all_agents)
        .map(|selection| selection.agents)
        .unwrap_or_else(|| vec![SkillAgentArg::Universal]);
    resolve_targets_for_agents(&selected, project, context)
}

#[test]
fn default_target_is_global_canonical_agents_dir() {
    let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
    let targets = resolve_targets(&[], false, false, &context).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].agent, SkillAgentArg::Universal);
    assert_eq!(
        targets[0].skill_dir,
        PathBuf::from("/home/tester/.agents/skills/ctx")
    );
}

#[test]
fn agent_global_paths_preserve_env_and_xdg_rules() {
    let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"))
        .with_xdg_config_home(PathBuf::from("/xdg"))
        .with_env_override("CODEX_HOME", PathBuf::from("/codex-home"))
        .with_env_override("CLAUDE_CONFIG_DIR", PathBuf::from("/claude-home"))
        .with_env_override("MIMOCODE_HOME", PathBuf::from("/mimocode-home"));
    let targets = resolve_targets(
        &[
            SkillAgentArg::Codex,
            SkillAgentArg::ClaudeCode,
            SkillAgentArg::OpenCode,
            SkillAgentArg::MiMoCode,
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
    assert_eq!(paths["codex"], PathBuf::from("/codex-home/skills/ctx"));
    assert_eq!(
        paths["claude-code"],
        PathBuf::from("/claude-home/skills/ctx")
    );
    assert_eq!(paths["opencode"], PathBuf::from("/xdg/opencode/skills/ctx"));
    assert_eq!(
        paths["mimocode"],
        PathBuf::from("/mimocode-home/config/skills/ctx")
    );
    assert_eq!(paths["amp"], PathBuf::from("/xdg/agents/skills/ctx"));
}

#[test]
fn project_paths_are_agent_specific_and_relative_to_cwd() {
    let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
    let targets = resolve_targets(
        &[
            SkillAgentArg::ClaudeCode,
            SkillAgentArg::Codex,
            SkillAgentArg::MiMoCode,
        ],
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
        PathBuf::from("/repo/.claude/skills/ctx")
    );
    assert_eq!(paths["codex"], PathBuf::from("/repo/.agents/skills/ctx"));
    assert_eq!(paths["mimocode"], PathBuf::from("/repo/.agents/skills/ctx"));
}

#[test]
fn default_selection_includes_universal_and_detected_agent_specific_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::create_dir_all(xdg.join("mimocode")).unwrap();
    let context = PathContext::for_tests(home, temp.path().join("repo")).with_xdg_config_home(xdg);

    assert_eq!(
        detected_agents(&context),
        vec![
            SkillAgentArg::ClaudeCode,
            SkillAgentArg::Codex,
            SkillAgentArg::MiMoCode
        ]
    );

    let SkillInstallSelectionPlan::Selected(selection) = plan_install_selection(
        SkillSelectionRequest {
            agents: &[],
            all_agents: false,
            allow_picker: false,
            project: false,
        },
        &context,
    )
    .unwrap() else {
        panic!("noninteractive selection should not prompt");
    };
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
    let SkillInstallSelectionPlan::Prompt(prompt) = plan_install_selection(
        SkillSelectionRequest {
            agents: &[],
            all_agents: false,
            allow_picker: true,
            project: false,
        },
        &context,
    )
    .unwrap() else {
        panic!("interactive selection should prompt");
    };
    assert_eq!(
        prompt
            .options
            .iter()
            .filter(|option| option.selected_by_default)
            .map(|option| option.agent)
            .collect::<Vec<_>>(),
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
fn picker_visibly_preselects_an_existing_native_copy() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cursor_skill = home.join(".cursor").join("skills").join("ctx");
    fs::create_dir_all(&cursor_skill).unwrap();
    fs::write(cursor_skill.join("SKILL.md"), "existing skill\n").unwrap();
    let context = PathContext::for_tests(home, temp.path().join("repo"))
        .with_env_override("CODEX_HOME", temp.path().join("missing-codex"));

    let SkillInstallSelectionPlan::Prompt(prompt) = plan_install_selection(
        SkillSelectionRequest {
            agents: &[],
            all_agents: false,
            allow_picker: true,
            project: false,
        },
        &context,
    )
    .unwrap() else {
        panic!("interactive selection should prompt");
    };

    let cursor = prompt
        .options
        .iter()
        .find(|option| option.agent == SkillAgentArg::Cursor)
        .unwrap();
    assert!(cursor.selected_by_default);
    assert_eq!(cursor.target.skill_dir, cursor_skill);
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
    assert_eq!(
        parse_picker_selection("mimo-code", options).unwrap(),
        vec![SkillAgentArg::MiMoCode]
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
    assert!(ctx_agent_integrations::skill::ensure_path_inside(
        Path::new("/base"),
        Path::new("/base/../evil")
    )
    .is_err());
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

    install_target(&target, true, true, PRODUCT.version).unwrap();
    assert_eq!(
        status_target(&target).unwrap().status,
        SkillInstallStatus::Current
    );

    fs::write(target.skill_dir.join("SKILL.md"), "old bundled content\n").unwrap();
    let old_hash = sha256_hex(b"old bundled content\n");
    let mut metadata = SkillMetadata::current(PRODUCT.version);
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
fn workflow_facts_are_closed_and_path_free() {
    let temp = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let SkillInstallSelectionPlan::Selected(selection) = plan_install_selection(
        SkillSelectionRequest {
            agents: &[SkillAgentArg::Codex, SkillAgentArg::ClaudeCode],
            all_agents: false,
            allow_picker: false,
            project: false,
        },
        &context,
    )
    .unwrap() else {
        panic!("explicit selection should not prompt");
    };
    let outcome = install(selection, true, false, &context, PRODUCT).unwrap();

    assert_eq!(
        outcome.telemetry.selection,
        Some(TargetSelectionFact::Explicit)
    );
    assert_eq!(outcome.telemetry.resolved_agents, Some(2));
    assert_eq!(outcome.telemetry.result, Some(IntegrationResultFact::Ok));
    assert_eq!(outcome.telemetry.modified_targets, Some(0));
}
