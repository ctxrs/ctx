#![cfg(unix)]

mod support;

use support::*;

const CURRENT_BUNDLED_SKILL: &[u8] = include_bytes!("../../../../skills/ctx/SKILL.md");
const RELEASED_LEGACY_SKILL: &[u8] =
    include_bytes!("../../../ctx-agent-integrations/src/skill/testdata/legacy_skill_v0_17_0.md");
const METADATA_FILE: &str = ".ctx-skill.json";

#[derive(Debug)]
struct IsolatedAgentRoots {
    codex: PathBuf,
    grok: PathBuf,
    claude: PathBuf,
    mimocode_home: PathBuf,
    mimocode_config: PathBuf,
    xdg_config: PathBuf,
}

impl IsolatedAgentRoots {
    fn new(temp: &TempDir) -> Self {
        let root = temp.path().join("agent-roots");
        Self {
            codex: root.join("codex"),
            grok: root.join("grok"),
            claude: root.join("claude"),
            mimocode_home: root.join("mimocode-home"),
            mimocode_config: root.join("mimocode-config"),
            xdg_config: root.join("xdg-config"),
        }
    }

    fn absent_skill_bases(&self, temp: &TempDir) -> Vec<PathBuf> {
        vec![
            self.grok.join("skills"),
            self.claude.join("skills"),
            temp.path().join(".cursor/skills"),
            self.xdg_config.join("opencode/skills"),
            self.mimocode_config.join("skills"),
            self.xdg_config.join("agents/skills"),
            temp.path().join(".gemini/skills"),
            temp.path().join(".gemini/antigravity/skills"),
            temp.path().join(".gemini/antigravity-cli/skills"),
            temp.path().join(".copilot/skills"),
            temp.path().join(".pi/agent/skills"),
            self.xdg_config.join("goose/skills"),
        ]
    }
}

#[derive(Debug)]
struct SkillSnapshot {
    body: Vec<u8>,
    metadata: Vec<u8>,
}

fn isolated_ctx(temp: &TempDir, binary: &Path, roots: &IsolatedAgentRoots) -> Command {
    let mut command = ctx_from_binary(temp, binary);
    command
        .env("CODEX_HOME", &roots.codex)
        .env("GROK_HOME", &roots.grok)
        .env("CLAUDE_CONFIG_DIR", &roots.claude)
        .env("MIMOCODE_HOME", &roots.mimocode_home)
        .env("MIMOCODE_CONFIG_DIR", &roots.mimocode_config)
        .env("XDG_CONFIG_HOME", &roots.xdg_config)
        .env("USERPROFILE", temp.path())
        .env("CTX_UPGRADE_OFF", "1");
    command
}

fn write_stale_managed_skill(skill_dir: &Path) -> SkillSnapshot {
    let body = b"stale metadata-owned ctx skill\n".to_vec();
    let metadata = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "installer": "ctx-cli",
        "skill_name": "ctx",
        "skill_hash": format!("sha256:{}", sha256_hex(&body)),
        "ctx_cli_version": "0.9.0",
        "installed_at": "2026-01-01T00:00:00Z",
    }))
    .unwrap();
    fs::create_dir_all(skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), &body).unwrap();
    fs::write(skill_dir.join(METADATA_FILE), &metadata).unwrap();
    SkillSnapshot { body, metadata }
}

fn assert_current_skill(skill_dir: &Path) {
    assert_eq!(
        fs::read(skill_dir.join("SKILL.md")).unwrap(),
        CURRENT_BUNDLED_SKILL
    );
    let metadata: Value =
        serde_json::from_slice(&fs::read(skill_dir.join(METADATA_FILE)).unwrap()).unwrap();
    assert_eq!(metadata["schema_version"], 1);
    assert_eq!(metadata["installer"], "ctx-cli");
    assert_eq!(metadata["skill_name"], "ctx");
    assert_eq!(
        metadata["skill_hash"],
        format!("sha256:{}", sha256_hex(CURRENT_BUNDLED_SKILL))
    );
    assert_eq!(metadata["ctx_cli_version"], env!("CARGO_PKG_VERSION"));
}

fn assert_skill_unchanged(skill_dir: &Path, before: &SkillSnapshot) {
    assert_eq!(fs::read(skill_dir.join("SKILL.md")).unwrap(), before.body);
    assert_eq!(
        fs::read(skill_dir.join(METADATA_FILE)).unwrap(),
        before.metadata
    );
}

