mod support;

use serde_json::{json, Value};
use support::*;
use toml_edit::DocumentMut;

const GLOBAL_MCP_AGENTS: &[&str] = &[
    "codex",
    "claude-code",
    "cursor",
    "opencode",
    "mimocode",
    "gemini-cli",
    "qwen-code",
    "goose",
    "kiro",
    "warp",
    "continue",
    "cline",
    "github-copilot",
    "zed",
    "windsurf",
];

const PROJECT_MCP_AGENTS: &[&str] = &[
    "codex",
    "claude-code",
    "cursor",
    "opencode",
    "mimocode",
    "gemini-cli",
    "qwen-code",
    "kiro",
    "warp",
    "continue",
    "zed",
    "roo-code",
];

#[derive(Debug, PartialEq, Eq)]
enum FakeHarnessState {
    Connected,
    PendingApproval,
}

#[derive(Debug, PartialEq, Eq)]
struct CommandServer {
    command: String,
    args: Vec<String>,
}

type CommandServerReader = fn(&Path) -> Option<CommandServer>;
type PlainMcpCase = (&'static str, CommandServerReader, CommandServerReader);

#[derive(Debug, PartialEq, Eq)]
struct OpenCodeServer {
    command: Vec<String>,
    enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct GooseExtension {
    enabled: bool,
    extension_type: String,
    cmd: String,
    args: Vec<String>,
}

#[test]
fn mcp_server_json_rpc_initialize_and_tools_list_are_usable() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "fake-harness", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "tools",
                "method": "tools/list"
            }),
        ],
    );

    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "ctx");
    assert_eq!(
        responses[0]["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    for expected in [
        "status",
        "sources",
        "search",
        "sql",
        "show_session",
        "show_event",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "missing MCP tool {expected} in {tools:#?}"
        );
    }
}

#[test]
fn codex_global_and_project_install_match_trusted_project_discovery() {
    let temp = tempdir();
    let codex_home = temp.path().join("codex-home");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).env("CODEX_HOME", &codex_home).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "codex",
        "--json",
    ]));
    assert_eq!(
        fake_codex_global(&codex_home),
        Some(CommandServer::ctx_stdio())
    );

    json_output(
        ctx(&temp)
            .env("CODEX_HOME", &codex_home)
            .current_dir(&project)
            .args([
                "integrations",
                "install",
                "mcp",
                "--agent",
                "codex",
                "--project",
                "--json",
            ]),
    );
    assert_eq!(fake_codex_project(&codex_home, &project), None);

    trust_codex_project(&codex_home, &project);
    assert_eq!(
        fake_codex_project(&codex_home, &project),
        Some(CommandServer::ctx_stdio())
    );
}

#[test]
fn claude_global_connects_and_project_config_is_pending_approval() {
    let temp = tempdir();
    let claude_config = temp.path().join("claude-config");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).env("CLAUDE_CONFIG_DIR", &claude_config).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "claude-code",
        "--json",
    ]));
    assert_eq!(
        fake_claude_user(&claude_config),
        Some(FakeHarnessState::Connected)
    );

    json_output(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", &claude_config)
            .current_dir(&project)
            .args([
                "integrations",
                "install",
                "mcp",
                "--agent",
                "claude-code",
                "--project",
                "--json",
            ]),
    );
    assert_eq!(
        fake_claude_project(&project),
        Some(FakeHarnessState::PendingApproval)
    );
}

#[test]
fn qwen_global_connects_and_project_requires_explicit_approval() {
    let temp = tempdir();
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "qwen-code",
        "--json",
    ]));
    assert_eq!(
        fake_qwen_global(temp.path()),
        Some(FakeHarnessState::Connected)
    );

    json_output(ctx(&temp).current_dir(&project).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "qwen-code",
        "--project",
        "--json",
    ]));
    assert_eq!(
        fake_qwen_project(&project),
        Some(FakeHarnessState::PendingApproval)
    );

    fake_qwen_approve_project_server(&project);
    assert_eq!(
        fake_qwen_project(&project),
        Some(FakeHarnessState::Connected)
    );
}

