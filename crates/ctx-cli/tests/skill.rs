mod support;

use sha2::{Digest, Sha256};
use support::*;

#[test]
fn skill_install_defaults_to_global_canonical_agents_dir_and_is_idempotent() {
    let temp = tempdir();

    let first = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["skill", "install", "--json"]),
    );
    assert_eq!(first["skill"], "ctx-agent-history-search");
    assert_eq!(first["results"][0]["agent"], "universal");
    assert_eq!(first["results"][0]["previous_status"], "missing");
    assert_eq!(first["results"][0]["status"], "current");
    assert_eq!(first["results"][0]["already_installed"], false);

    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    assert!(skill_dir.join("SKILL.md").exists());
    assert!(skill_dir.join(".ctx-skill.json").exists());

    let second = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["skill", "install", "--json"]),
    );
    assert_eq!(second["results"][0]["previous_status"], "current");
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["updated"], false);

    let status = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["skill", "status", "--json"]),
    );
    assert_eq!(status["results"][0]["status"], "current");
}

#[test]
fn skill_install_auto_targets_universal_and_detected_claude_code() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".claude")).unwrap();

    let install = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["skill", "install", "--json"]),
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
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(temp
        .path()
        .join(".claude")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
}

#[test]
fn skill_install_refreshes_stale_bundled_copy() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "old instructions\n").unwrap();
    let old_hash = format!("sha256:{:x}", Sha256::digest(b"old instructions\n"));
    fs::write(
        skill_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx-agent-history-search",
            "skill_hash": old_hash,
            "ctx_cli_version": "0.0.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let stale = json_output(ctx(&temp).args(["skill", "status", "--agent", "universal", "--json"]));
    assert_eq!(stale["results"][0]["status"], "stale");

    let install =
        json_output(ctx(&temp).args(["skill", "install", "--agent", "universal", "--json"]));
    assert_eq!(install["results"][0]["previous_status"], "stale");
    assert_eq!(install["results"][0]["updated"], true);
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("ctx Agent History Search"));
}

#[test]
fn skill_install_preserves_modified_copy_unless_forced() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "local custom instructions\n").unwrap();

    let output = ctx(&temp)
        .args(["skill", "install", "--agent", "universal", "--json"])
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
        .contains("--force"));
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "local custom instructions\n"
    );

    let forced = json_output(ctx(&temp).args([
        "skill",
        "install",
        "--agent",
        "universal",
        "--force",
        "--json",
    ]));
    assert_eq!(forced["results"][0]["success"], true);
    assert_eq!(forced["results"][0]["previous_status"], "modified");
    assert_eq!(forced["results"][0]["status"], "current");
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("ctx Agent History Search"));
}

#[test]
fn skill_install_agent_paths_respect_env_xdg_and_project_scope() {
    let temp = tempdir();
    let home = temp.path();
    let xdg = temp.path().join("xdg-config");
    let codex_home = temp.path().join("custom-codex");
    let claude_home = temp.path().join("custom-claude");

    let global = json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("CODEX_HOME", &codex_home)
            .env("CLAUDE_CONFIG_DIR", &claude_home)
            .args([
                "skill",
                "install",
                "--agent",
                "codex",
                "--agent",
                "claude-code",
                "--agent",
                "opencode",
                "--json",
            ]),
    );
    assert_eq!(global["results"].as_array().unwrap().len(), 3);
    assert!(codex_home
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(claude_home
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(xdg
        .join("opencode")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());

    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "skill",
        "install",
        "--project",
        "--agent",
        "codex",
        "--agent",
        "claude-code",
        "--json",
    ]);
    let project_output = json_output(&mut command);
    assert_eq!(project_output["scope"], "project");
    assert!(project
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(project
        .join(".claude")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(!home
        .join(".codex")
        .join("skills")
        .join("ctx-agent-history-search")
        .exists());
}
