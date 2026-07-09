mod support;

use support::*;

const COMMAND_NAME: &str = "ctx-history";
const QUERY: &str = "needle topic with spaces";
const ALL_SLASH_COMMAND_AGENTS: &[&str] = &[
    "codex",
    "claude-code",
    "cursor",
    "opencode",
    "mimocode",
    "gemini-cli",
    "qwen-code",
    "antigravity",
    "github-copilot",
    "pi",
    "goose",
    "continue",
    "windsurf",
];
const SKILL_ONLY_SLASH_COMMAND_AGENTS: &[&str] = &[
    "codex",
    "claude-code",
    "cursor",
    "antigravity",
    "github-copilot",
    "pi",
];
const MANUAL_ONLY_SLASH_COMMAND_AGENTS: &[&str] = &["goose", "continue"];
const WRITABLE_SLASH_COMMAND_AGENTS: &[&str] = &[
    "opencode",
    "mimocode",
    "gemini-cli",
    "qwen-code",
    "windsurf",
];

// These tests are hermetic fake-harness E2E checks. They run the real ctx
// installer binary with temp HOME/XDG/CTX_DATA_ROOT, then simulate the stable
// file-discovery and argument-interpolation behavior documented by each
// provider. Real CLIs and UI reload flows are intentionally not required here;
// optional live smoke can sit outside Bazel because those flows need installed
// third-party tools, auth, or interactive approval.

#[test]
fn slash_command_e2e_detected_global_harnesses_discover_and_invoke() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-config");

    fs::create_dir_all(xdg.join("opencode")).unwrap();
    fs::create_dir_all(xdg.join("mimocode")).unwrap();
    fs::create_dir_all(temp.path().join(".gemini")).unwrap();
    fs::create_dir_all(temp.path().join(".qwen")).unwrap();
    fs::create_dir_all(temp.path().join(".codeium").join("windsurf")).unwrap();

    let output = json_output(ctx(&temp).env("XDG_CONFIG_HOME", &xdg).args([
        "integrations",
        "install",
        "slash-commands",
        "--json",
    ]));

    assert_eq!(output["integration"], "slash-commands");
    assert_eq!(output["scope"], "global");
    assert_eq!(
        output_agents(&output),
        vec![
            "opencode",
            "mimocode",
            "gemini-cli",
            "qwen-code",
            "windsurf"
        ]
    );

    let opencode = OpenCodeHarness::global(&xdg);
    let mimocode = MiMoCodeHarness::global(&xdg);
    let gemini = GeminiHarness::global(temp.path());
    let qwen = QwenHarness::global(temp.path());
    let windsurf = WindsurfHarness::global(temp.path());

    assert_result_path(&output, "opencode", &opencode.command_path(COMMAND_NAME));
    assert_result_path(&output, "mimocode", &mimocode.command_path(COMMAND_NAME));
    assert_result_path(&output, "gemini-cli", &gemini.command_path(COMMAND_NAME));
    assert_result_path(&output, "qwen-code", &qwen.command_path(COMMAND_NAME));
    assert_result_path(&output, "windsurf", &windsurf.command_path(COMMAND_NAME));
    assert_result_paths_under(&output, &[temp.path(), &xdg]);

    assert_ctx_history_prompt(&opencode.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&mimocode.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&gemini.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&qwen.invoke(COMMAND_NAME, QUERY), QUERY);
    windsurf.assert_workflow_ready(COMMAND_NAME);
}

#[test]
fn slash_command_e2e_project_harnesses_discover_and_invoke() {
    let temp = tempdir();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--agent",
        "opencode",
        "--agent",
        "mimocode",
        "--agent",
        "gemini-cli",
        "--agent",
        "qwen-code",
        "--agent",
        "windsurf",
        "--project",
        "--json",
    ]);
    let output = json_output(&mut command);

    assert_eq!(output["scope"], "project");
    assert_eq!(
        output_agents(&output),
        vec![
            "opencode",
            "mimocode",
            "gemini-cli",
            "qwen-code",
            "windsurf"
        ]
    );

    let opencode = OpenCodeHarness::project(&project);
    let mimocode = MiMoCodeHarness::project(&project);
    let gemini = GeminiHarness::project(&project);
    let qwen = QwenHarness::project(&project);
    let windsurf = WindsurfHarness::project(&project);

    assert_result_path(&output, "opencode", &opencode.command_path(COMMAND_NAME));
    assert_result_path(&output, "mimocode", &mimocode.command_path(COMMAND_NAME));
    assert_result_path(&output, "gemini-cli", &gemini.command_path(COMMAND_NAME));
    assert_result_path(&output, "qwen-code", &qwen.command_path(COMMAND_NAME));
    assert_result_path(&output, "windsurf", &windsurf.command_path(COMMAND_NAME));
    assert_result_paths_under(&output, &[&project]);

    assert_ctx_history_prompt(&opencode.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&mimocode.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&gemini.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&qwen.invoke(COMMAND_NAME, QUERY), QUERY);
    windsurf.assert_workflow_ready(COMMAND_NAME);
}

