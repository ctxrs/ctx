mod support;

use sha2::{Digest, Sha256};
use support::*;

const CURRENT_BUNDLED_SKILL_BODY: &str = include_str!("../../../../skills/ctx/SKILL.md");

fn human_stdout(command: &mut assert_cmd::Command) -> String {
    let output = command.assert().success().get_output().clone();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert!(output.stdout.contains(&0x1b), "{:?}", output.stdout);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn skill_human_receipts_use_ui_and_json_stays_plain() {
    let temp = tempdir();

    let missing = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "status",
        "skills",
        "--agent",
        "universal",
    ]));
    assert!(missing.contains("Agent skill needs attention"), "{missing}");
    assert!(missing.contains("missing"), "{missing}");

    let installed = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
    ]));
    assert!(installed.contains("Agent skill installed"), "{installed}");
    assert!(installed.contains("installed"), "{installed}");

    let current = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "status",
        "skills",
        "--agent",
        "universal",
    ]));
    assert!(current.contains("Agent skill is current"), "{current}");
    assert!(current.contains("current"), "{current}");

    let machine = ctx(&temp)
        .args([
            "--color=always",
            "integrations",
            "status",
            "skills",
            "--agent",
            "universal",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(!machine.stdout.contains(&0x1b), "{:?}", machine.stdout);
    let value: Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(value["results"][0]["status"], "current");
}

fn bundled_skill_hash(body: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
}

#[test]
fn skill_install_defaults_to_global_canonical_agents_dir_and_is_idempotent() {
    let temp = tempdir();

    let first = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "install", "skills", "--format=json"]),
    );
    assert_eq!(first["skill"], "ctx");
    assert_eq!(first["results"][0]["agent"], "universal");
    assert_eq!(first["results"][0]["previous_status"], "missing");
    assert_eq!(first["results"][0]["status"], "current");
    assert_eq!(first["results"][0]["already_installed"], false);

    let skill_dir = temp.path().join(".agents").join("skills").join("ctx");
    assert!(skill_dir.join("SKILL.md").exists());
    assert!(skill_dir.join(".ctx-skill.json").exists());

    let second = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "install", "skills", "--format=json"]),
    );
    assert_eq!(second["results"][0]["previous_status"], "current");
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["updated"], false);

    let status = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "status", "skills", "--format=json"]),
    );
    assert_eq!(status["results"][0]["status"], "current");
}

#[test]
fn skill_install_migrates_a_managed_legacy_skill_once() {
    let temp = tempdir();
    let skills_dir = temp.path().join(".agents").join("skills");
    let legacy_dir = skills_dir.join("ctx-agent-history-search");
    let skill_dir = skills_dir.join("ctx");
    let legacy_body = "legacy managed skill\n";
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(legacy_dir.join("SKILL.md"), legacy_body).unwrap();
    fs::write(legacy_dir.join("notes.txt"), "keep").unwrap();
    fs::write(
        legacy_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx-agent-history-search",
            "skill_hash": bundled_skill_hash(legacy_body),
            "ctx_cli_version": "0.9.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let status = json_output(ctx(&temp).args([
        "integrations",
        "status",
        "skills",
        "--agent",
        "universal",
        "--format=json",
    ]));
    assert_eq!(status["results"][0]["status"], "stale");
    assert_eq!(status["results"][0]["legacy_status"], "stale");
    assert_eq!(status["results"][0]["legacy_path"], json!(legacy_dir));

    let migrated = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--format=json",
    ]));
    assert_eq!(migrated["results"][0]["previous_status"], "stale");
    assert_eq!(migrated["results"][0]["status"], "current");
    assert_eq!(migrated["results"][0]["migrated"], true);
    assert_eq!(migrated["results"][0]["updated"], true);
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(!legacy_dir.join("SKILL.md").exists());
    assert!(!legacy_dir.join(".ctx-skill.json").exists());
    assert_eq!(
        fs::read_to_string(legacy_dir.join("notes.txt")).unwrap(),
        "keep"
    );

    let second = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--format=json",
    ]));
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["migrated"], false);
    assert_eq!(second["results"][0]["updated"], false);
}