#[test]
fn opencode_global_and_project_configs_are_connected_with_command_array_shape() {
    let temp = tempdir();
    let xdg_config = temp.path().join("xdg-config");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg_config).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "opencode",
        "--json",
    ]));
    assert_eq!(
        fake_opencode_global(&xdg_config),
        Some(OpenCodeServer::ctx_local())
    );

    json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .current_dir(&project)
            .args([
                "integrations",
                "install",
                "mcp",
                "--agent",
                "opencode",
                "--project",
                "--json",
            ]),
    );
    assert_eq!(
        fake_opencode_project(&project),
        Some(OpenCodeServer::ctx_local())
    );
}

#[test]
fn mimocode_global_and_project_configs_are_connected_with_command_array_shape() {
    let temp = tempdir();
    let mimocode_home = temp.path().join("mimocode-home");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).env("MIMOCODE_HOME", &mimocode_home).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "mimo-code",
        "--json",
    ]));
    assert_eq!(
        fake_mimocode_global(&mimocode_home.join("config")),
        Some(OpenCodeServer::ctx_local())
    );

    json_output(ctx(&temp).current_dir(&project).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "mimocode",
        "--project",
        "--json",
    ]));
    assert_eq!(
        fake_mimocode_project(&project),
        Some(OpenCodeServer::ctx_local())
    );
}

#[test]
fn mcp_global_all_agents_json_covers_the_complete_supported_matrix() {
    let temp = tempdir();
    let xdg_config = temp.path().join("xdg-config");
    let codex_home = temp.path().join("codex-home");
    let claude_config = temp.path().join("claude-config");
    let copilot_home = temp.path().join("copilot-home");

    let output = json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("CODEX_HOME", &codex_home)
            .env("CLAUDE_CONFIG_DIR", &claude_config)
            .env("COPILOT_HOME", &copilot_home)
            .args(["integrations", "install", "mcp", "--all-agents", "--json"]),
    );

    assert_eq!(output["integration"], "mcp");
    assert_eq!(output["scope"], "global");
    assert_eq!(output_agents(&output), GLOBAL_MCP_AGENTS);
    for agent in GLOBAL_MCP_AGENTS {
        assert_current_install_result(&output, agent, "global");
    }
    assert_result_paths_under(&output, &[temp.path(), &xdg_config]);

    assert_eq!(
        fake_codex_global(&codex_home),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_claude_user(&claude_config),
        Some(FakeHarnessState::Connected)
    );
    assert_eq!(
        fake_cursor_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_opencode_global(&xdg_config),
        Some(OpenCodeServer::ctx_local())
    );
    assert_eq!(
        fake_mimocode_global(&xdg_config.join("mimocode")),
        Some(OpenCodeServer::ctx_local())
    );
    assert_eq!(
        fake_gemini_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_qwen_global(temp.path()),
        Some(FakeHarnessState::Connected)
    );
    assert_eq!(
        fake_goose_global(&xdg_config),
        Some(GooseExtension::ctx_stdio())
    );
    assert_eq!(
        fake_kiro_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_warp_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_continue_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_cline_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_github_copilot_global(&copilot_home),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_zed_global(&xdg_config),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_windsurf_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
}

#[test]
fn mcp_project_all_agents_json_covers_the_complete_supported_matrix() {
    let temp = tempdir();
    let xdg_config = temp.path().join("xdg-config");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    let output = json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .current_dir(&project)
            .args([
                "integrations",
                "install",
                "mcp",
                "--all-agents",
                "--project",
                "--json",
            ]),
    );

    assert_eq!(output["integration"], "mcp");
    assert_eq!(output["scope"], "project");
    assert_eq!(output_agents(&output), PROJECT_MCP_AGENTS);
    for agent in PROJECT_MCP_AGENTS {
        assert_current_install_result(&output, agent, "project");
    }
    assert_result_paths_under(&output, &[&project]);

    assert_eq!(
        codex_toml_server(&project.join(".codex").join("config.toml")),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_claude_project(&project),
        Some(FakeHarnessState::PendingApproval)
    );
    assert_eq!(
        fake_cursor_project(&project),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_opencode_project(&project),
        Some(OpenCodeServer::ctx_local())
    );
    assert_eq!(
        fake_mimocode_project(&project),
        Some(OpenCodeServer::ctx_local())
    );
    assert_eq!(
        fake_gemini_project(&project),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_qwen_project(&project),
        Some(FakeHarnessState::PendingApproval)
    );
    assert_eq!(
        fake_kiro_project(&project),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_warp_project(&project),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(
        fake_continue_project(&project),
        Some(CommandServer::ctx_stdio())
    );
    assert_eq!(fake_zed_project(&project), Some(CommandServer::ctx_stdio()));
    assert_eq!(fake_roo_project(&project), Some(CommandServer::ctx_stdio()));
}