#[test]
fn slash_command_e2e_skill_only_agents_do_not_write_legacy_command_surfaces() {
    let temp = tempdir();

    for agent in [
        "codex",
        "claude-code",
        "cursor",
        "antigravity",
        "github-copilot",
        "pi",
    ] {
        let output = json_output(ctx(&temp).args([
            "integrations",
            "install",
            "slash-commands",
            "--agent",
            agent,
            "--json",
        ]));
        assert_eq!(output["results"][0]["agent"], agent);
        assert_eq!(output["results"][0]["status"], "skill_only");
    }

    assert!(!temp.path().join(".codex").join("prompts").exists());
    assert!(!temp.path().join(".codex").join("commands").exists());
    assert!(!temp.path().join(".claude").join("commands").exists());
    assert!(!temp.path().join(".cursor").join("commands").exists());
}

#[test]
fn slash_command_e2e_manual_only_agents_are_accepted_without_writing_files() {
    let temp = tempdir();

    for agent in ["goose", "continue"] {
        let output = json_output(ctx(&temp).args([
            "integrations",
            "install",
            "slash-commands",
            "--agent",
            agent,
            "--json",
        ]));
        let result = result_for_agent(&output, agent);
        assert_eq!(result["status"], "manual_only");
        assert_eq!(result["previous_status"], "manual_only");
        assert_eq!(result["scope"], Value::Null);
        assert_eq!(result["path"], Value::Null);
        assert_eq!(result["success"], true);
        assert_eq!(result["already_installed"], true);
        assert_eq!(result["updated"], false);
        assert!(result["note"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains(agent));
    }

    assert!(!temp.path().join(".config").join("goose").exists());
    assert!(!temp.path().join(".continue").exists());
}

#[test]
fn slash_command_e2e_all_agents_json_covers_the_complete_accepted_matrix() {
    let temp = tempdir();

    let output = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "slash-commands",
        "--all-agents",
        "--json",
    ]));

    assert_eq!(output["integration"], "slash-commands");
    assert_eq!(output["scope"], "global");
    assert_eq!(output_agents(&output), ALL_SLASH_COMMAND_AGENTS);

    for agent in SKILL_ONLY_SLASH_COMMAND_AGENTS {
        assert_agent_category(&output, agent, "skill_only");
    }
    for agent in MANUAL_ONLY_SLASH_COMMAND_AGENTS {
        assert_agent_category(&output, agent, "manual_only");
    }
    for agent in WRITABLE_SLASH_COMMAND_AGENTS {
        assert_writable_install_result(&output, agent, "global", temp.path());
    }
}

#[test]
fn slash_command_e2e_project_all_agents_json_covers_the_complete_accepted_matrix() {
    let temp = tempdir();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();

    let output = json_output(ctx(&temp).current_dir(&project).args([
        "integrations",
        "install",
        "slash-commands",
        "--all-agents",
        "--project",
        "--json",
    ]));

    assert_eq!(output["integration"], "slash-commands");
    assert_eq!(output["scope"], "project");
    assert_eq!(output_agents(&output), ALL_SLASH_COMMAND_AGENTS);

    for agent in SKILL_ONLY_SLASH_COMMAND_AGENTS {
        assert_agent_category(&output, agent, "skill_only");
    }
    for agent in MANUAL_ONLY_SLASH_COMMAND_AGENTS {
        assert_agent_category(&output, agent, "manual_only");
    }
    for agent in WRITABLE_SLASH_COMMAND_AGENTS {
        assert_writable_install_result(&output, agent, "project", &project);
    }

    let opencode = OpenCodeHarness::project(&project);
    let mimocode = MiMoCodeHarness::project(&project);
    let gemini = GeminiHarness::project(&project);
    let qwen = QwenHarness::project(&project);
    let windsurf = WindsurfHarness::project(&project);

    assert_ctx_history_prompt(&opencode.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&mimocode.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&gemini.invoke(COMMAND_NAME, QUERY), QUERY);
    assert_ctx_history_prompt(&qwen.invoke(COMMAND_NAME, QUERY), QUERY);
    windsurf.assert_workflow_ready(COMMAND_NAME);
}

struct OpenCodeHarness {
    command_dir: PathBuf,
}

impl OpenCodeHarness {
    fn global(xdg_config_home: &Path) -> Self {
        Self {
            command_dir: xdg_config_home.join("opencode").join("commands"),
        }
    }

    fn project(project: &Path) -> Self {
        Self {
            command_dir: project.join(".opencode").join("commands"),
        }
    }

