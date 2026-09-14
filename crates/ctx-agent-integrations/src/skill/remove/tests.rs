use std::fs;

use super::*;
use crate::skill::{
    default_agent_selection, explicit_agent_selection, install_target, single_target, PathContext,
    SkillAgentArg, SkillSelectionSource, BUNDLED_SKILL_BODY,
};

const RELEASED_LEGACY_SKILL: &[u8] = include_bytes!("../testdata/legacy_skill_v0_17_0.md");

fn fixture() -> (tempfile::TempDir, PathContext, SkillTarget) {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().join("home"), root.path().join("repo"));
    let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
    (root, context, target)
}

fn write_managed_copy(dir: &Path, name: &str, body: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
    let metadata = SkillMetadata {
        schema_version: 1,
        installer: "ctx-cli".to_owned(),
        skill_name: name.to_owned(),
        skill_hash: sha256_hex(body),
        ctx_cli_version: "0.9.0".to_owned(),
        installed_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    fs::write(
        dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
}

#[test]
fn missing_remove_is_idempotent_and_does_not_create_parent_dirs() {
    let (_root, _context, target) = fixture();

    for _ in 0..2 {
        let result = remove_target(&target, false).unwrap();
        assert!(result.success);
        assert!(result.already_absent);
        assert!(!result.removed);
        assert_eq!(result.status, SkillInstallStatus::Missing);
    }
    assert!(!target.skill_dir.exists());
}

#[test]
fn current_and_metadata_owned_stale_copies_are_removed_without_force() {
    let (_root, _context, target) = fixture();
    install_target(&target, false, true, "1.0.0").unwrap();
    fs::write(target.skill_dir.join("notes.txt"), "keep").unwrap();

    let current = remove_target(&target, false).unwrap();

    assert!(current.success);
    assert!(current.removed_current);
    assert!(!target.skill_dir.join("SKILL.md").exists());
    assert!(!target.skill_dir.join(METADATA_FILE).exists());
    assert_eq!(
        fs::read_to_string(target.skill_dir.join("notes.txt")).unwrap(),
        "keep"
    );

    write_managed_copy(&target.skill_dir, "ctx", b"old managed skill\n");
    let stale = remove_target(&target, false).unwrap();
    assert_eq!(stale.previous_status, SkillInstallStatus::Stale);
    assert!(stale.success);
    assert!(stale.removed_current);
}

#[test]
fn exact_bundled_bytes_without_strict_metadata_require_force() {
    let (_root, _context, target) = fixture();
    fs::create_dir_all(&target.skill_dir).unwrap();
    fs::write(target.skill_dir.join("SKILL.md"), BUNDLED_SKILL_BODY).unwrap();
    fs::write(target.skill_dir.join(METADATA_FILE), b"malformed metadata").unwrap();

    let preserved = remove_target(&target, false).unwrap();
    assert!(!preserved.success);
    assert!(preserved.force_required);
    assert_eq!(preserved.previous_status, SkillInstallStatus::Stale);
    assert!(target.skill_dir.join("SKILL.md").is_file());

    let forced = remove_target(&target, true).unwrap();
    assert!(forced.success);
    assert!(forced.removed_current);
    assert!(!target.skill_dir.join("SKILL.md").exists());
    assert_eq!(
        fs::read(target.skill_dir.join(METADATA_FILE)).unwrap(),
        b"malformed metadata"
    );
}

#[test]
fn matching_hash_with_foreign_metadata_is_modified_and_preserved() {
    let (_root, _context, target) = fixture();
    let body = b"foreign managed-looking skill\n";
    fs::create_dir_all(&target.skill_dir).unwrap();
    fs::write(target.skill_dir.join("SKILL.md"), body).unwrap();
    let metadata = SkillMetadata {
        schema_version: 1,
        installer: "another-installer".to_owned(),
        skill_name: "ctx".to_owned(),
        skill_hash: sha256_hex(body),
        ctx_cli_version: "1.0.0".to_owned(),
        installed_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    let metadata_body = serde_json::to_vec_pretty(&metadata).unwrap();
    fs::write(target.skill_dir.join(METADATA_FILE), &metadata_body).unwrap();

    assert_eq!(
        status_target(&target).unwrap().status,
        SkillInstallStatus::Modified
    );
    let preserved = remove_target(&target, false).unwrap();
    assert!(!preserved.success);
    assert!(target.skill_dir.join("SKILL.md").is_file());

    let forced = remove_target(&target, true).unwrap();
    assert!(forced.success);
    assert!(!target.skill_dir.join("SKILL.md").exists());
    assert_eq!(
        fs::read(target.skill_dir.join(METADATA_FILE)).unwrap(),
        metadata_body
    );
}

#[test]
fn current_and_legacy_are_both_preflighted_before_any_mutation() {
    let (_root, _context, target) = fixture();
    install_target(&target, false, true, "1.0.0").unwrap();
    let legacy_dir = legacy_skill_dir(&target).unwrap();
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(legacy_dir.join("SKILL.md"), b"local legacy edits\n").unwrap();
    fs::write(legacy_dir.join("notes.txt"), b"keep legacy sibling").unwrap();

    let preserved = remove_target(&target, false).unwrap();

    assert!(!preserved.success);
    assert!(preserved.force_required);
    assert!(target.skill_dir.join("SKILL.md").is_file());
    assert!(legacy_dir.join("SKILL.md").is_file());

    let forced = remove_target(&target, true).unwrap();
    assert!(forced.success);
    assert!(forced.removed_current);
    assert!(forced.removed_legacy);
    assert!(!target.skill_dir.join("SKILL.md").exists());
    assert!(!legacy_dir.join("SKILL.md").exists());
    assert_eq!(
        fs::read(legacy_dir.join("notes.txt")).unwrap(),
        b"keep legacy sibling"
    );
}

#[test]
fn released_metadata_free_legacy_snapshot_is_the_ownership_exception() {
    let (_root, _context, target) = fixture();
    let legacy_dir = legacy_skill_dir(&target).unwrap();
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(legacy_dir.join("SKILL.md"), RELEASED_LEGACY_SKILL).unwrap();

    let result = remove_target(&target, false).unwrap();

    assert!(result.success);
    assert!(result.removed_legacy);
    assert!(!legacy_dir.join("SKILL.md").exists());
}

#[test]
fn default_remove_includes_safe_existing_native_targets_without_a_picker() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().join("home"), root.path().join("repo"));
    let cursor = single_target(SkillAgentArg::Cursor, false, &context).unwrap();
    write_managed_copy(&cursor.skill_dir, "ctx", b"managed cursor skill\n");

    let receipt = execute_remove(
        SkillRemoveRequest {
            selection: default_agent_selection(&context),
            project: false,
            force: false,
        },
        &context,
    )
    .unwrap();

    assert_eq!(receipt.selection.source, SkillSelectionSource::Fallback);
    assert!(receipt.selection.agents.contains(&SkillAgentArg::Cursor));
    assert!(!cursor.skill_dir.join("SKILL.md").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn compare_and_remove_race_preserves_the_concurrent_edit() {
    let (_root, _context, target) = fixture();
    install_target(&target, false, true, "1.0.0").unwrap();
    let skill_file = target.skill_dir.join("SKILL.md");
    let changed_file = skill_file.clone();

    let result = with_before_skill_remove_hook(
        move |_| fs::write(&changed_file, b"concurrent local edit\n").unwrap(),
        || remove_target(&target, false),
    )
    .unwrap();

    assert!(!result.success);
    assert!(!result.force_required);
    assert!(result.error.unwrap().contains("concurrently changed"));
    assert_eq!(fs::read(skill_file).unwrap(), b"concurrent local edit\n");
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn concurrently_changed_owned_metadata_is_preserved() {
    let (_root, _context, target) = fixture();
    install_target(&target, false, true, "1.0.0").unwrap();
    let snapshot = snapshot_copy(&target.authority_root, &target.skill_dir, false)
        .unwrap()
        .expect("installed skill has a snapshot");
    fs::remove_file(target.skill_dir.join("SKILL.md")).unwrap();
    let metadata_path = target.skill_dir.join(METADATA_FILE);
    let changed_metadata = metadata_path.clone();

    let removed = with_before_skill_remove_hook(
        move |_| fs::write(&changed_metadata, b"concurrent foreign metadata").unwrap(),
        || remove_snapshot(&snapshot),
    )
    .unwrap();

    assert!(!removed);
    assert_eq!(
        fs::read(metadata_path).unwrap(),
        b"concurrent foreign metadata"
    );
}

#[cfg(unix)]
#[test]
fn force_does_not_follow_a_symlinked_skill_file() {
    let (root, _context, target) = fixture();
    fs::create_dir_all(&target.skill_dir).unwrap();
    let outside = root.path().join("outside-skill.md");
    fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, target.skill_dir.join("SKILL.md")).unwrap();

    let error = remove_target(&target, true).unwrap_err();

    assert!(format!("{error:#}").contains("not a regular file"));
    assert_eq!(fs::read(outside).unwrap(), b"outside");
    assert!(target.skill_dir.join("SKILL.md").is_symlink());
}

#[test]
fn target_local_safety_failure_preserves_completed_and_later_results() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().join("home"), root.path().join("repo"));
    let universal = single_target(SkillAgentArg::Universal, false, &context).unwrap();
    let codex = single_target(SkillAgentArg::Codex, false, &context).unwrap();
    let cursor = single_target(SkillAgentArg::Cursor, false, &context).unwrap();
    write_managed_copy(&universal.skill_dir, "ctx", b"managed universal skill\n");
    fs::create_dir_all(codex.skill_dir.join("SKILL.md")).unwrap();
    write_managed_copy(&cursor.skill_dir, "ctx", b"managed cursor skill\n");

    let receipt = execute_remove(
        SkillRemoveRequest {
            selection: explicit_agent_selection(
                &[
                    SkillAgentArg::Universal,
                    SkillAgentArg::Codex,
                    SkillAgentArg::Cursor,
                ],
                false,
            )
            .unwrap(),
            project: false,
            force: true,
        },
        &context,
    )
    .unwrap();

    assert_eq!(receipt.results.len(), 3);
    assert_eq!(receipt.failed, 1);
    assert_eq!(receipt.removed_targets, 2);
    assert!(receipt.results[0].success && receipt.results[0].removed);
    assert!(!receipt.results[1].success);
    assert!(receipt.results[1]
        .error
        .as_deref()
        .unwrap()
        .contains("not a regular file"));
    assert!(receipt.results[2].success && receipt.results[2].removed);
    assert!(!universal.skill_dir.join("SKILL.md").exists());
    assert!(codex.skill_dir.join("SKILL.md").is_dir());
    assert!(!cursor.skill_dir.join("SKILL.md").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn force_rejects_an_ancestor_swap_without_touching_the_replacement() {
    let (root, _context, target) = fixture();
    install_target(&target, false, true, "1.0.0").unwrap();
    let skill_file = target.skill_dir.join("SKILL.md");
    let original_ancestor = target.base_dir.parent().unwrap().to_path_buf();
    let displaced_ancestor = root.path().join("displaced-agents");
    let replacement_skill = skill_file.clone();
    let trigger = skill_file.clone();
    let displaced_for_hook = displaced_ancestor.clone();

    let result = with_before_skill_remove_hook(
        move |path| {
            if path == trigger {
                fs::rename(&original_ancestor, &displaced_for_hook).unwrap();
                fs::create_dir_all(replacement_skill.parent().unwrap()).unwrap();
                fs::write(&replacement_skill, b"replacement outside authority\n").unwrap();
            }
        },
        || remove_target(&target, true),
    )
    .unwrap();

    assert!(!result.success);
    assert_eq!(result.status, SkillInstallStatus::Modified);
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("directory identity changed"));
    assert_eq!(
        fs::read(&skill_file).unwrap(),
        b"replacement outside authority\n"
    );
    assert_eq!(
        fs::read(displaced_ancestor.join("skills/ctx/SKILL.md")).unwrap(),
        BUNDLED_SKILL_BODY.as_bytes()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn partial_current_removal_reinspects_status_after_a_legacy_race() {
    let (_root, _context, target) = fixture();
    install_target(&target, false, true, "1.0.0").unwrap();
    let legacy_dir = legacy_skill_dir(&target).unwrap();
    write_managed_copy(
        &legacy_dir,
        "ctx-agent-history-search",
        b"managed legacy skill\n",
    );
    let legacy_file = legacy_dir.join("SKILL.md");
    let changed_legacy = legacy_file.clone();

    let result = with_before_skill_remove_hook(
        move |path| {
            if path == changed_legacy {
                fs::write(path, b"concurrent legacy edit\n").unwrap();
            }
        },
        || remove_target(&target, false),
    )
    .unwrap();

    assert!(!result.success);
    assert_eq!(result.previous_status, SkillInstallStatus::Stale);
    assert_eq!(result.status, SkillInstallStatus::Modified);
    assert!(result.removed_current);
    assert!(!result.removed_legacy);
    assert!(!target.skill_dir.join("SKILL.md").exists());
    assert_eq!(fs::read(legacy_file).unwrap(), b"concurrent legacy edit\n");
}