#[test]
fn default_skill_install_migrates_a_managed_legacy_codex_copy() {
    let temp = tempdir();
    let legacy_dir = temp
        .path()
        .join(".codex")
        .join("skills")
        .join("ctx-agent-history-search");
    let legacy_body = "legacy managed codex skill\n";
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(legacy_dir.join("SKILL.md"), legacy_body).unwrap();
    fs::write(
        legacy_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx-agent-history-search",
            "skill_hash": bundled_skill_hash(legacy_body),
            "ctx_cli_version": "0.9.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let migrated = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join(".codex"))
            .args(["integrations", "install", "skills", "--format=json"]),
    );

    let results = migrated["results"].as_array().unwrap();
    assert_eq!(results[0]["agent"], "universal");
    assert!(results
        .iter()
        .any(|result| result["agent"] == "codex" && result["migrated"] == true));
    assert!(!legacy_dir.join("SKILL.md").exists());
    assert!(temp
        .path()
        .join(".codex")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .is_file());
}

#[test]
fn skill_install_preserves_modified_legacy_skill_unless_forced() {
    let temp = tempdir();
    let skills_dir = temp.path().join(".agents").join("skills");
    let legacy_dir = skills_dir.join("ctx-agent-history-search");
    let skill_dir = skills_dir.join("ctx");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::write(legacy_dir.join("SKILL.md"), "local legacy edits\n").unwrap();
    fs::write(legacy_dir.join("notes.txt"), "keep").unwrap();

    let output = ctx(&temp)
        .args([
            "integrations",
            "install",
            "skills",
            "--agent",
            "universal",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let preserved: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(preserved["results"][0]["status"], "modified");
    assert_eq!(preserved["results"][0]["migrated"], false);
    assert!(preserved["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("preserved locally modified legacy"));
    assert!(!skill_dir.exists());
    assert_eq!(
        fs::read_to_string(legacy_dir.join("SKILL.md")).unwrap(),
        "local legacy edits\n"
    );

    let forced = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--force",
        "--format=json",
    ]));
    assert_eq!(forced["results"][0]["migrated"], true);
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(!legacy_dir.join("SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(legacy_dir.join("notes.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn skill_install_auto_targets_universal_and_detected_claude_code() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".claude")).unwrap();

    let install = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "install", "skills", "--format=json"]),
    );
    assert_eq!(install["results"].as_array().unwrap().len(), 2);
    assert_eq!(install["results"][0]["agent"], "universal");
    assert_eq!(install["results"][1]["agent"], "claude-code");
    assert_eq!(install["results"][0]["status"], "current");
    assert_eq!(install["results"][1]["status"], "current");

    assert!(temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(temp
        .path()
        .join(".claude")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
}

#[test]
fn skill_install_detected_mimocode_uses_universal_skill_location() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-config");
    fs::create_dir_all(xdg.join("mimocode")).unwrap();

    let output = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "integrations",
        "install",
        "skills",
        "--format=json",
    ]));

    let agents = output["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["agent"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(agents, vec!["universal"]);
    assert!(temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(!xdg.join("mimocode").join("skills").join("ctx").exists());
}

fn skill_result<'a>(output: &'a Value, agent: &str) -> &'a Value {
    output["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["agent"] == agent)
        .unwrap_or_else(|| panic!("no {agent} target in {output:#}"))
}

#[test]
fn default_skill_status_and_install_cover_an_existing_native_cursor_copy() {
    let temp = tempdir();
    let skill_dir = temp.path().join(".cursor").join("skills").join("ctx");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "old instructions\n").unwrap();
    fs::write(
        skill_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx",
            "skill_hash": bundled_skill_hash("old instructions\n"),
            "ctx_cli_version": "0.0.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let status = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "status", "skills", "--format=json"]),
    );
    assert_eq!(skill_result(&status, "cursor")["status"], "stale");
    assert_eq!(skill_result(&status, "cursor")["path"], json!(skill_dir));

    let install = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "install", "skills", "--format=json"]),
    );
    assert_eq!(skill_result(&install, "cursor")["previous_status"], "stale");
    assert_eq!(skill_result(&install, "cursor")["updated"], true);
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        CURRENT_BUNDLED_SKILL_BODY
    );
}

