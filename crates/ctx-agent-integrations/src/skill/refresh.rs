use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::{
    bundled_hash,
    install::{
        is_windows_reparse_point, legacy_skill_dir, metadata_manages_hash,
        read_optional_regular_file,
    },
    install_target, single_target, status_target, PathContext, SkillAgentArg, SkillInstallStatus,
    SkillTarget, METADATA_FILE,
};

const MAX_AUTOMATIC_SKILL_BYTES: u64 = 1024 * 1024;
const MAX_AUTOMATIC_METADATA_BYTES: u64 = 64 * 1024;

pub fn existing_managed_skill_refresh_required(context: &PathContext) -> bool {
    distinct_global_targets(context)
        .iter()
        .any(|target| target_refresh_required(target).unwrap_or(false))
}

pub fn refresh_existing_managed_skills(context: &PathContext, product_version: &str) {
    for target in distinct_global_targets(context) {
        if !target_refresh_required(&target).unwrap_or(false) {
            continue;
        }
        let _ = install_target(&target, false, false, product_version);
    }
}

fn distinct_global_targets(context: &PathContext) -> Vec<SkillTarget> {
    let mut skill_dirs = Vec::<PathBuf>::new();
    let mut targets = Vec::new();
    for agent in SkillAgentArg::ALL.iter().copied() {
        let Ok(target) = single_target(agent, false, context) else {
            continue;
        };
        if skill_dirs.contains(&target.skill_dir) {
            continue;
        }
        skill_dirs.push(target.skill_dir.clone());
        targets.push(target);
    }
    targets
}

fn target_refresh_required(target: &SkillTarget) -> Result<bool> {
    if !automatic_target_files_are_bounded(target) {
        return Ok(false);
    }
    let status = status_target(target)?;
    if status.legacy_status == Some(SkillInstallStatus::Modified) {
        return Ok(false);
    }
    let has_managed_legacy = status.legacy_status == Some(SkillInstallStatus::Stale);
    let metadata_path = target.skill_dir.join(METADATA_FILE);
    let current_metadata_absent = read_optional_regular_file(&metadata_path)?.is_none();
    let Some(installed_hash) = status.installed_hash.as_deref() else {
        // A current-name metadata file without its body is an ambiguous pair,
        // even when a legacy copy could otherwise authorize migration.
        return Ok(current_metadata_absent && has_managed_legacy);
    };
    if !metadata_manages_hash(status.metadata.as_ref(), installed_hash) {
        // This is the one unambiguous journal-free recovery state: migration
        // published the exact current body, metadata publication failed, and
        // the recognized legacy copy remains to authorize a retry.
        return Ok(current_metadata_absent
            && has_managed_legacy
            && installed_hash == bundled_hash());
    }
    Ok(installed_hash != bundled_hash() || has_managed_legacy)
}

fn automatic_target_files_are_bounded(target: &SkillTarget) -> bool {
    let Ok(legacy_dir) = legacy_skill_dir(target) else {
        return false;
    };
    [
        (target.skill_dir.join("SKILL.md"), MAX_AUTOMATIC_SKILL_BYTES),
        (
            target.skill_dir.join(METADATA_FILE),
            MAX_AUTOMATIC_METADATA_BYTES,
        ),
        (legacy_dir.join("SKILL.md"), MAX_AUTOMATIC_SKILL_BYTES),
        (legacy_dir.join(METADATA_FILE), MAX_AUTOMATIC_METADATA_BYTES),
    ]
    .into_iter()
    .all(|(path, max_bytes)| optional_regular_file_is_bounded(&path, max_bytes))
}

