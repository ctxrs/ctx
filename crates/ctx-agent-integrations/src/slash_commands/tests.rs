use super::*;

const PRODUCT_VERSION: &str = "1.0.0-test";

fn request(agent: SlashCommandAgent) -> SlashCommandInstallRequest {
    SlashCommandInstallRequest {
        agents: vec![agent],
        all_agents: false,
        project: true,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    }
}

#[test]
fn detected_file_targets_are_selected_once_and_in_order() {
    let root = tempfile::tempdir().unwrap();
    let xdg = root.path().join("xdg");
    fs::create_dir_all(xdg.join("opencode")).unwrap();
    fs::create_dir_all(xdg.join("mimocode")).unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned())
        .with_xdg_config_home(xdg);
    let request = SlashCommandInstallRequest {
        agents: Vec::new(),
        all_agents: false,
        project: false,
        force: false,
        product_version: PRODUCT_VERSION.to_owned(),
    };

    assert_eq!(
        selected_agents(
            &request.agents,
            request.all_agents,
            request.project,
            &context,
        ),
        vec![SlashCommandAgent::OpenCode, SlashCommandAgent::MiMoCode]
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn managed_file_is_idempotent_and_refreshes_stale_content() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let request = request(SlashCommandAgent::OpenCode);
    let first = execute_install(request.clone(), &context).unwrap();
    assert_eq!(
        first.results[0].previous_status,
        SlashCommandInstallStatus::Missing
    );
    assert!(!first.results[0].already_installed);

    let second = execute_install(request.clone(), &context).unwrap();
    assert!(second.results[0].already_installed);

    let target = match SlashCommandAgent::OpenCode.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => unreachable!(),
    };
    let old_body = "---\ndescription: old\n---\n\nold\n";
    fs::write(target.command_path(), old_body).unwrap();
    let mut metadata = SlashCommandMetadata::current(&target, PRODUCT_VERSION);
    metadata
        .files
        .insert(target.filename.clone(), sha256_hex(old_body.as_bytes()));
    fs::write(
        target.base_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();

    let refreshed = execute_install(request, &context).unwrap();
    assert_eq!(
        refreshed.results[0].previous_status,
        SlashCommandInstallStatus::Stale
    );
    assert!(refreshed.results[0].updated);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn local_command_edits_require_force_and_unrelated_files_survive() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let mut request = request(SlashCommandAgent::GeminiCli);
    let target = match SlashCommandAgent::GeminiCli.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => unreachable!(),
    };
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(target.command_path(), "prompt = 'local'\n").unwrap();
    fs::write(target.base_dir.join("keep.txt"), "keep").unwrap();

    let skipped = execute_install(request.clone(), &context).unwrap();
    assert!(!skipped.results[0].success);
    assert_eq!(
        skipped.results[0].status,
        SlashCommandInstallStatus::Modified
    );

    request.force = true;
    let forced = execute_install(request, &context).unwrap();
    assert!(forced.results[0].success);
    assert_eq!(
        fs::read_to_string(target.base_dir.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn managed_legacy_command_is_migrated_to_ctx() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let request = request(SlashCommandAgent::OpenCode);
    let target = match SlashCommandAgent::OpenCode.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => unreachable!(),
    };
    let legacy_path = target.legacy_command_path();
    let legacy_body = "---\ndescription: Search local agent history with ctx\n---\n";
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(&legacy_path, legacy_body).unwrap();
    fs::write(target.base_dir.join("keep.txt"), "keep").unwrap();
    let metadata = SlashCommandMetadata {
        schema_version: 1,
        installer: "ctx-cli".to_owned(),
        command_name: LEGACY_COMMAND_NAME.to_owned(),
        files: BTreeMap::from([(target.legacy_filename(), sha256_hex(legacy_body.as_bytes()))]),
        ctx_cli_version: "0.9.0".to_owned(),
        installed_at: utc_now().to_rfc3339(),
    };
    fs::write(
        target.base_dir.join(METADATA_FILE),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();

    let migrated = execute_install(request.clone(), &context).unwrap();
    let result = &migrated.results[0];
    assert!(result.success);
    assert!(result.migrated);
    assert!(result.updated);
    assert!(!result.already_installed);
    assert_eq!(result.previous_status, SlashCommandInstallStatus::Stale);
    assert_eq!(result.legacy_path.as_deref(), Some(legacy_path.as_path()));
    assert!(target.command_path().is_file());
    assert!(!legacy_path.exists());
    assert_eq!(
        fs::read_to_string(target.base_dir.join("keep.txt")).unwrap(),
        "keep"
    );

    let second = execute_install(request, &context).unwrap();
    assert!(second.results[0].already_installed);
    assert!(!second.results[0].migrated);
    assert!(!second.results[0].updated);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn modified_legacy_command_is_preserved_unless_forced() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let mut request = request(SlashCommandAgent::GeminiCli);
    let target = match SlashCommandAgent::GeminiCli.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => unreachable!(),
    };
    let legacy_path = target.legacy_command_path();
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::write(&legacy_path, "prompt = 'locally edited'\n").unwrap();

    let skipped = execute_install(request.clone(), &context).unwrap();
    assert!(!skipped.results[0].success);
    assert_eq!(
        skipped.results[0].status,
        SlashCommandInstallStatus::Modified
    );
    assert!(!target.command_path().exists());
    assert_eq!(
        fs::read_to_string(&legacy_path).unwrap(),
        "prompt = 'locally edited'\n"
    );

    request.force = true;
    let forced = execute_install(request, &context).unwrap();
    assert!(forced.results[0].success);
    assert!(forced.results[0].migrated);
    assert!(target.command_path().is_file());
    assert!(!legacy_path.exists());
}

