use super::*;

fn context() -> (tempfile::TempDir, Context) {
    let temp = tempfile::tempdir().unwrap();
    let paths = PathContext::for_tests(temp.path().join("home"), temp.path().join("project"));
    let context = Context {
        paths,
        executable: temp.path().join("bin/ctx"),
    };
    (temp, context)
}

#[test]
fn explicit_lifecycle_preserves_user_fields_and_other_hooks() {
    for agent in [Agent::Claude, Agent::Copilot, Agent::Codex] {
        if !agent.supported() {
            continue;
        }
        let (_temp, context) = context();
        let target = path(agent, true, &context);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let existing = json!({"permissions":{"deny":["Bash(rm:*)"]},
            "hooks":{agent.event():[{"matcher":"Other","hooks":[{"command":"/user/hook"}]}],
                "AnotherEvent":[{"command":"/user/other"}]}});
        fs::write(&target, json::render(&existing).unwrap()).unwrap();
        assert_eq!(status(agent, true, &context).unwrap(), State::Missing);
        assert_eq!(install(agent, true, &context).unwrap(), State::Current);
        let first = fs::read(&target).unwrap();
        assert_eq!(install(agent, true, &context).unwrap(), State::Current);
        assert_eq!(first, fs::read(&target).unwrap());
        let installed = document(&target).unwrap().unwrap();
        assert_eq!(installed["permissions"], existing["permissions"]);
        assert_eq!(
            installed["hooks"]["AnotherEvent"],
            existing["hooks"]["AnotherEvent"]
        );
        assert_eq!(
            installed["hooks"][agent.event()][0],
            existing["hooks"][agent.event()][0]
        );
        assert_eq!(status(agent, true, &context).unwrap(), State::Current);
        assert_eq!(remove(agent, true, &context).unwrap(), State::Missing);
        assert_eq!(remove(agent, true, &context).unwrap(), State::Missing);
        let remaining = document(&target).unwrap().unwrap();
        assert_eq!(
            remaining["hooks"][agent.event()],
            existing["hooks"][agent.event()]
        );
        assert_eq!(remaining["permissions"], existing["permissions"]);
    }
}

#[test]
fn legacy_ctx_output_hook_is_upgraded_or_removed_without_touching_user_hooks() {
    for agent in [Agent::Claude, Agent::Copilot, Agent::Codex] {
        if !agent.supported() {
            continue;
        }
        let (_temp, context) = context();
        let target = path(agent, true, &context);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let user_hook = json!({"matcher":"Other","hooks":[{"command":"/user/hook"}]});
        let legacy = expected_route(agent, &context.executable, "output").unwrap();
        let original = json!({"version":1,"permissions":{"deny":["Bash(rm:*)"]},
            "hooks":{agent.event():[user_hook.clone(), legacy.clone()]}});
        fs::write(&target, json::render(&original).unwrap()).unwrap();

        assert_eq!(status(agent, true, &context).unwrap(), State::Legacy);
        assert_eq!(install(agent, true, &context).unwrap(), State::Current);
        let upgraded = document(&target).unwrap().unwrap();
        assert_eq!(upgraded["permissions"], original["permissions"]);
        assert_eq!(upgraded["hooks"][agent.event()][0], user_hook);
        assert_eq!(
            upgraded["hooks"][agent.event()][1],
            expected(agent, &context.executable).unwrap()
        );
        assert_eq!(remove(agent, true, &context).unwrap(), State::Missing);
        assert_eq!(
            document(&target).unwrap().unwrap()["hooks"][agent.event()],
            json!([user_hook])
        );

        fs::write(&target, json::render(&original).unwrap()).unwrap();
        assert_eq!(remove(agent, true, &context).unwrap(), State::Missing);
        assert_eq!(
            document(&target).unwrap().unwrap()["hooks"][agent.event()],
            json!([user_hook])
        );
    }
}

#[test]
fn ambiguous_or_modified_legacy_hook_is_not_replaced() {
    let (_temp, context) = context();
    let target = path(Agent::Claude, true, &context);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let legacy = expected_route(Agent::Claude, &context.executable, "output").unwrap();
    for entries in [
        json!([legacy.clone(), legacy.clone()]),
        json!([{"matcher":"Edited","hooks":legacy["hooks"]}]),
    ] {
        let original = json::render(&json!({"hooks":{"PostToolUse":entries}})).unwrap();
        fs::write(&target, &original).unwrap();
        assert_eq!(
            status(Agent::Claude, true, &context).unwrap(),
            State::Conflict
        );
        assert!(install(Agent::Claude, true, &context).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), original);
    }
}

#[test]
fn standalone_sift_hook_blocks_install_without_mutation() {
    for agent in [Agent::Claude, Agent::Copilot, Agent::Codex] {
        if !agent.supported() {
            continue;
        }
        let (_temp, context) = context();
        let target = if agent == Agent::Copilot {
            path(agent, false, &context).with_file_name("sift.json")
        } else {
            path(agent, false, &context)
        };
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let body = json::render(&json!({"hooks":{agent.event():[{
            "matcher":"Bash", "hooks":[{"command":format!("'sift' hook {}", agent.name())}]
        }]}, "permissions":{"ask":["Bash"]}}))
        .unwrap();
        fs::write(&target, &body).unwrap();
        assert_eq!(status(agent, true, &context).unwrap(), State::SiftConflict);
        assert!(install(agent, true, &context).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), body);
        assert!(!path(agent, true, &context).exists());
    }
}