fn optional_regular_file_is_bounded(path: &Path, max_bytes: u64) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            metadata.file_type().is_file()
                && !is_windows_reparse_point(&metadata)
                && metadata.len() <= max_bytes
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::skill::{
        sha256_hex, SkillMetadata, BUNDLED_SKILL_BODY, BUNDLED_SKILL_NAME,
        LEGACY_BUNDLED_SKILL_NAME,
    };

    const RELEASED_LEGACY_SKILL_V0_17_0: &[u8] = include_bytes!("testdata/legacy_skill_v0_17_0.md");
    const TEST_PRODUCT_VERSION: &str = "1.2.3";

    fn test_context(root: &tempfile::TempDir) -> PathContext {
        PathContext::for_tests(root.path().join("home"), root.path().join("repo"))
    }

    fn metadata_body(
        installer: &str,
        skill_name: &str,
        managed_body: &[u8],
        padding_bytes: usize,
    ) -> Vec<u8> {
        let metadata = SkillMetadata {
            schema_version: 1,
            installer: installer.to_owned(),
            skill_name: skill_name.to_owned(),
            skill_hash: sha256_hex(managed_body),
            ctx_cli_version: "0.1.0".to_owned(),
            installed_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let mut value = serde_json::to_value(metadata).unwrap();
        if padding_bytes > 0 {
            value.as_object_mut().unwrap().insert(
                "padding".to_owned(),
                serde_json::Value::String("x".repeat(padding_bytes)),
            );
        }
        serde_json::to_vec_pretty(&value).unwrap()
    }

    fn write_owned_current(target: &SkillTarget, body: &[u8]) {
        fs::create_dir_all(&target.skill_dir).unwrap();
        fs::write(target.skill_dir.join("SKILL.md"), body).unwrap();
        fs::write(
            target.skill_dir.join(METADATA_FILE),
            metadata_body("ctx-cli", BUNDLED_SKILL_NAME, body, 0),
        )
        .unwrap();
    }

    fn write_owned_legacy(target: &SkillTarget, body: &[u8], padding_bytes: usize) -> PathBuf {
        let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("SKILL.md"), body).unwrap();
        fs::write(
            legacy_dir.join(METADATA_FILE),
            metadata_body("ctx-cli", LEGACY_BUNDLED_SKILL_NAME, body, padding_bytes),
        )
        .unwrap();
        legacy_dir
    }

    fn managed_file_paths(target: &SkillTarget) -> Vec<PathBuf> {
        let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        vec![
            target.skill_dir.join("SKILL.md"),
            target.skill_dir.join(METADATA_FILE),
            legacy_dir.join("SKILL.md"),
            legacy_dir.join(METADATA_FILE),
        ]
    }

    fn optional_test_file(path: &Path) -> Option<Vec<u8>> {
        match fs::read(path) {
            Ok(body) => Some(body),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => panic!("read {}: {error}", path.display()),
        }
    }

    fn assert_automatic_refresh_preserves(
        context: &PathContext,
        target: &SkillTarget,
        scenario: &str,
    ) {
        let paths = managed_file_paths(target);
        let before = paths
            .iter()
            .map(|path| optional_test_file(path))
            .collect::<Vec<_>>();

        assert!(
            !existing_managed_skill_refresh_required(context),
            "{scenario}"
        );
        refresh_existing_managed_skills(context, TEST_PRODUCT_VERSION);

        let after = paths
            .iter()
            .map(|path| optional_test_file(path))
            .collect::<Vec<_>>();
        assert_eq!(after, before, "{scenario}");
        for path in paths {
            let lock_name = format!(
                ".{}.ctx-agent-integrations.lock",
                path.file_name().unwrap().to_string_lossy()
            );
            assert!(!path.with_file_name(lock_name).exists(), "{scenario}");
        }
    }

    #[test]
    fn stale_owned_current_skill_is_refreshed() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
        write_owned_current(&target, b"previous managed skill\n");

        assert!(existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        let metadata: SkillMetadata =
            serde_json::from_slice(&fs::read(target.skill_dir.join(METADATA_FILE)).unwrap())
                .unwrap();
        assert_eq!(metadata.skill_hash, bundled_hash());
        assert_eq!(metadata.ctx_cli_version, TEST_PRODUCT_VERSION);
        assert!(!existing_managed_skill_refresh_required(&context));
    }

    #[test]
    fn released_metadata_free_legacy_skill_is_migrated() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
        let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("SKILL.md"), RELEASED_LEGACY_SKILL_V0_17_0).unwrap();

        assert!(existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        let metadata: SkillMetadata =
            serde_json::from_slice(&fs::read(target.skill_dir.join(METADATA_FILE)).unwrap())
                .unwrap();
        assert_eq!(metadata.ctx_cli_version, TEST_PRODUCT_VERSION);
        assert!(!legacy_dir.join("SKILL.md").exists());
    }

    #[test]
    fn released_legacy_with_untrusted_metadata_is_preserved() {
        let cases = [
            ("malformed", b"{not-json\n".to_vec()),
            (
                "foreign",
                metadata_body(
                    "another-installer",
                    LEGACY_BUNDLED_SKILL_NAME,
                    RELEASED_LEGACY_SKILL_V0_17_0,
                    0,
                ),
            ),
            (
                "mismatched",
                metadata_body(
                    "ctx-cli",
                    LEGACY_BUNDLED_SKILL_NAME,
                    b"different legacy body\n",
                    0,
                ),
            ),
        ];

        for (scenario, metadata) in cases {
            let root = tempfile::tempdir().unwrap();
            let context = test_context(&root);
            let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
            let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
            fs::create_dir_all(&legacy_dir).unwrap();
            fs::write(legacy_dir.join("SKILL.md"), RELEASED_LEGACY_SKILL_V0_17_0).unwrap();
            fs::write(legacy_dir.join(METADATA_FILE), &metadata).unwrap();

            assert_eq!(
                status_target(&target).unwrap().legacy_status,
                Some(SkillInstallStatus::Modified),
                "{scenario}"
            );
            let explicit = install_target(&target, false, true, TEST_PRODUCT_VERSION).unwrap();
            assert!(!explicit.success, "{scenario}");
            assert_automatic_refresh_preserves(&context, &target, scenario);
        }
    }

    #[test]
    fn missing_skill_roots_are_not_created() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        assert!(!context.home.exists());

        assert!(!existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert!(!context.home.exists());
        assert!(!context.cwd.exists());
    }

    #[test]
    fn metadata_free_and_mismatched_current_skills_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        let metadata_free = single_target(SkillAgentArg::Universal, false, &context).unwrap();
        fs::create_dir_all(&metadata_free.skill_dir).unwrap();
        fs::write(metadata_free.skill_dir.join("SKILL.md"), BUNDLED_SKILL_BODY).unwrap();

        let mismatched = single_target(SkillAgentArg::Codex, false, &context).unwrap();
        fs::create_dir_all(&mismatched.skill_dir).unwrap();
        fs::write(mismatched.skill_dir.join("SKILL.md"), b"local body\n").unwrap();
        let mismatched_metadata = SkillMetadata {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            skill_name: BUNDLED_SKILL_NAME.to_owned(),
            skill_hash: sha256_hex(b"different body\n"),
            ctx_cli_version: "0.1.0".to_owned(),
            installed_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let mismatched_metadata_body = serde_json::to_vec_pretty(&mismatched_metadata).unwrap();
        fs::write(
            mismatched.skill_dir.join(METADATA_FILE),
            &mismatched_metadata_body,
        )
        .unwrap();

        assert!(!existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert_eq!(
            fs::read(metadata_free.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        assert!(!metadata_free.skill_dir.join(METADATA_FILE).exists());
        assert_eq!(
            fs::read(mismatched.skill_dir.join("SKILL.md")).unwrap(),
            b"local body\n"
        );
        assert_eq!(
            fs::read(mismatched.skill_dir.join(METADATA_FILE)).unwrap(),
            mismatched_metadata_body
        );
    }

    #[test]
    fn malformed_orphaned_current_metadata_blocks_legacy_migration() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
        fs::create_dir_all(&target.skill_dir).unwrap();
        let malformed_metadata = b"{not-json\n";
        fs::write(target.skill_dir.join(METADATA_FILE), malformed_metadata).unwrap();
        let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("SKILL.md"), RELEASED_LEGACY_SKILL_V0_17_0).unwrap();

        assert!(!existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert!(!target.skill_dir.join("SKILL.md").exists());
        assert_eq!(
            fs::read(target.skill_dir.join(METADATA_FILE)).unwrap(),
            malformed_metadata
        );
        assert_eq!(
            fs::read(legacy_dir.join("SKILL.md")).unwrap(),
            RELEASED_LEGACY_SKILL_V0_17_0
        );
    }

    #[test]
    fn present_untrusted_current_metadata_blocks_interrupted_migration_recovery() {
        let cases = [
            ("malformed", b"{not-json\n".to_vec()),
            (
                "mismatched",
                metadata_body(
                    "ctx-cli",
                    BUNDLED_SKILL_NAME,
                    b"different current body\n",
                    0,
                ),
            ),
        ];

        for (scenario, metadata) in cases {
            let root = tempfile::tempdir().unwrap();
            let context = test_context(&root);
            let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
            fs::create_dir_all(&target.skill_dir).unwrap();
            fs::write(target.skill_dir.join("SKILL.md"), BUNDLED_SKILL_BODY).unwrap();
            fs::write(target.skill_dir.join(METADATA_FILE), metadata).unwrap();
            let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
            fs::create_dir_all(&legacy_dir).unwrap();
            fs::write(legacy_dir.join("SKILL.md"), RELEASED_LEGACY_SKILL_V0_17_0).unwrap();

            assert_automatic_refresh_preserves(&context, &target, scenario);
        }
    }

    #[test]
    fn interrupted_migration_converges_on_the_second_refresh() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
        let legacy_dir = write_owned_legacy(&target, b"previous managed legacy\n", 0);
        fs::create_dir_all(&target.skill_dir).unwrap();
        let metadata_lock = target
            .skill_dir
            .join(format!(".{METADATA_FILE}.ctx-agent-integrations.lock"));
        fs::create_dir(&metadata_lock).unwrap();

        assert!(existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        assert!(!target.skill_dir.join(METADATA_FILE).exists());
        assert!(legacy_dir.join("SKILL.md").is_file());

        fs::remove_dir(&metadata_lock).unwrap();
        assert!(existing_managed_skill_refresh_required(&context));
        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        let metadata: SkillMetadata =
            serde_json::from_slice(&fs::read(target.skill_dir.join(METADATA_FILE)).unwrap())
                .unwrap();
        assert_eq!(metadata.skill_hash, bundled_hash());
        assert_eq!(metadata.ctx_cli_version, TEST_PRODUCT_VERSION);
        assert!(!legacy_dir.join("SKILL.md").exists());
        assert!(!existing_managed_skill_refresh_required(&context));
    }

    #[test]
    fn oversized_automatic_candidates_are_preserved_without_creating_files() {
        for scenario in 0..4 {
            let root = tempfile::tempdir().unwrap();
            let context = test_context(&root);
            let target = single_target(SkillAgentArg::Universal, false, &context).unwrap();
            let scenario_name = match scenario {
                0 => {
                    let body = vec![b'c'; MAX_AUTOMATIC_SKILL_BYTES as usize + 1];
                    write_owned_current(&target, &body);
                    "oversized current skill"
                }
                1 => {
                    let body = b"previous managed current\n";
                    fs::create_dir_all(&target.skill_dir).unwrap();
                    fs::write(target.skill_dir.join("SKILL.md"), body).unwrap();
                    fs::write(
                        target.skill_dir.join(METADATA_FILE),
                        metadata_body(
                            "ctx-cli",
                            BUNDLED_SKILL_NAME,
                            body,
                            MAX_AUTOMATIC_METADATA_BYTES as usize,
                        ),
                    )
                    .unwrap();
                    "oversized current metadata"
                }
                2 => {
                    let body = vec![b'l'; MAX_AUTOMATIC_SKILL_BYTES as usize + 1];
                    write_owned_legacy(&target, &body, 0);
                    "oversized legacy skill"
                }
                3 => {
                    write_owned_legacy(
                        &target,
                        RELEASED_LEGACY_SKILL_V0_17_0,
                        MAX_AUTOMATIC_METADATA_BYTES as usize,
                    );
                    "oversized legacy metadata"
                }
                _ => unreachable!(),
            };

            assert_automatic_refresh_preserves(&context, &target, scenario_name);
        }
    }

    #[test]
    fn identical_global_skill_directories_are_deduplicated() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let shared_agent_root = home.join(".agents");
        let context = PathContext::for_tests(home, root.path().join("repo"))
            .with_env_override("CODEX_HOME", shared_agent_root);
        let duplicate_dir = single_target(SkillAgentArg::Universal, false, &context)
            .unwrap()
            .skill_dir;

        let duplicate_count = distinct_global_targets(&context)
            .iter()
            .filter(|target| target.skill_dir == duplicate_dir)
            .count();

        assert_eq!(duplicate_count, 1);
    }

    #[test]
    fn refresh_continues_after_one_target_fails() {
        let root = tempfile::tempdir().unwrap();
        let context = test_context(&root);
        let blocked = single_target(SkillAgentArg::Universal, false, &context).unwrap();
        let succeeding = single_target(SkillAgentArg::Codex, false, &context).unwrap();
        write_owned_current(&blocked, b"blocked stale skill\n");
        write_owned_current(&succeeding, b"refreshable stale skill\n");
        fs::create_dir(
            blocked
                .skill_dir
                .join(".SKILL.md.ctx-agent-integrations.lock"),
        )
        .unwrap();

        refresh_existing_managed_skills(&context, TEST_PRODUCT_VERSION);

        assert_eq!(
            fs::read(blocked.skill_dir.join("SKILL.md")).unwrap(),
            b"blocked stale skill\n"
        );
        assert_eq!(
            fs::read(succeeding.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
    }
}
