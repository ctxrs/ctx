use std::{collections::BTreeMap, fs};

use super::directory_fence::with_before_remove_hook;
use super::*;

const PRODUCT_VERSION: &str = "1.0.0-test";

fn context(root: &tempfile::TempDir) -> PathContext {
    PathContext::for_tests(root.path().to_owned(), root.path().to_owned())
}

#[cfg(unix)]
fn link_directory(target: &std::path::Path, link: &std::path::Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn link_directory(target: &std::path::Path, link: &std::path::Path) {
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .status()
        .expect("create Windows directory junction");
    assert!(status.success(), "failed to create Windows junction");
}

fn target(agent: SlashCommandAgent, context: &PathContext) -> CommandFileTarget {
    match agent.install_plan(true, context) {
        SlashCommandPlan::File(target) => target,
        SlashCommandPlan::SkillOnly { .. } | SlashCommandPlan::ManualOnly { .. } => {
            panic!("expected file target")
        }
    }
}

fn status_request(agent: SlashCommandAgent) -> SlashCommandStatusRequest {
    SlashCommandStatusRequest {
        agents: vec![agent],
        all_agents: false,
        project: true,
    }
}

fn remove_request(agent: SlashCommandAgent, force: bool) -> SlashCommandRemoveRequest {
    SlashCommandRemoveRequest {
        agents: vec![agent],
        all_agents: false,
        project: true,
        force,
    }
}

fn write_current_metadata(target: &CommandFileTarget, body: &[u8]) {
    let metadata = SlashCommandMetadata {
        schema_version: 1,
        installer: "ctx-cli".to_owned(),
        command_name: COMMAND_NAME.to_owned(),
        files: BTreeMap::from([(target.filename.clone(), sha256_hex(body))]),
        ctx_cli_version: PRODUCT_VERSION.to_owned(),
        installed_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    fs::write(
        target.base_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
}

fn write_legacy_metadata(target: &CommandFileTarget, body: &[u8]) {
    let metadata = SlashCommandMetadata {
        schema_version: 1,
        installer: "ctx-cli".to_owned(),
        command_name: LEGACY_COMMAND_NAME.to_owned(),
        files: BTreeMap::from([(target.legacy_filename(), sha256_hex(body))]),
        ctx_cli_version: "0.9.0".to_owned(),
        installed_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    fs::write(
        target.base_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
}

#[test]
fn project_detection_uses_project_roots_and_explicit_selection_is_exact() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("project");
    fs::create_dir_all(home.join(".gemini")).unwrap();
    fs::create_dir_all(project.join(".qwen")).unwrap();
    let context = PathContext::for_tests(home, project);

    let detected = execute_status(
        SlashCommandStatusRequest {
            agents: Vec::new(),
            all_agents: false,
            project: true,
        },
        &context,
    );
    assert_eq!(detected.selected_agents, 1);
    assert_eq!(detected.results[0].agent, SlashCommandAgent::QwenCode);

    let explicit = execute_status(
        SlashCommandStatusRequest {
            agents: vec![SlashCommandAgent::Codex, SlashCommandAgent::Codex],
            all_agents: false,
            project: true,
        },
        &context,
    );
    assert_eq!(explicit.selected_agents, 1);
    assert_eq!(explicit.results[0].agent, SlashCommandAgent::Codex);
}

#[test]
fn status_distinguishes_missing_current_stale_and_unowned_bundled_bytes() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let target = target(SlashCommandAgent::OpenCode, &context);

    let missing = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(
        missing.results[0].status,
        SlashCommandInstallStatus::Missing
    );

    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.command_path(), target.body.as_bytes()).unwrap();
    let unowned = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(
        unowned.results[0].status,
        SlashCommandInstallStatus::Modified
    );
    assert!(unowned.results[0].force_required);

    write_current_metadata(&target, target.body.as_bytes());
    let current = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(
        current.results[0].status,
        SlashCommandInstallStatus::Current
    );

    let old = b"---\ndescription: old ctx command\n---\n";
    fs::write(target.command_path(), old).unwrap();
    write_current_metadata(&target, old);
    let stale = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(stale.results[0].status, SlashCommandInstallStatus::Stale);
}

#[test]
fn modified_legacy_wins_over_a_current_command() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let target = target(SlashCommandAgent::OpenCode, &context);
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.command_path(), target.body.as_bytes()).unwrap();
    write_current_metadata(&target, target.body.as_bytes());
    fs::write(target.legacy_command_path(), b"local legacy command").unwrap();

    let receipt = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(
        receipt.results[0].status,
        SlashCommandInstallStatus::Modified
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn strictly_owned_current_remove_is_idempotent_and_preserves_siblings() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let install = super::super::SlashCommandInstallRequest {
        agents: vec![SlashCommandAgent::GeminiCli],
        all_agents: false,
        project: true,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    };
    super::super::execute_install(install, &context).unwrap();
    let target = target(SlashCommandAgent::GeminiCli, &context);
    fs::write(target.base_dir.join("keep.toml"), "keep").unwrap();

    let removed = execute_remove(
        remove_request(SlashCommandAgent::GeminiCli, false),
        &context,
    );
    let result = &removed.results[0];
    assert!(result.success && result.current_removed && result.metadata_removed);
    assert_eq!(result.previous_status, SlashCommandInstallStatus::Current);
    assert_eq!(result.status, SlashCommandInstallStatus::Missing);
    assert_eq!(
        fs::read_to_string(target.base_dir.join("keep.toml")).unwrap(),
        "keep"
    );

    let second = execute_remove(
        remove_request(SlashCommandAgent::GeminiCli, false),
        &context,
    );
    assert!(second.results[0].success);
    assert!(second.results[0].already_absent);
    assert!(!second.results[0].modified);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn stale_managed_legacy_is_removed_without_force() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let target = target(SlashCommandAgent::OpenCode, &context);
    let body = b"managed legacy ctx-history command";
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.legacy_command_path(), body).unwrap();
    write_legacy_metadata(&target, body);

    let status = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(status.results[0].status, SlashCommandInstallStatus::Stale);
    let removed = execute_remove(remove_request(SlashCommandAgent::OpenCode, false), &context);
    assert!(removed.results[0].success);
    assert!(removed.results[0].legacy_removed);
    assert!(removed.results[0].metadata_removed);
    assert!(!target.legacy_command_path().exists());
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn unowned_bytes_require_force_and_extra_metadata_entries_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let target = target(SlashCommandAgent::QwenCode, &context);
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.command_path(), target.body.as_bytes()).unwrap();

    let preserved = execute_remove(remove_request(SlashCommandAgent::QwenCode, false), &context);
    assert!(!preserved.results[0].success);
    assert!(preserved.results[0].force_required);
    assert!(target.command_path().is_file());

    let mut metadata = SlashCommandMetadata::current(&target, PRODUCT_VERSION);
    metadata
        .files
        .insert("unrelated.md".to_owned(), sha256_hex(b"unrelated"));
    fs::write(
        target.base_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let forced = execute_remove(remove_request(SlashCommandAgent::QwenCode, true), &context);
    assert!(forced.results[0].success);
    assert!(!forced.results[0].force_required);
    assert!(forced.results[0].current_removed);
    assert!(!forced.results[0].metadata_removed);
    assert!(target.base_dir.join(METADATA_FILE).is_file());
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn force_does_not_remove_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let target = target(SlashCommandAgent::OpenCode, &context);
    fs::create_dir_all(&target.base_dir).unwrap();
    let external = root.path().join("external.md");
    fs::write(&external, "external").unwrap();
    std::os::unix::fs::symlink(&external, target.command_path()).unwrap();

    let receipt = execute_remove(remove_request(SlashCommandAgent::OpenCode, true), &context);
    assert!(!receipt.results[0].success);
    assert!(!receipt.results[0].force_required);
    assert!(receipt.results[0]
        .error
        .as_deref()
        .unwrap()
        .contains("symlink"));
    assert_eq!(fs::read_to_string(external).unwrap(), "external");
    assert!(fs::symlink_metadata(target.command_path())
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(any(unix, windows))]
#[test]
fn status_and_force_remove_reject_a_linked_parent_chain() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let outside = root.path().join("outside-opencode");
    let outside_command = outside.join("commands/ctx.md");
    fs::create_dir_all(outside_command.parent().unwrap()).unwrap();
    fs::write(&outside_command, b"outside command").unwrap();
    link_directory(&outside, &root.path().join(".opencode"));

    let status = execute_status(status_request(SlashCommandAgent::OpenCode), &context);
    assert_eq!(status.failed, 1);
    assert!(!status.results[0].success);
    assert!(!status.results[0].force_required);
    assert!(status.results[0]
        .error
        .as_deref()
        .unwrap()
        .contains("symlink or reparse point"));

    let removed = execute_remove(remove_request(SlashCommandAgent::OpenCode, true), &context);
    assert_eq!(removed.failed, 1);
    assert!(!removed.results[0].success);
    assert!(!removed.results[0].force_required);
    assert!(!removed.results[0].modified);
    assert_eq!(fs::read(&outside_command).unwrap(), b"outside command");
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn ancestor_swap_after_validation_cannot_redirect_forced_removal() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let install = super::super::SlashCommandInstallRequest {
        agents: vec![SlashCommandAgent::OpenCode],
        all_agents: false,
        project: true,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    };
    super::super::execute_install(install, &context).unwrap();
    let target = target(SlashCommandAgent::OpenCode, &context);
    let command_path = target.command_path();
    let original_parent = root.path().join(".opencode");
    let displaced_parent = root.path().join("displaced-opencode");
    let hook_displaced_parent = displaced_parent.clone();
    let replacement_command = command_path.clone();
    let swapped_command = command_path.clone();
    let receipt = with_before_remove_hook(
        move |path| {
            if path == swapped_command {
                fs::rename(&original_parent, &hook_displaced_parent).unwrap();
                fs::create_dir_all(replacement_command.parent().unwrap()).unwrap();
                fs::write(&replacement_command, b"replacement command").unwrap();
            }
        },
        || execute_remove(remove_request(SlashCommandAgent::OpenCode, true), &context),
    );

    let result = &receipt.results[0];
    assert!(!result.success);
    assert_eq!(result.status, SlashCommandInstallStatus::Modified);
    assert!(!result.current_removed);
    assert!(!result.force_required);
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("directory identity changed"));
    assert_eq!(fs::read(&command_path).unwrap(), b"replacement command");
    assert_eq!(
        fs::read(displaced_parent.join("commands/ctx.md")).unwrap(),
        target.body.as_bytes()
    );
}

