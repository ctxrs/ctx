mod support;

use support::*;

#[test]
fn slash_commands_install_opencode_global_and_is_idempotent() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-config");

    let first = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "opencode",
        "--json",
    ]));
    assert_eq!(first["integration"], "slash-commands");
    assert_eq!(first["command"], "ctx-history");
    assert_eq!(first["results"][0]["agent"], "opencode");
    assert_eq!(first["results"][0]["previous_status"], "missing");
    assert_eq!(first["results"][0]["status"], "current");
    assert_eq!(first["results"][0]["already_installed"], false);

    let command_path = xdg.join("opencode").join("commands").join("ctx-history.md");
    assert!(command_path.exists());
    assert!(fs::read_to_string(&command_path)
        .unwrap()
        .contains("$ARGUMENTS"));
    assert!(command_path
        .parent()
        .unwrap()
        .join(".ctx-slash-commands.json")
        .exists());

    let second = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "opencode",
        "--json",
    ]));
    assert_eq!(second["results"][0]["previous_status"], "current");
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["updated"], false);
}

#[test]
fn slash_commands_install_codex_is_skill_only_without_deprecated_prompts() {
    let temp = tempdir();

    let output = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "codex",
        "--json",
    ]));
    assert_eq!(output["results"][0]["agent"], "codex");
    assert_eq!(output["results"][0]["status"], "skill_only");
    assert!(output["results"][0]["note"]
        .as_str()
        .unwrap()
        .contains("ctx skill install --agent codex"));
    assert!(!temp.path().join(".codex").join("prompts").exists());
}

#[test]
fn slash_commands_install_gemini_project_writes_toml() {
    let temp = tempdir();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "gemini-cli",
        "--project",
        "--json",
    ]);
    let output = json_output(&mut command);
    assert_eq!(output["scope"], "project");
    assert_eq!(output["results"][0]["agent"], "gemini-cli");
    assert_eq!(
        output["results"][0]["path"],
        json!(project.join(".gemini/commands/ctx-history.toml"))
    );

    let command_path = project
        .join(".gemini")
        .join("commands")
        .join("ctx-history.toml");
    let body = fs::read_to_string(command_path).unwrap();
    assert!(body.contains("description ="));
    assert!(body.contains("prompt = '''"));
    assert!(body.contains("{{args}}"));
}

#[test]
fn slash_commands_install_qwen_project_writes_markdown() {
    let temp = tempdir();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "qwen-code",
        "--project",
        "--json",
    ]);
    let output = json_output(&mut command);
    assert_eq!(output["scope"], "project");
    assert_eq!(output["results"][0]["agent"], "qwen-code");

    let command_path = project
        .join(".qwen")
        .join("commands")
        .join("ctx-history.md");
    let body = fs::read_to_string(command_path).unwrap();
    assert!(body.contains("---\ndescription:"));
    assert!(body.contains("{{args}}"));
}