#[test]
fn cursor_global_and_project_configs_are_stdio_mcp_servers() {
    let temp = tempdir();
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "cursor",
        "--json",
    ]));
    assert_eq!(
        fake_cursor_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );

    json_output(ctx(&temp).current_dir(&project).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "cursor",
        "--project",
        "--json",
    ]));
    assert_eq!(
        fake_cursor_project(&project),
        Some(CommandServer::ctx_stdio())
    );
}

#[test]
fn gemini_kiro_and_warp_global_and_project_configs_are_plain_mcp_servers() {
    let cases: &[PlainMcpCase] = &[
        ("gemini-cli", fake_gemini_global, fake_gemini_project),
        ("kiro", fake_kiro_global, fake_kiro_project),
        ("warp", fake_warp_global, fake_warp_project),
    ];

    for (agent, global_reader, project_reader) in cases {
        let temp = tempdir();
        let project = temp.path().join(format!("workspace-{agent}"));
        fs::create_dir_all(&project).unwrap();

        json_output(ctx(&temp).args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            agent,
            "--json",
        ]));
        assert_eq!(
            global_reader(temp.path()),
            Some(CommandServer::ctx_stdio()),
            "{agent} global config"
        );

        json_output(ctx(&temp).current_dir(&project).args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            agent,
            "--project",
            "--json",
        ]));
        assert_eq!(
            project_reader(&project),
            Some(CommandServer::ctx_stdio()),
            "{agent} project config"
        );
    }
}

#[test]
fn zed_global_and_project_configs_use_context_servers_root() {
    let temp = tempdir();
    let xdg_config = temp.path().join("xdg-config");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg_config).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "zed",
        "--json",
    ]));
    assert_eq!(
        fake_zed_global(&xdg_config),
        Some(CommandServer::ctx_stdio())
    );

    json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .current_dir(&project)
            .args([
                "integrations",
                "install",
                "mcp",
                "--agent",
                "zed",
                "--project",
                "--json",
            ]),
    );
    assert_eq!(fake_zed_project(&project), Some(CommandServer::ctx_stdio()));
}

#[test]
fn cline_copilot_and_windsurf_global_configs_use_native_json_shapes() {
    let temp = tempdir();
    let copilot_home = temp.path().join("copilot-home");

    json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "cline",
        "--json",
    ]));
    assert_eq!(
        fake_cline_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );

    json_output(ctx(&temp).env("COPILOT_HOME", &copilot_home).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "github-copilot",
        "--json",
    ]));
    assert_eq!(
        fake_github_copilot_global(&copilot_home),
        Some(CommandServer::ctx_stdio())
    );

    json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "windsurf",
        "--json",
    ]));
    assert_eq!(
        fake_windsurf_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );
}

#[test]
fn goose_global_config_is_connected_and_project_config_is_unsupported() {
    let temp = tempdir();
    let xdg_config = temp.path().join("xdg-config");
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg_config).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "goose",
        "--json",
    ]));
    assert_eq!(
        fake_goose_global(&xdg_config),
        Some(GooseExtension::ctx_stdio())
    );

    let goose_project = ctx(&temp)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .current_dir(&project)
        .args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            "goose",
            "--project",
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let goose_project: Value = serde_json::from_slice(&goose_project).unwrap();
    assert_eq!(goose_project["results"][0]["agent"], "goose");
    assert_eq!(goose_project["results"][0]["scope"], "project");
    assert_eq!(goose_project["results"][0]["status"], "unsupported");
    assert_eq!(goose_project["results"][0]["supported"], false);

    let goose_project_status = json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .current_dir(&project)
            .args([
                "integrations",
                "status",
                "mcp",
                "--agent",
                "goose",
                "--project",
                "--json",
            ]),
    );
    assert_eq!(goose_project_status["results"][0]["agent"], "goose");
    assert_eq!(goose_project_status["results"][0]["scope"], "project");
    assert_eq!(goose_project_status["results"][0]["status"], "unsupported");
    assert_eq!(goose_project_status["results"][0]["supported"], false);
}

