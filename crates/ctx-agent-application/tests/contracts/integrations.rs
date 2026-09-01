mod support;

use support::*;

#[test]
fn slash_commands_install_opencode_global_and_is_idempotent() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-config");

    let first = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "--color=always",
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "opencode",
        "--format=json",
    ]));
    assert_eq!(first["integration"], "slash-commands");
    assert_eq!(first["command"], "ctx");
    assert_eq!(first["results"][0]["agent"], "opencode");
    assert_eq!(first["results"][0]["previous_status"], "missing");
    assert_eq!(first["results"][0]["status"], "current");
    assert_eq!(first["results"][0]["already_installed"], false);

    let command_path = xdg.join("opencode").join("commands").join("ctx.md");
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
        "--format=json",
    ]));
    assert_eq!(second["results"][0]["previous_status"], "current");
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["updated"], false);
}

#[test]
fn slash_commands_install_migrates_managed_ctx_history_to_ctx() {
    let temp = tempdir();
    let project = temp.path().join("project");
    let command_dir = project.join(".gemini").join("commands");
    let legacy_path = command_dir.join("ctx-history.toml");
    let command_path = command_dir.join("ctx.toml");
    let legacy_body = "prompt = 'managed legacy command'\n";
    fs::create_dir_all(&command_dir).unwrap();
    fs::write(&legacy_path, legacy_body).unwrap();
    fs::write(command_dir.join("keep.txt"), "keep").unwrap();
    fs::write(
        command_dir.join(".ctx-slash-commands.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "command_name": "ctx-history",
            "files": {
                "ctx-history.toml": "sha256:8e6ef57e9d2ba609496d3ac98016385cb9e9613d65a93430c5d7b7453accecfb"
            },
            "ctx_cli_version": "0.9.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "gemini-cli",
        "--project",
        "--format=json",
    ]);
    let migrated = json_output(&mut command);
    assert_eq!(migrated["results"][0]["previous_status"], "stale");
    assert_eq!(migrated["results"][0]["status"], "current");
    assert_eq!(migrated["results"][0]["migrated"], true);
    assert_eq!(migrated["results"][0]["legacy_path"], json!(legacy_path));
    assert!(command_path.is_file());
    assert!(!legacy_path.exists());
    assert_eq!(
        fs::read_to_string(command_dir.join("keep.txt")).unwrap(),
        "keep"
    );

    let mut second = ctx(&temp);
    second.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "gemini-cli",
        "--project",
        "--format=json",
    ]);
    let second = json_output(&mut second);
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["migrated"], false);
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
        "--format=json",
    ]));
    assert_eq!(output["results"][0]["agent"], "codex");
    assert_eq!(output["results"][0]["status"], "skill_only");
    assert!(output["results"][0]["note"]
        .as_str()
        .unwrap()
        .contains("ctx integrations install skill --agent codex"));
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
        "--format=json",
    ]);
    let output = json_output(&mut command);
    assert_eq!(output["scope"], "project");
    assert_eq!(output["results"][0]["agent"], "gemini-cli");
    assert_eq!(
        output["results"][0]["path"],
        json!(project.join(".gemini/commands/ctx.toml"))
    );

    let command_path = project.join(".gemini").join("commands").join("ctx.toml");
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
        "--format=json",
    ]);
    let output = json_output(&mut command);
    assert_eq!(output["scope"], "project");
    assert_eq!(output["results"][0]["agent"], "qwen-code");

    let command_path = project.join(".qwen").join("commands").join("ctx.md");
    let body = fs::read_to_string(command_path).unwrap();
    assert!(body.contains("---\ndescription:"));
    assert!(body.contains("{{args}}"));
}