#[test]
fn default_skill_status_ignores_an_unselected_path_the_installer_cannot_own() {
    let temp = tempdir();
    let copilot_dir = temp.path().join(".copilot").join("skills");
    fs::create_dir_all(&copilot_dir).unwrap();
    fs::write(copilot_dir.join("ctx"), "not a skill directory\n").unwrap();
    #[cfg(unix)]
    {
        let linked = temp.path().join("linked-skill");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join("SKILL.md"), CURRENT_BUNDLED_SKILL_BODY).unwrap();
        let cursor_dir = temp.path().join(".cursor").join("skills");
        fs::create_dir_all(&cursor_dir).unwrap();
        std::os::unix::fs::symlink(&linked, cursor_dir.join("ctx")).unwrap();

        let linked_file = temp.path().join("linked-skill.md");
        fs::write(&linked_file, CURRENT_BUNDLED_SKILL_BODY).unwrap();
        let gemini_skill_dir = temp.path().join(".gemini").join("skills").join("ctx");
        fs::create_dir_all(&gemini_skill_dir).unwrap();
        std::os::unix::fs::symlink(&linked_file, gemini_skill_dir.join("SKILL.md")).unwrap();

        let outside_grok_skills = temp.path().join("outside-grok-skills");
        fs::create_dir_all(outside_grok_skills.join("ctx")).unwrap();
        fs::write(
            outside_grok_skills.join("ctx").join("SKILL.md"),
            CURRENT_BUNDLED_SKILL_BODY,
        )
        .unwrap();
        let grok_dir = temp.path().join(".grok");
        fs::create_dir_all(&grok_dir).unwrap();
        std::os::unix::fs::symlink(&outside_grok_skills, grok_dir.join("skills")).unwrap();
    }

    let status = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "status", "skills", "--format=json"]),
    );

    let agents = status["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["agent"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(agents, vec!["universal"], "{status:#}");
}

#[test]
fn explicit_skill_install_does_not_touch_an_unselected_stale_copy() {
    let temp = tempdir();
    let cursor_dir = temp.path().join(".cursor").join("skills").join("ctx");
    fs::create_dir_all(&cursor_dir).unwrap();
    fs::write(cursor_dir.join("SKILL.md"), "old instructions\n").unwrap();
    fs::write(
        cursor_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx",
            "skill_hash": bundled_skill_hash("old instructions\n"),
            "ctx_cli_version": "0.0.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let install = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args([
                "integrations",
                "install",
                "skills",
                "--agent",
                "universal",
                "--format=json",
            ]),
    );
    assert_eq!(skill_result(&install, "universal")["status"], "current");
    assert_eq!(
        install["results"].as_array().unwrap().len(),
        1,
        "{install:#}"
    );
    assert_eq!(
        fs::read_to_string(cursor_dir.join("SKILL.md")).unwrap(),
        "old instructions\n"
    );
}

#[test]
fn skill_install_refreshes_stale_bundled_copy() {
    let temp = tempdir();
    let skill_dir = temp.path().join(".agents").join("skills").join("ctx");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "old instructions\n").unwrap();
    let old_hash = format!("sha256:{:x}", Sha256::digest(b"old instructions\n"));
    fs::write(
        skill_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx",
            "skill_hash": old_hash,
            "ctx_cli_version": "0.0.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let stale = json_output(ctx(&temp).args([
        "integrations",
        "status",
        "skills",
        "--agent",
        "universal",
        "--format=json",
    ]));
    assert_eq!(stale["results"][0]["status"], "stale");

    let install = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--format=json",
    ]));
    assert_eq!(install["results"][0]["previous_status"], "stale");
    assert_eq!(install["results"][0]["updated"], true);
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("\n# ctx\n"));
}

#[test]
fn skill_install_backfills_current_metadata_without_rewriting_body() {
    let temp = tempdir();
    let skill_dir = temp.path().join(".agents").join("skills").join("ctx");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), CURRENT_BUNDLED_SKILL_BODY).unwrap();
    fs::write(
        skill_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx",
            "skill_hash": bundled_skill_hash(CURRENT_BUNDLED_SKILL_BODY),
            "ctx_cli_version": "0.0.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let install = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--format=json",
    ]));
    assert_eq!(install["results"][0]["success"], true);
    assert_eq!(install["results"][0]["previous_status"], "current");
    assert_eq!(install["results"][0]["status"], "current");
    assert_eq!(install["results"][0]["already_installed"], true);
    assert_eq!(install["results"][0]["updated"], false);
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        CURRENT_BUNDLED_SKILL_BODY
    );

    let metadata: Value =
        serde_json::from_slice(&fs::read(skill_dir.join(".ctx-skill.json")).unwrap()).unwrap();
    assert_eq!(metadata["ctx_cli_version"], ctx_product_version(&temp));
}