    fn invoke(&self, command_name: &str, args: &str) -> String {
        assert_ctx_metadata(&self.command_dir, &format!("{command_name}.md"));
        let markdown = parse_frontmatter_markdown(&self.command_path(command_name));
        assert_eq!(markdown.description, "Search local agent history with ctx");
        assert!(markdown
            .frontmatter
            .contains("argument-hint: [question or topic]"));
        assert!(
            markdown.body.contains("$ARGUMENTS"),
            "OpenCode needs $ARGUMENTS so multi-word ctx queries survive"
        );
        markdown.body.replace("$ARGUMENTS", args)
    }

    fn command_path(&self, command_name: &str) -> PathBuf {
        command_path(&self.command_dir, command_name, "md")
    }
}

struct MiMoCodeHarness {
    command_dir: PathBuf,
}

impl MiMoCodeHarness {
    fn global(xdg_config_home: &Path) -> Self {
        Self {
            command_dir: xdg_config_home.join("mimocode").join("commands"),
        }
    }

    fn project(project: &Path) -> Self {
        Self {
            command_dir: project.join(".mimocode").join("commands"),
        }
    }

    fn invoke(&self, command_name: &str, args: &str) -> String {
        assert_ctx_metadata(&self.command_dir, &format!("{command_name}.md"));
        let markdown = parse_frontmatter_markdown(&self.command_path(command_name));
        assert_eq!(markdown.description, "Search local agent history with ctx");
        assert!(markdown
            .frontmatter
            .contains("argument-hint: [question or topic]"));
        assert!(
            markdown.body.contains("$ARGUMENTS"),
            "MiMo Code needs $ARGUMENTS so multi-word ctx queries survive"
        );
        markdown.body.replace("$ARGUMENTS", args)
    }

    fn command_path(&self, command_name: &str) -> PathBuf {
        command_path(&self.command_dir, command_name, "md")
    }
}

struct GeminiHarness {
    command_dir: PathBuf,
}

impl GeminiHarness {
    fn global(home: &Path) -> Self {
        Self {
            command_dir: home.join(".gemini").join("commands"),
        }
    }

    fn project(project: &Path) -> Self {
        Self {
            command_dir: project.join(".gemini").join("commands"),
        }
    }

    fn invoke(&self, command_name: &str, args: &str) -> String {
        assert_ctx_metadata(&self.command_dir, &format!("{command_name}.toml"));
        let command = parse_gemini_toml_command(&self.command_path(command_name));
        assert_eq!(command.description, "Search local agent history with ctx");
        assert!(
            command.prompt.contains("{{args}}"),
            "Gemini commands use {{args}} for the full invocation tail"
        );
        command.prompt.replace("{{args}}", args)
    }

    fn command_path(&self, command_name: &str) -> PathBuf {
        command_path(&self.command_dir, command_name, "toml")
    }
}

struct QwenHarness {
    command_dir: PathBuf,
}

impl QwenHarness {
    fn global(home: &Path) -> Self {
        Self {
            command_dir: home.join(".qwen").join("commands"),
        }
    }

    fn project(project: &Path) -> Self {
        Self {
            command_dir: project.join(".qwen").join("commands"),
        }
    }

    fn invoke(&self, command_name: &str, args: &str) -> String {
        assert_ctx_metadata(&self.command_dir, &format!("{command_name}.md"));
        let markdown = parse_frontmatter_markdown(&self.command_path(command_name));
        assert_eq!(markdown.description, "Search local agent history with ctx");
        assert!(
            markdown.body.contains("{{args}}"),
            "Qwen markdown commands use {{args}} for the full invocation tail"
        );
        markdown.body.replace("{{args}}", args)
    }

    fn command_path(&self, command_name: &str) -> PathBuf {
        command_path(&self.command_dir, command_name, "md")
    }
}

struct WindsurfHarness {
    workflow_dir: PathBuf,
}

impl WindsurfHarness {
    fn global(home: &Path) -> Self {
        Self {
            workflow_dir: home
                .join(".codeium")
                .join("windsurf")
                .join("global_workflows"),
        }
    }

    fn project(project: &Path) -> Self {
        Self {
            workflow_dir: project.join(".windsurf").join("workflows"),
        }
    }

    fn assert_workflow_ready(&self, command_name: &str) {
        assert_ctx_metadata(&self.workflow_dir, &format!("{command_name}.md"));
        let body = read_command(&self.command_path(command_name));
        assert!(body.starts_with("# ctx History\n"));
        assert!(body.contains("text after `/ctx-history`"));
        assert!(body.contains("ctx search \"<query>\""));
        assert!(body.contains("ctx show event <id> --window 5"));
        assert!(
            !body.contains("$ARGUMENTS") && !body.contains("{{args}}"),
            "Windsurf workflows are UI/manual instructions, not token interpolation commands"
        );
    }