#[test]
fn continue_yaml_and_roo_project_only_are_covered() {
    let temp = tempdir();
    let project = temp.path().join("workspace");
    fs::create_dir_all(&project).unwrap();

    json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "continue",
        "--json",
    ]));
    assert_eq!(
        fake_continue_global(temp.path()),
        Some(CommandServer::ctx_stdio())
    );

    json_output(ctx(&temp).current_dir(&project).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "continue",
        "--project",
        "--json",
    ]));
    assert_eq!(
        fake_continue_project(&project),
        Some(CommandServer::ctx_stdio())
    );

    json_output(ctx(&temp).current_dir(&project).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "roo-code",
        "--project",
        "--json",
    ]));
    assert_eq!(fake_roo_project(&project), Some(CommandServer::ctx_stdio()));

    let roo_global = ctx(&temp)
        .args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            "roo-code",
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let roo_global: Value = serde_json::from_slice(&roo_global).unwrap();
    assert_eq!(roo_global["results"][0]["status"], "unsupported");
    assert_eq!(roo_global["results"][0]["supported"], false);
}

impl CommandServer {
    fn ctx_stdio() -> Self {
        Self {
            command: "ctx".to_owned(),
            args: vec!["mcp".to_owned(), "serve".to_owned()],
        }
    }
}

impl OpenCodeServer {
    fn ctx_local() -> Self {
        Self {
            command: vec!["ctx".to_owned(), "mcp".to_owned(), "serve".to_owned()],
            enabled: true,
        }
    }
}

impl GooseExtension {
    fn ctx_stdio() -> Self {
        Self {
            enabled: true,
            extension_type: "stdio".to_owned(),
            cmd: "ctx".to_owned(),
            args: vec!["mcp".to_owned(), "serve".to_owned()],
        }
    }
}

fn fake_codex_global(codex_home: &Path) -> Option<CommandServer> {
    codex_toml_server(&codex_home.join("config.toml"))
}

fn fake_codex_project(codex_home: &Path, project: &Path) -> Option<CommandServer> {
    if !fake_codex_trusts_project(codex_home, project) {
        return None;
    }
    codex_toml_server(&project.join(".codex").join("config.toml"))
}

fn fake_codex_trusts_project(codex_home: &Path, project: &Path) -> bool {
    let doc = read_toml(&codex_home.join("config.toml"));
    doc.get("projects")
        .and_then(toml_edit::Item::as_table)
        .and_then(|projects| projects.get(project.to_string_lossy().as_ref()))
        .and_then(toml_edit::Item::as_table)
        .and_then(|project| project.get("trust_level"))
        .and_then(toml_edit::Item::as_str)
        == Some("trusted")
}

fn trust_codex_project(codex_home: &Path, project: &Path) {
    let config = codex_home.join("config.toml");
    let mut body = fs::read_to_string(&config).unwrap();
    body.push_str(&format!(
        "\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
        escape_toml_basic_string(&project.to_string_lossy())
    ));
    fs::write(config, body).unwrap();
}

fn codex_toml_server(path: &Path) -> Option<CommandServer> {
    let doc = read_toml(path);
    let table = doc.get("mcp_servers")?.as_table()?.get("ctx")?.as_table()?;
    let command = table.get("command")?.as_str()?.to_owned();
    let args = table
        .get("args")?
        .as_array()?
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    Some(CommandServer { command, args })
}

fn read_toml(path: &Path) -> DocumentMut {
    fs::read_to_string(path)
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap()
}

fn output_agents(output: &Value) -> Vec<&str> {
    output["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["agent"].as_str().unwrap())
        .collect()
}

fn result_for_agent<'a>(output: &'a Value, agent: &str) -> &'a Value {
    output["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["agent"] == agent)
        .unwrap_or_else(|| panic!("missing result for {agent}"))
}