#[test]
fn generated_command_bytes_match_the_public_contract() {
    assert_eq!(
        opencode_command_body(),
        format!(
            "---\ndescription: Search agent history or trace code with ctx\nargument-hint: [question, topic, file, line, commit, or PR]\n---\n\n{COMMAND_INSTRUCTIONS}"
        )
    );
    assert!(gemini_command_body().contains("User request: {{args}}"));
    assert!(qwen_command_body().ends_with(
        COMMAND_INSTRUCTIONS
            .replace("$ARGUMENTS", "{{args}}")
            .as_str()
    ));
}

#[test]
fn skill_only_agents_do_not_write_legacy_prompts() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let receipt = execute_install(request(SlashCommandAgent::Codex), &context).unwrap();
    assert_eq!(
        receipt.results[0].status,
        SlashCommandInstallStatus::SkillOnly
    );
    assert!(!root.path().join(".codex").join("prompts").exists());
}

#[test]
fn grok_build_is_skill_only_and_writes_no_command_file() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let receipt = execute_install(request(SlashCommandAgent::GrokBuild), &context).unwrap();

    assert_eq!(receipt.results[0].agent.id(), "grok-build");
    assert_eq!(receipt.results[0].agent.display_name(), "Grok Build");
    assert_eq!(
        receipt.results[0].status,
        SlashCommandInstallStatus::SkillOnly
    );
    assert!(receipt.results[0].path.is_none());
    assert!(!root.path().join(".grok").exists());
}

#[test]
fn interrupted_content_then_metadata_publication_is_unowned_until_forced() {
    let root = tempfile::tempdir().unwrap();
    let context = PathContext::for_tests(root.path().to_owned(), root.path().to_owned());
    let request = request(SlashCommandAgent::OpenCode);
    let target = match SlashCommandAgent::OpenCode.install_plan(true, &context) {
        SlashCommandPlan::File(target) => target,
        _ => unreachable!(),
    };
    fs::create_dir_all(&target.base_dir).unwrap();
    fs::create_dir(target.base_dir.join(METADATA_FILE)).unwrap();

    let error = execute_install(request.clone(), &context).unwrap_err();
    assert!(format!("{error:#}").contains("non-regular file"));
    assert_eq!(
        fs::read(target.command_path()).unwrap(),
        target.body.as_bytes()
    );
    assert_eq!(
        status_file_target(&target).unwrap().status,
        SlashCommandInstallStatus::Modified
    );

    fs::remove_dir(target.base_dir.join(METADATA_FILE)).unwrap();
    let preserved = execute_install(request.clone(), &context).unwrap();
    assert!(!preserved.results[0].success);
    assert_eq!(
        preserved.results[0].status,
        SlashCommandInstallStatus::Modified
    );

    let mut forced = request;
    forced.force = true;
    let repaired = execute_install(forced, &context).unwrap();
    assert!(repaired.results[0].success);
    assert_eq!(
        status_file_target(&target).unwrap().status,
        SlashCommandInstallStatus::Current
    );
}
