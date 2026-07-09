mod support;

use sha2::{Digest, Sha256};
use support::*;

const CURRENT_BUNDLED_SKILL_BODY: &str =
    include_str!("../../../skills/ctx-agent-history-search/SKILL.md");

const INITIAL_PUBLIC_BUNDLED_SKILL_BODY: &str = r#"---
name: ctx-agent-history-search
description: Use ctx to search local coding-agent history before acting. Use when prior agent sessions may contain relevant commands, attempts, decisions, source citations, or transcript context.
---

# ctx Agent History Search

Use ctx as a local retrieval tool before repeating investigation work. Treat ctx
output as cited source material from local transcripts, not as generated
analysis.

## Prerequisites

- Require the `ctx` CLI to be installed and set up.
- Start with `ctx status --json`.
- If `ctx` is missing or not set up, tell the user the local history index is
  unavailable and do not invent results.

## Workflow

1. Check health:

   ```bash
   ctx status --json
   ```

2. Inspect available provider sources:

   ```bash
   ctx sources --json
   ```

3. Re-import only when recent local history matters or search misses something
   the user says should exist:

   ```bash
   ctx import --all --json
   ctx import --resume --json
   ```

   Treat `--resume` as an idempotent rescan marker, not a guarantee that every
   provider has native cursor resume.

4. Search with tight filters whenever possible:

   ```bash
   ctx search "<query>" --json
   ctx search "<query>" --provider codex --json
   ctx search "<query>" --repo <repo> --json
   ctx search "<query>" --file <path> --json
   ctx search "<query>" --since 30d --json
   ```

5. Inspect the best cited result before relying on it:

   ```bash
   ctx show event <ctx-event-id> --window 5 --format json
   ctx show session <ctx-session-id> --mode lite --format json
   ```

6. Locate original provider material when source identity or resume hints matter:

   ```bash
   ctx locate event <ctx-event-id> --format json
   ctx locate session <ctx-session-id> --format json
   ```

7. Export a transcript only when another agent or artifact needs a file:

   ```bash
   ctx export session <ctx-session-id> --mode lite --format markdown --out /tmp/ctx-session.md
   ```

## Citation Rules

- Cite ctx material when it affects your answer or implementation.
- Include the provider, ctx session ID, ctx event ID when available, provider
  session ID when available, and source path or cursor when present.
- If you synthesize across multiple snippets, label the conclusion as your
  synthesis and cite the supporting snippets.
- If a source citation is stale or unavailable, say ctx returned indexed text
  but the raw source could not be opened.

## Safety Rules

- Prefer JSON for ranking and routing.
- Do not say ctx inferred a decision unless the cited text explicitly states
  that decision.
- Do not state that ctx wrote model analysis.
- Treat `~/.ctx`, provider transcript paths, and JSON output as private local
  history unless the user explicitly asks to share reviewed excerpts.
- Use typed IDs. Do not fall back to old ambiguous `ctx show <uuid>` behavior.
"#;

fn bundled_skill_hash(body: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
}

#[test]
fn skill_install_defaults_to_global_canonical_agents_dir_and_is_idempotent() {
    let temp = tempdir();

    let first = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "install", "skills", "--json"]),
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
            .args(["integrations", "install", "skills", "--json"]),
    );
    assert_eq!(second["results"][0]["previous_status"], "current");
    assert_eq!(second["results"][0]["already_installed"], true);
    assert_eq!(second["results"][0]["updated"], false);

    let status = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["integrations", "status", "skills", "--json"]),
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
            .args(["integrations", "install", "skills", "--json"]),
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

    let stale = json_output(ctx(&temp).args([
        "integrations",
        "status",
        "skills",
        "--agent",
        "universal",
        "--json",
    ]));
    assert_eq!(stale["results"][0]["status"], "stale");

    let install = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--json",
    ]));
    assert_eq!(install["results"][0]["previous_status"], "stale");
    assert_eq!(install["results"][0]["updated"], true);
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("ctx Agent History Search"));
}

#[test]
fn skill_install_auto_upgrades_legacy_bundled_copy_without_metadata() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        INITIAL_PUBLIC_BUNDLED_SKILL_BODY,
    )
    .unwrap();
    assert_eq!(
        bundled_skill_hash(INITIAL_PUBLIC_BUNDLED_SKILL_BODY),
        "sha256:c0647d2368714b09a5f652583b9f2c34e88502b0ab441ba44c4698313675dbcc"
    );

    let stale = json_output(ctx(&temp).args([
        "integrations",
        "status",
        "skills",
        "--agent",
        "universal",
        "--json",
    ]));
    assert_eq!(stale["results"][0]["status"], "stale");
    assert_eq!(
        stale["results"][0]["installed_hash"],
        "sha256:c0647d2368714b09a5f652583b9f2c34e88502b0ab441ba44c4698313675dbcc"
    );

    let install = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "skills",
        "--agent",
        "universal",
        "--json",
    ]));
    assert_eq!(install["results"][0]["success"], true);
    assert_eq!(install["results"][0]["previous_status"], "stale");
    assert_eq!(install["results"][0]["status"], "current");
    assert_eq!(install["results"][0]["updated"], true);
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        CURRENT_BUNDLED_SKILL_BODY
    );

    let metadata: Value =
        serde_json::from_slice(&fs::read(skill_dir.join(".ctx-skill.json")).unwrap()).unwrap();
    assert_eq!(metadata["skill_name"], "ctx-agent-history-search");
    assert_eq!(
        metadata["skill_hash"].as_str().unwrap(),
        bundled_skill_hash(CURRENT_BUNDLED_SKILL_BODY)
    );
    assert_eq!(metadata["ctx_cli_version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn skill_install_backfills_current_metadata_without_rewriting_body() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), CURRENT_BUNDLED_SKILL_BODY).unwrap();
    fs::write(
        skill_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx-agent-history-search",
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
        "--json",
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
    assert_eq!(metadata["ctx_cli_version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn skill_install_default_fallback_preserves_custom_copy_without_failing() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "local custom instructions\n").unwrap();

    ctx(&temp)
        .args(["integrations", "install", "skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "preserved existing Universal .agents skill; use --force to replace",
        ));
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "local custom instructions\n"
    );
    assert!(!skill_dir.join(".ctx-skill.json").exists());
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
        .args([
            "integrations",
            "install",
            "skills",
            "--agent",
            "universal",
            "--json",
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
                "--json",
            ]),
    );
    assert_eq!(global["results"].as_array().unwrap().len(), 4);
    assert!(codex_home
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(mimocode_home
        .join("config")
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
    assert!(project
        .join(".mimocode")
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