fn assert_current_install_result(output: &Value, agent: &str, scope: &str) {
    let result = result_for_agent(output, agent);
    assert_eq!(result["scope"], scope);
    assert_eq!(result["supported"], true);
    assert_eq!(result["success"], true);
    assert_eq!(result["previous_status"], "missing");
    assert_eq!(result["status"], "current");
    assert_eq!(result["already_installed"], false);
    assert_eq!(result["modified"], true);
    assert!(result["path"].as_str().is_some());
}

fn assert_result_paths_under(output: &Value, roots: &[&Path]) {
    for result in output["results"].as_array().unwrap() {
        let path = result["path"].as_str().unwrap();
        assert!(
            roots.iter().any(|root| Path::new(path).starts_with(root)),
            "result path {path} escaped temp roots {roots:?}"
        );
    }
}

fn fake_claude_user(claude_config: &Path) -> Option<FakeHarnessState> {
    json_mcp_stdio_server(&claude_config.join(".claude.json")).map(|_| FakeHarnessState::Connected)
}

fn fake_claude_project(project: &Path) -> Option<FakeHarnessState> {
    json_mcp_stdio_server(&project.join(".mcp.json")).map(|_| FakeHarnessState::PendingApproval)
}

fn fake_qwen_global(home: &Path) -> Option<FakeHarnessState> {
    json_mcp_plain_server(&home.join(".qwen").join("settings.json"))
        .map(|_| FakeHarnessState::Connected)
}

fn fake_qwen_project(project: &Path) -> Option<FakeHarnessState> {
    json_mcp_plain_server(&project.join(".qwen").join("settings.json"))?;
    if project.join(".qwen").join("approved-mcp-ctx").exists() {
        Some(FakeHarnessState::Connected)
    } else {
        Some(FakeHarnessState::PendingApproval)
    }
}

fn fake_qwen_approve_project_server(project: &Path) {
    let marker = project.join(".qwen").join("approved-mcp-ctx");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(marker, "ctx\n").unwrap();
}

fn fake_opencode_global(xdg_config: &Path) -> Option<OpenCodeServer> {
    json_opencode_server(&xdg_config.join("opencode").join("opencode.json"))
}

fn fake_opencode_project(project: &Path) -> Option<OpenCodeServer> {
    json_opencode_server(&project.join("opencode.json"))
}

fn fake_mimocode_global(config_dir: &Path) -> Option<OpenCodeServer> {
    json_opencode_server(&config_dir.join("mimocode.jsonc"))
}

fn fake_mimocode_project(project: &Path) -> Option<OpenCodeServer> {
    json_opencode_server(&project.join(".mimocode").join("mimocode.jsonc"))
}

fn fake_cursor_global(home: &Path) -> Option<CommandServer> {
    json_mcp_stdio_server(&home.join(".cursor").join("mcp.json"))
}

fn fake_cursor_project(project: &Path) -> Option<CommandServer> {
    json_mcp_stdio_server(&project.join(".cursor").join("mcp.json"))
}

fn fake_gemini_global(home: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&home.join(".gemini").join("settings.json"))
}

fn fake_gemini_project(project: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&project.join(".gemini").join("settings.json"))
}

fn fake_kiro_global(home: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&home.join(".kiro").join("settings").join("mcp.json"))
}

fn fake_kiro_project(project: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&project.join(".kiro").join("settings").join("mcp.json"))
}

fn fake_warp_global(home: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&home.join(".warp").join(".mcp.json"))
}

fn fake_warp_project(project: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&project.join(".warp").join(".mcp.json"))
}

fn fake_cline_global(home: &Path) -> Option<CommandServer> {
    json_cline_server(&home.join(".cline").join("mcp.json"))
}

fn fake_github_copilot_global(copilot_home: &Path) -> Option<CommandServer> {
    json_copilot_server(&copilot_home.join("mcp-config.json"))
}

fn fake_zed_global(xdg_config: &Path) -> Option<CommandServer> {
    json_context_server(&xdg_config.join("zed").join("settings.json"))
}

fn fake_zed_project(project: &Path) -> Option<CommandServer> {
    json_context_server(&project.join(".zed").join("settings.json"))
}

fn fake_windsurf_global(home: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&home.join(".codeium").join("mcp_config.json"))
}