#[cfg(windows)]
#[test]
fn held_directory_handles_block_ancestor_swap_after_validation() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let install = super::super::SlashCommandInstallRequest {
        agents: vec![SlashCommandAgent::OpenCode],
        all_agents: false,
        project: true,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    };
    super::super::execute_install(install, &context).unwrap();
    let target = target(SlashCommandAgent::OpenCode, &context);
    let command_path = target.command_path();
    let original_parent = root.path().join(".opencode");
    let displaced_parent = root.path().join("displaced-opencode");
    let swap_blocked = Arc::new(AtomicBool::new(false));
    let hook_swap_blocked = Arc::clone(&swap_blocked);
    let swapped_command = command_path.clone();

    let receipt = with_before_remove_hook(
        move |path| {
            if path == swapped_command {
                hook_swap_blocked.store(
                    fs::rename(&original_parent, &displaced_parent).is_err(),
                    Ordering::SeqCst,
                );
            }
        },
        || execute_remove(remove_request(SlashCommandAgent::OpenCode, true), &context),
    );

    assert!(swap_blocked.load(Ordering::SeqCst));
    assert!(receipt.results[0].success);
    assert!(receipt.results[0].current_removed);
    assert!(!command_path.exists());
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn compare_and_remove_race_preserves_concurrent_bytes() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let install = super::super::SlashCommandInstallRequest {
        agents: vec![SlashCommandAgent::OpenCode],
        all_agents: false,
        project: true,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    };
    super::super::execute_install(install, &context).unwrap();
    let target = target(SlashCommandAgent::OpenCode, &context);
    let command_path = target.command_path();
    let changed_path = command_path.clone();

    let receipt = with_before_remove_hook(
        move |path| {
            if path == changed_path {
                fs::write(path, b"concurrently changed").unwrap();
            }
        },
        || execute_remove(remove_request(SlashCommandAgent::OpenCode, false), &context),
    );
    assert!(!receipt.results[0].success);
    assert_eq!(fs::read(command_path).unwrap(), b"concurrently changed");
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn final_reinspection_reports_recreated_entries_and_preserves_removal_flags() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    let install = super::super::SlashCommandInstallRequest {
        agents: vec![SlashCommandAgent::OpenCode],
        all_agents: false,
        project: true,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    };
    super::super::execute_install(install, &context).unwrap();
    let target = target(SlashCommandAgent::OpenCode, &context);
    fs::write(target.legacy_command_path(), b"local legacy command").unwrap();
    let command_path = target.command_path();
    let recreated_command = command_path.clone();
    let metadata_path = target.base_dir.join(METADATA_FILE);

    let receipt = with_before_remove_hook(
        move |path| {
            if path == metadata_path {
                fs::write(&recreated_command, b"concurrently recreated current").unwrap();
            }
        },
        || execute_remove(remove_request(SlashCommandAgent::OpenCode, true), &context),
    );

    let result = &receipt.results[0];
    assert!(!result.success);
    assert_eq!(result.status, SlashCommandInstallStatus::Modified);
    assert!(result.modified);
    assert!(result.current_removed);
    assert!(result.legacy_removed);
    assert!(result.metadata_removed);
    assert!(!result.force_required);
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("changed during removal"));
    assert_eq!(
        fs::read(command_path).unwrap(),
        b"concurrently recreated current"
    );
    assert!(!target.legacy_command_path().exists());
    assert!(!target.base_dir.join(METADATA_FILE).exists());
}

#[test]
fn informational_targets_are_successful_non_mutating_remove_states() {
    let root = tempfile::tempdir().unwrap();
    let context = context(&root);
    for (agent, expected) in [
        (
            SlashCommandAgent::Codex,
            SlashCommandInstallStatus::SkillOnly,
        ),
        (
            SlashCommandAgent::Continue,
            SlashCommandInstallStatus::ManualOnly,
        ),
    ] {
        let receipt = execute_remove(remove_request(agent, true), &context);
        let result = &receipt.results[0];
        assert!(result.success && result.already_absent && !result.modified);
        assert_eq!(result.status, expected);
    }
    assert!(!root.path().join(".codex").exists());
    assert!(!root.path().join(".continue").exists());
}