#[test]
fn modified_ctx_entry_is_not_removed() {
    let (_temp, context) = context();
    let target = path(Agent::Claude, true, &context);
    install(Agent::Claude, true, &context).unwrap();
    let mut value = document(&target).unwrap().unwrap();
    value["hooks"]["PostToolUse"][0]["matcher"] = json!("Edited");
    fs::write(&target, json::render(&value).unwrap()).unwrap();
    assert_eq!(
        status(Agent::Claude, true, &context).unwrap(),
        State::Conflict
    );
    assert_eq!(
        remove(Agent::Claude, true, &context).unwrap(),
        State::Missing
    );
    assert_eq!(document(&target).unwrap().unwrap(), value);
}

#[test]
fn removal_recognizes_older_executable_but_leaves_custom_hook() {
    let (_temp, mut context) = context();
    let target = path(Agent::Claude, true, &context);
    install(Agent::Claude, true, &context).unwrap();
    let original = context.executable.clone();
    context.executable = context.paths.home.join("new/ctx");
    assert_eq!(
        status(Agent::Claude, true, &context).unwrap(),
        State::Conflict
    );
    assert_eq!(
        remove(Agent::Claude, true, &context).unwrap(),
        State::Missing
    );
    let remaining = document(&target).unwrap().unwrap();
    assert_eq!(remaining["hooks"]["PostToolUse"], json!([]));
    assert_ne!(original, context.executable);
}

#[test]
fn local_claude_sift_hook_conflicts_with_project_install() {
    let (_temp, context) = context();
    let target = path(Agent::Claude, true, &context).with_file_name("settings.local.json");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(
        &target,
        json::render(&json!({"hooks":{"PostToolUse":[{
            "matcher":"Bash", "hooks":[{"command":"sift hook claude"}]
        }]}}))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        status(Agent::Claude, true, &context).unwrap(),
        State::SiftConflict
    );
    assert!(install(Agent::Claude, true, &context).is_err());
    assert!(!path(Agent::Claude, true, &context).exists());
}

#[test]
fn copilot_checks_all_effective_hook_sources_before_installing() {
    for (relative, host) in [
        ("home/.copilot/hooks/custom.json", "copilot"),
        ("project/.github/hooks/custom.json", "copilot"),
        ("home/.copilot/settings.json", "copilot"),
        ("project/.github/copilot/settings.json", "copilot"),
        ("project/.github/copilot/settings.local.json", "copilot"),
        ("project/.claude/settings.json", "claude"),
        ("project/.claude/settings.local.json", "claude"),
    ] {
        let (temp, context) = context();
        let source = temp.path().join(relative);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let body = json::render(&json!({"permissions":{"deny":["Bash(rm:*)"]},
            "hooks":{"postToolUse":[{"command":format!("sift hook {host}")}]}}))
        .unwrap();
        fs::write(&source, &body).unwrap();
        assert_eq!(
            status(Agent::Copilot, true, &context).unwrap(),
            State::SiftConflict,
            "missed {relative}"
        );
        assert!(
            install(Agent::Copilot, true, &context).is_err(),
            "installed despite {relative}"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), body);
        assert!(!path(Agent::Copilot, true, &context).exists());
    }
}

#[test]
fn unrelated_copilot_hook_files_remain_untouched() {
    let (_temp, context) = context();
    let source = context.paths.cwd.join(".github/hooks/custom.json");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let body = json::render(&json!({"version":1,"hooks":{"postToolUse":[{
        "type":"command","exec":"/user/custom","args":["check"]
    }]}}))
    .unwrap();
    fs::write(&source, &body).unwrap();
    assert_eq!(
        install(Agent::Copilot, true, &context).unwrap(),
        State::Current
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), body);
    assert_eq!(
        remove(Agent::Copilot, true, &context).unwrap(),
        State::Missing
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), body);
}

#[test]
fn copilot_shell_field_sift_hook_is_a_conflict() {
    let (_temp, context) = context();
    let source = context.paths.cwd.join(".github/hooks/custom.json");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        &source,
        json::render(&json!({"hooks":{"postToolUse":[{
            "type":"command", "bash":"sift hook copilot"
        }]}}))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        status(Agent::Copilot, true, &context).unwrap(),
        State::SiftConflict
    );
    assert!(install(Agent::Copilot, true, &context).is_err());
    assert!(!path(Agent::Copilot, true, &context).exists());
}

#[test]
fn existing_runtime_command_is_registered_with_host_schema() {
    let (_temp, context) = context();
    for agent in [Agent::Claude, Agent::Copilot, Agent::Codex] {
        if !agent.supported() {
            continue;
        }
        install(agent, true, &context).unwrap();
        let value = document(&path(agent, true, &context)).unwrap().unwrap();
        let hook = &value["hooks"][agent.event()][0];
        assert!(contains_invocation(hook, "ctx", agent));
        assert_eq!(hook, &expected(agent, &context.executable).unwrap());
        let raw = json!({"tool_name":"unsupported", "tool_response":{"stdout":"raw"}});
        assert!(!contains_invocation(&raw, "ctx", agent));
    }
}