fn fake_goose_global(xdg_config: &Path) -> Option<GooseExtension> {
    let root: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(xdg_config.join("goose").join("config.yaml")).unwrap(),
    )
    .unwrap();
    let extension = yaml_mapping_get(yaml_mapping_get(&root, "extensions")?, "ctx")?;
    let enabled = yaml_mapping_get(extension, "enabled")?.as_bool()?;
    let extension_type = yaml_mapping_get(extension, "type")?.as_str()?.to_owned();
    let cmd = yaml_mapping_get(extension, "cmd")?.as_str()?.to_owned();
    let args = yaml_mapping_get(extension, "args")?
        .as_sequence()?
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    Some(GooseExtension {
        enabled,
        extension_type,
        cmd,
        args,
    })
}

fn fake_continue_global(home: &Path) -> Option<CommandServer> {
    continue_yaml_server(&home.join(".continue").join("config.yaml"))
}

fn fake_continue_project(project: &Path) -> Option<CommandServer> {
    continue_yaml_server(
        &project
            .join(".continue")
            .join("mcpServers")
            .join("ctx.yaml"),
    )
}

fn fake_roo_project(project: &Path) -> Option<CommandServer> {
    json_mcp_plain_server(&project.join(".roo").join("mcp.json"))
}

fn json_mcp_stdio_server(path: &Path) -> Option<CommandServer> {
    let server = json_mcp_server(path)?;
    assert_eq!(server.get("type").and_then(Value::as_str), Some("stdio"));
    command_server_from_json(server)
}

fn json_mcp_plain_server(path: &Path) -> Option<CommandServer> {
    command_server_from_json(json_mcp_server(path)?)
}

fn json_cline_server(path: &Path) -> Option<CommandServer> {
    let server = json_mcp_server(path)?;
    assert_eq!(server.get("disabled").and_then(Value::as_bool), Some(false));
    let auto_approve = server.get("autoApprove")?.as_array()?;
    assert!(auto_approve.is_empty());
    command_server_from_json(server)
}

fn json_copilot_server(path: &Path) -> Option<CommandServer> {
    let server = json_mcp_server(path)?;
    assert_eq!(server.get("type").and_then(Value::as_str), Some("local"));
    let tools = server.get("tools")?.as_array()?;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].as_str(), Some("*"));
    command_server_from_json(server)
}

fn json_mcp_server(path: &Path) -> Option<&'static Value> {
    let root = read_json_leaked(path);
    root.get("mcpServers")?.get("ctx")
}

fn json_context_server(path: &Path) -> Option<CommandServer> {
    let root = read_json_leaked(path);
    assert!(
        root.get("mcpServers")
            .and_then(|servers| servers.get("ctx"))
            .is_none(),
        "Zed MCP config must use context_servers.ctx, not mcpServers.ctx"
    );
    command_server_from_json(root.get("context_servers")?.get("ctx")?)
}

fn command_server_from_json(server: &Value) -> Option<CommandServer> {
    let command = server.get("command")?.as_str()?.to_owned();
    let args = server
        .get("args")?
        .as_array()?
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    Some(CommandServer { command, args })
}

fn json_opencode_server(path: &Path) -> Option<OpenCodeServer> {
    let root = read_json_leaked(path);
    let server = root.get("mcp")?.get("ctx")?;
    assert_eq!(server.get("type").and_then(Value::as_str), Some("local"));
    let command = server
        .get("command")?
        .as_array()?
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    let enabled = server.get("enabled")?.as_bool()?;
    Some(OpenCodeServer { command, enabled })
}

fn continue_yaml_server(path: &Path) -> Option<CommandServer> {
    let root: serde_yaml::Value = serde_yaml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let servers = yaml_mapping_get(&root, "mcpServers")?.as_sequence()?;
    let server = servers.iter().find(|server| {
        yaml_mapping_get(server, "name").and_then(serde_yaml::Value::as_str) == Some("ctx")
    })?;
    assert_eq!(
        yaml_mapping_get(server, "type").and_then(serde_yaml::Value::as_str),
        Some("stdio")
    );
    let command = yaml_mapping_get(server, "command")?.as_str()?.to_owned();
    let args = yaml_mapping_get(server, "args")?
        .as_sequence()?
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    Some(CommandServer { command, args })
}

fn yaml_mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_owned()))
}

fn read_json_leaked(path: &Path) -> &'static Value {
    let value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    Box::leak(Box::new(value))
}

fn escape_toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