#[test]
fn skill_install_default_fallback_preserves_custom_copy_without_failing() {
    let temp = tempdir();
    let skill_dir = temp.path().join(".agents").join("skills").join("ctx");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "local custom instructions\n").unwrap();

    ctx(&temp)
        .args(["integrations", "install", "skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("preserved existing").not())
        .stderr(
            predicate::str::contains("Universal .agents Agent Skill was not changed").and(
                predicate::str::contains(
                    "ctx integrations install skills --agent universal --force",
                ),
            ),
        );
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "local custom instructions\n"
    );
    assert!(!skill_dir.join(".ctx-skill.json").exists());
}

#[test]
fn skill_install_preserves_modified_copy_unless_forced() {
    let temp = tempdir();
    let skill_dir = temp.path().join(".agents").join("skills").join("ctx");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "local custom instructions\n").unwrap();

    ctx(&temp)
        .args(["integrations", "install", "skills", "--agent", "universal"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("preserved existing").not())
        .stderr(
            predicate::str::contains("Universal .agents Agent Skill was not changed")
                .and(predicate::str::contains(
                    "ctx integrations install skills --agent universal --force",
                ))
                .and(predicate::str::contains("failed to install skill").not()),
        );

    let output = ctx(&temp)
        .args([
            "integrations",
            "install",
            "skills",
            "--agent",
            "universal",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"][0]["success"], false);
    assert_eq!(json["results"][0]["previous_status"], "modified");
    assert_eq!(json["results"][0]["status"], "modified");
    assert!(json["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("preserved existing Universal .agents skill; use --force to replace"));
    assert_eq!(
        output.stderr,
        b"Error: failed to install skill for 1 target(s)\n"
    );
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "local custom instructions\n"
    );

    let forced = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--force",
        "--format=json",
    ]));
    assert_eq!(forced["results"][0]["success"], true);
    assert_eq!(forced["results"][0]["previous_status"], "modified");
    assert_eq!(forced["results"][0]["status"], "current");
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("\n# ctx\n"));
}

#[test]
fn skill_install_agent_paths_respect_env_xdg_and_project_scope() {
    let temp = tempdir();
    let home = temp.path();
    let xdg = temp.path().join("xdg-config");
    let codex_home = temp.path().join("custom-codex");
    let claude_home = temp.path().join("custom-claude");
    let mimocode_home = temp.path().join("custom-mimocode");

    let global = json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("CODEX_HOME", &codex_home)
            .env("CLAUDE_CONFIG_DIR", &claude_home)
            .env("MIMOCODE_HOME", &mimocode_home)
            .args([
                "integrations",
                "install",
                "skills",
                "--agent",
                "codex",
                "--agent",
                "claude-code",
                "--agent",
                "opencode",
                "--agent",
                "mimocode",
                "--format=json",
            ]),
    );
    assert_eq!(global["results"].as_array().unwrap().len(), 4);
    assert!(codex_home
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(mimocode_home
        .join("config")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(claude_home
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(xdg
        .join("opencode")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());

    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "skills",
        "--project",
        "--agent",
        "codex",
        "--agent",
        "claude-code",
        "--agent",
        "mimocode",
        "--format=json",
    ]);
    let project_output = json_output(&mut command);
    assert_eq!(project_output["scope"], "project");
    assert!(project
        .join(".agents")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(project
        .join(".claude")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(project
        .join(".agents")
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(!home.join(".codex").join("skills").join("ctx").exists());
}

#[test]
fn skill_install_mimocode_honors_config_dir_env() {
    let temp = tempdir();
    let config_dir = temp.path().join("mimocode-config");

    let output = json_output(ctx(&temp).env("MIMOCODE_CONFIG_DIR", &config_dir).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "mimocode",
        "--format=json",
    ]));

    assert_eq!(output["results"][0]["agent"], "mimocode");
    assert!(config_dir
        .join("skills")
        .join("ctx")
        .join("SKILL.md")
        .exists());
    assert!(!temp
        .path()
        .join(".config")
        .join("mimocode")
        .join("skills")
        .exists());
}

#[test]
fn skill_install_mimocode_rejects_relative_home_override() {
    let temp = tempdir();

    let stderr = failure_stderr(
        ctx(&temp)
            .env("MIMOCODE_HOME", "relative-mimocode-home")
            .args([
                "integrations",
                "install",
                "skills",
                "--agent",
                "mimocode",
                "--format=json",
            ]),
    );

    assert!(stderr.contains("MIMOCODE_HOME must be an absolute path"));
}