fn assert_skill_base_absent(base: &Path) {
    assert!(!base.join("ctx").exists(), "{}", base.display());
    assert!(
        !base.join("ctx-agent-history-search").exists(),
        "{}",
        base.display()
    );
}

#[test]
fn first_mutation_eligible_startup_refreshes_current_and_migrates_one_legacy() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "managed_skill_refresh_startup");
    let roots = IsolatedAgentRoots::new(&temp);

    let universal_skill = temp.path().join(".agents/skills/ctx");
    write_stale_managed_skill(&universal_skill);

    let codex_skills = roots.codex.join("skills");
    let legacy_skill = codex_skills.join("ctx-agent-history-search");
    fs::create_dir_all(&legacy_skill).unwrap();
    fs::write(legacy_skill.join("SKILL.md"), RELEASED_LEGACY_SKILL).unwrap();

    let absent_skill_bases = roots.absent_skill_bases(&temp);
    for base in &absent_skill_bases {
        assert_skill_base_absent(base);
    }

    isolated_ctx(&temp, &binary, &roots)
        .args(["setup", "--no-daemon", "--progress", "plain"])
        .assert()
        .success();

    assert_current_skill(&universal_skill);
    assert_current_skill(&codex_skills.join("ctx"));
    assert!(!legacy_skill.join("SKILL.md").exists());
    for base in &absent_skill_bases {
        assert_skill_base_absent(base);
    }
}

#[test]
fn absent_corrupt_and_wrong_version_markers_prevent_startup_mutation() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "managed_skill_refresh_marker_gate");
    let marker_path = install_marker_path(&binary);
    let valid_marker = fs::read(&marker_path).unwrap();
    let roots = IsolatedAgentRoots::new(&temp);
    let skill_dir = temp.path().join(".agents/skills/ctx");
    let before = write_stale_managed_skill(&skill_dir);

    fs::remove_file(&marker_path).unwrap();
    isolated_ctx(&temp, &binary, &roots)
        .args(["setup", "--no-daemon", "--progress", "plain"])
        .assert()
        .success();
    assert!(!marker_path.exists());
    assert_skill_unchanged(&skill_dir, &before);

    let corrupt_marker = b"{not-json";
    fs::write(&marker_path, corrupt_marker).unwrap();
    isolated_ctx(&temp, &binary, &roots)
        .args(["setup", "--no-daemon", "--progress", "plain"])
        .assert()
        .success();
    assert_eq!(fs::read(&marker_path).unwrap(), corrupt_marker);
    assert_skill_unchanged(&skill_dir, &before);

    let mut wrong_version_marker: Value = serde_json::from_slice(&valid_marker).unwrap();
    wrong_version_marker["version"] = json!("999.999.999");
    let wrong_version_marker = serde_json::to_vec_pretty(&wrong_version_marker).unwrap();
    fs::write(&marker_path, &wrong_version_marker).unwrap();
    isolated_ctx(&temp, &binary, &roots)
        .args(["setup", "--no-daemon", "--progress", "plain"])
        .assert()
        .success();
    assert_eq!(fs::read(&marker_path).unwrap(), wrong_version_marker);
    assert_skill_unchanged(&skill_dir, &before);
}

#[test]
fn automatic_refresh_failure_does_not_fail_the_requested_command() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "managed_skill_refresh_failure_isolation");
    let roots = IsolatedAgentRoots::new(&temp);
    let skill_dir = temp.path().join(".agents/skills/ctx");
    let before = write_stale_managed_skill(&skill_dir);
    fs::create_dir(skill_dir.join(".SKILL.md.ctx-agent-integrations.lock")).unwrap();

    isolated_ctx(&temp, &binary, &roots)
        .args(["setup", "--no-daemon", "--progress", "plain"])
        .assert()
        .success();

    assert_skill_unchanged(&skill_dir, &before);
}

#[test]
fn observational_status_and_refresh_off_search_leave_owned_skill_unchanged() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "managed_skill_refresh_observational");
    let roots = IsolatedAgentRoots::new(&temp);
    let skill_dir = temp.path().join(".agents/skills/ctx");
    let before = write_stale_managed_skill(&skill_dir);

    isolated_ctx(&temp, &binary, &roots)
        .args(["status", "--format=json"])
        .assert()
        .success();
    assert_skill_unchanged(&skill_dir, &before);

    isolated_ctx(&temp, &binary, &roots)
        .args(["search", "needle", "--refresh", "off", "--format=json"])
        .assert()
        .failure();
    assert_skill_unchanged(&skill_dir, &before);
}