#[cfg(unix)]
#[test]
fn plugin_cli_delegates_exact_argv_and_preserves_the_marketplace() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempdir();
    let bin_dir = temp.path().join("bin");
    let codex = bin_dir.join("codex");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(
        &codex,
        r#"#!/bin/sh
set -eu
state="$0.state"
log="$0.log"
/bin/mkdir -p "$state"
printf '%s\n' "$*" >> "$log"
case "$*" in
  "plugin marketplace list --json")
    if [ -f "$state/marketplace" ]; then
      printf '%s\n' '{"marketplaces":[{"name":"ctx","marketplaceSource":{"type":"github","value":"ctxrs/ctx"}}]}'
    else
      printf '%s\n' '{"marketplaces":[]}'
    fi
    ;;
  "plugin marketplace add ctxrs/ctx --json")
    : > "$state/marketplace"
    printf '%s\n' '{}'
    ;;
  "plugin list --json")
    printf '%s' '{"installed":['
    separator=''
    if [ -f "$state/current" ]; then
      printf '%s' '{"pluginId":"ctx@ctx","name":"ctx","marketplaceName":"ctx","installed":true,"version":"99.0.0"}'
      separator=','
    fi
    if [ -f "$state/legacy" ]; then
      printf '%s%s' "$separator" '{"pluginId":"ctx-agent-history-search@ctx","name":"ctx-agent-history-search","marketplaceName":"ctx","installed":true}'
    fi
    printf '%s\n' ']}'
    ;;
  "plugin add ctx@ctx --json")
    if [ -f "$state/fail-install" ]; then
      printf '%s\n' 'private install failure' >&2
      exit 23
    fi
    : > "$state/current"
    printf '%s\n' '{}'
    ;;
  "plugin remove ctx@ctx --json")
    /bin/rm -f "$state/current"
    printf '%s\n' '{}'
    ;;
  "plugin remove ctx-agent-history-search@ctx --json")
    /bin/rm -f "$state/legacy"
    printf '%s\n' '{}'
    ;;
  *) exit 99 ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex, permissions).unwrap();

    let installed = json_output(ctx(&temp).env("PATH", &bin_dir).args([
        "integrations",
        "install",
        "plugin",
        "--agent",
        "codex",
        "--format=json",
    ]));
    assert_eq!(installed["integration"], "plugin");
    assert_eq!(installed["results"][0]["action"], "installed");
    assert_eq!(installed["results"][0]["status"], "installed");
    assert_eq!(installed["results"][0]["installed_version"], "99.0.0");
    assert!(installed["results"][0].get("expected_version").is_none());

    let status = json_output(ctx(&temp).env("PATH", &bin_dir).args([
        "integrations",
        "status",
        "plugin",
        "--agent",
        "codex",
        "--format=json",
    ]));
    assert_eq!(status["results"][0]["action"], "inspected");
    assert_eq!(status["results"][0]["status"], "installed");

    let removed = json_output(ctx(&temp).env("PATH", &bin_dir).args([
        "integrations",
        "remove",
        "plugin",
        "--agent",
        "codex",
        "--format=json",
    ]));
    assert_eq!(removed["results"][0]["action"], "removed");
    assert_eq!(removed["results"][0]["status"], "missing");

    let absent = json_output(ctx(&temp).env("PATH", &bin_dir).args([
        "integrations",
        "remove",
        "plugin",
        "--agent",
        "codex",
        "--format=json",
    ]));
    assert_eq!(absent["results"][0]["action"], "already_absent");
    let state = PathBuf::from(format!("{}.state", codex.display()));
    assert!(state.join("marketplace").is_file());

    fs::write(state.join("legacy"), "").unwrap();
    fs::write(state.join("fail-install"), "").unwrap();
    let failed = ctx(&temp)
        .env("PATH", &bin_dir)
        .args([
            "integrations",
            "install",
            "plugin",
            "--agent",
            "codex",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let failed: Value = serde_json::from_slice(&failed).unwrap();
    assert_eq!(failed["results"][0]["action"], "failed");
    assert_eq!(failed["results"][0]["status"], "legacy_installed");
    assert!(state.join("legacy").is_file());
    assert!(!state.join("current").exists());

    fs::remove_file(state.join("fail-install")).unwrap();
    let migrated = json_output(ctx(&temp).env("PATH", &bin_dir).args([
        "integrations",
        "install",
        "plugin",
        "--agent",
        "codex",
        "--format=json",
    ]));
    assert_eq!(migrated["results"][0]["action"], "installed");
    assert!(state.join("current").is_file());
    assert!(!state.join("legacy").exists());
    assert_eq!(
        fs::read_to_string(format!("{}.log", codex.display())).unwrap(),
        concat!(
            "plugin marketplace list --json\n",
            "plugin list --json\n",
            "plugin marketplace add ctxrs/ctx --json\n",
            "plugin marketplace list --json\n",
            "plugin add ctx@ctx --json\n",
            "plugin list --json\n",
            "plugin marketplace list --json\n",
            "plugin list --json\n",
            "plugin marketplace list --json\n",
            "plugin list --json\n",
            "plugin remove ctx@ctx --json\n",
            "plugin list --json\n",
            "plugin marketplace list --json\n",
            "plugin list --json\n",
            "plugin marketplace list --json\n",
            "plugin list --json\n",
            "plugin add ctx@ctx --json\n",
            "plugin list --json\n",
            "plugin marketplace list --json\n",
            "plugin list --json\n",
            "plugin add ctx@ctx --json\n",
            "plugin list --json\n",
            "plugin remove ctx-agent-history-search@ctx --json\n",
            "plugin list --json\n",
        )
    );
}
