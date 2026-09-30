use super::*;

#[test]
fn managed_root_selection_and_command_override_use_their_own_config() {
    let sandbox = Sandbox::new();
    sandbox.write("history/config.toml", "[indexing]\nmode = \"manual\"\n");
    sandbox.write("override/config.toml", "[indexing]\nmode = \"auto\"\n");
    let selected = json_output(
        sandbox
            .command()
            .env_remove("CTX_DAEMON_ENABLED")
            .args(["status", "--format=json"])
            .output()
            .unwrap(),
    );
    assert_eq!(selected["indexing"]["mode"], "manual");
    assert_eq!(
        selected["lexical"]["path"],
        json!(sandbox.root.join("history/search/lexical"))
    );
    let overridden = json_output(
        sandbox
            .command()
            .env_remove("CTX_DAEMON_ENABLED")
            .args(["--data-root", "../override", "status", "--format=json"])
            .output()
            .unwrap(),
    );
    assert_eq!(overridden["indexing"]["mode"], "auto");
    assert!(!sandbox.root.join("home/.ctx").exists());

    for value in [None, Some("")] {
        let mut command = sandbox.command();
        match value {
            None => {
                command.env_remove("CTX_DATA_ROOT");
            }
            Some(value) => {
                command.env("CTX_DATA_ROOT", value);
            }
        }
        let default = json_output(command.args(["status", "--format=json"]).output().unwrap());
        assert_eq!(
            default["lexical"]["path"],
            json!(sandbox.root.join("home/.ctx/search/lexical"))
        );
    }
    assert!(!sandbox.root.join("home/.ctx").exists());
}

#[test]
fn invalid_managed_root_fails_without_writing_a_fallback() {
    let sandbox = Sandbox::new();
    for value in ["relative", " ", "/tmp/ctx\nroot"] {
        for override_root in [false, true] {
            let mut command = sandbox.command();
            command.env("CTX_DATA_ROOT", value);
            if override_root {
                command.args(["--data-root", "../override"]);
            }
            let output = command.args(["status", "--format=json"]).output().unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("CTX_DATA_ROOT"));
        }
    }
    for root in ["history", "override", "home/.ctx"] {
        assert!(!sandbox.root.join(root).exists());
    }
}

#[test]
fn invalid_managed_root_keeps_independent_engines_and_partial_search_available() {
    let sandbox = Sandbox::new();
    sandbox.write("repo/workflow.py", PYTHON_SOURCE);
    sandbox.graph(&["index", "."]);
    let before = sandbox.protected_state();
    for global_options in [false, true] {
        let mut output_command = sandbox.command();
        output_command.env("CTX_DATA_ROOT", "relative");
        if global_options {
            output_command.args(["--color", "never"]);
        }
        let output = output_command
            .args(["sift", "run", "--raw", "--"])
            .arg(&sandbox.binary)
            .args(["sift", "restore", "--encoding=raw"])
            .write_stdin("independent output")
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(output.stdout, b"independent output");
    }
    let graph = json_output(
        sandbox
            .command()
            .env("CTX_DATA_ROOT", "relative")
            .args(["--color", "never", "graph", "callers", "kernel", "--json"])
            .output()
            .unwrap(),
    );
    assert_call(&graph, "launch", "kernel");
    for scope in ["graph", "all"] {
        let result = json_output(
            sandbox
                .command()
                .env("CTX_DATA_ROOT", "relative")
                .args([
                    "search",
                    "kernel",
                    "--scope",
                    scope,
                    "--limit=5",
                    "--format=json",
                ])
                .output()
                .unwrap(),
        );
        assert_search_envelope(&result, scope, 5, scope == "all");
        assert_eq!(result["graph"]["status"], "ok");
        if scope == "all" {
            assert_unavailable(&result["history"]);
            assert!(result["history"]["error"]
                .as_str()
                .unwrap()
                .contains("CTX_DATA_ROOT"));
        }
    }
    assert_eq!(sandbox.protected_state(), before);
}