    fn command_path(&self, command_name: &str) -> PathBuf {
        command_path(&self.workflow_dir, command_name, "md")
    }
}

struct MarkdownCommand {
    description: String,
    frontmatter: String,
    body: String,
}

struct GeminiCommand {
    description: String,
    prompt: String,
}

fn output_agents(output: &Value) -> Vec<&str> {
    output["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["agent"].as_str().unwrap())
        .collect()
}

fn assert_result_path(output: &Value, agent: &str, expected: &Path) {
    let result = result_for_agent(output, agent);
    assert_eq!(result["path"], json!(expected));
}

fn result_for_agent<'a>(output: &'a Value, agent: &str) -> &'a Value {
    output["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["agent"] == agent)
        .unwrap_or_else(|| panic!("missing result for {agent}"))
}

fn assert_agent_category(output: &Value, agent: &str, status: &str) {
    let result = result_for_agent(output, agent);
    assert_eq!(result["status"], status);
    assert_eq!(result["previous_status"], status);
    assert_eq!(result["scope"], Value::Null);
    assert_eq!(result["path"], Value::Null);
    assert_eq!(result["success"], true);
    assert_eq!(result["already_installed"], true);
    assert_eq!(result["updated"], false);
}

fn assert_writable_install_result(output: &Value, agent: &str, scope: &str, root: &Path) {
    let result = result_for_agent(output, agent);
    assert_eq!(result["status"], "current");
    assert_eq!(result["previous_status"], "missing");
    assert_eq!(result["scope"], scope);
    assert!(
        result["path"]
            .as_str()
            .unwrap()
            .starts_with(root.to_str().unwrap()),
        "{agent} path escaped {root:?}: {}",
        result["path"]
    );
    assert_eq!(result["success"], true);
    assert_eq!(result["already_installed"], false);
    assert_eq!(result["updated"], false);
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

fn assert_ctx_metadata(command_dir: &Path, filename: &str) {
    let metadata_path = command_dir.join(".ctx-slash-commands.json");
    let metadata: Value = serde_json::from_str(&read_command(&metadata_path)).unwrap();
    assert_eq!(metadata["schema_version"], 1);
    assert_eq!(metadata["installer"], "ctx-cli");
    assert_eq!(metadata["command_name"], COMMAND_NAME);
    assert!(metadata["files"][filename]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
}

fn command_path(command_dir: &Path, command_name: &str, extension: &str) -> PathBuf {
    command_dir.join(format!("{command_name}.{extension}"))
}

fn read_command(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn parse_frontmatter_markdown(path: &Path) -> MarkdownCommand {
    let body = read_command(path);
    let rest = body
        .strip_prefix("---\n")
        .unwrap_or_else(|| panic!("{} must start with YAML frontmatter", path.display()));
    let end = rest
        .find("\n---\n")
        .unwrap_or_else(|| panic!("{} must close YAML frontmatter", path.display()));
    let frontmatter = &rest[..end];
    let prompt = &rest[end + "\n---\n".len()..];
    let description = frontmatter
        .lines()
        .find_map(|line| line.strip_prefix("description: "))
        .unwrap_or_else(|| panic!("{} frontmatter must include description", path.display()));

    MarkdownCommand {
        description: description.to_owned(),
        frontmatter: frontmatter.to_owned(),
        body: prompt.to_owned(),
    }
}

fn parse_gemini_toml_command(path: &Path) -> GeminiCommand {
    let body = read_command(path);
    let description = body
        .lines()
        .find_map(|line| line.strip_prefix("description = "))
        .and_then(parse_basic_toml_string)
        .unwrap_or_else(|| panic!("{} must include a TOML description", path.display()));
    let prompt_start = "prompt = '''\n";
    let prompt = body
        .split_once(prompt_start)
        .and_then(|(_, rest)| rest.split_once("'''"))
        .map(|(prompt, _)| prompt)
        .unwrap_or_else(|| panic!("{} must include a TOML literal prompt", path.display()));

    GeminiCommand {
        description,
        prompt: prompt.to_owned(),
    }
}

fn parse_basic_toml_string(value: &str) -> Option<String> {
    let quoted = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut parsed = String::new();
    let mut chars = quoted.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            parsed.push(match chars.next()? {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return None,
            });
        } else {
            parsed.push(ch);
        }
    }
    Some(parsed)
}

fn assert_ctx_history_prompt(rendered: &str, query: &str) {
    assert!(rendered.contains("Use ctx to search local coding-agent history"));
    assert!(rendered.contains(&format!("User request: {query}")));
    assert!(rendered.contains("ctx citations"));
    assert!(!rendered.contains("$ARGUMENTS"));
    assert!(!rendered.contains("{{args}}"));
}
