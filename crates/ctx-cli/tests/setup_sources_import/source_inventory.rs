use super::{ctx, support::*};

#[test]
fn setup_skips_empty_codex_session_tree() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert!(
        setup["catalog"]["cataloged_sessions"].is_null(),
        "{setup:#}"
    );
    assert!(setup["catalog"]["source_files"].is_null(), "{setup:#}");
    assert_eq!(
        setup["refresh_request"]["receipt"]["current"]["current_source_count"], 0,
        "{setup:#}",
    );

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    let codex_sessions = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "codex" && source["source_format"] == "codex_session_jsonl_tree"
        })
        .unwrap();
    assert_eq!(codex_sessions["status"], "empty");
    assert_eq!(codex_sessions["importable"], false);
}

#[test]
fn sources_default_hides_unsupported_missing_locations() {
    let temp = tempdir();

    let sources = json_output(ctx(&temp).args(["--color=always", "sources", "--format=json"]));
    assert_eq!(sources["scope"], "default");
    assert!(sources["hidden_missing_sources"].as_u64().unwrap() > 0);
    let visible = sources["sources"].as_array().unwrap();
    assert!(visible.iter().any(|source| source["provider"] == "codex"));
    assert!(visible.iter().any(|source| source["provider"] == "claude"));
    assert!(visible.iter().any(|source| source["provider"] == "cursor"));
    assert!(visible.iter().any(|source| source["provider"] == "pi"));
    assert!(visible
        .iter()
        .any(|source| source["provider"] == "opencode"));
    assert!(visible
        .iter()
        .any(|source| source["provider"] == "copilot_cli"));

    let text = ctx(&temp)
        .arg("sources")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("missing provider locations are hidden"));
    assert!(text.contains("ctx sources --all"));

    let all_sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    assert_eq!(all_sources["scope"], "all");
    assert_eq!(all_sources["hidden_missing_sources"], 0);
    let all = all_sources["sources"].as_array().unwrap();
    assert!(all.len() > visible.len());
}

#[test]
fn request_scoped_import_paths_do_not_enter_the_automatic_source_inventory() {
    let temp = tempdir();
    let explicit_root = temp.path().join("external-codex-history");
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &explicit_root,
    );
    let explicit_path = explicit_root.to_str().unwrap();

    let before = json_output(ctx(&temp).args(["sources", "--provider", "codex", "--format=json"]));
    assert!(before["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| source["path"] != explicit_path));

    for _ in 0..2 {
        json_output(ctx(&temp).args([
            "import",
            "--provider",
            "codex",
            "--path",
            explicit_path,
            "--format=json",
            "--progress",
            "none",
        ]));
    }

    let after = json_output(ctx(&temp).args(["sources", "--provider", "codex", "--format=json"]));
    let matches = after["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| source["path"] == explicit_path)
        .collect::<Vec<_>>();
    assert!(matches.is_empty(), "request overlay leaked into {after:#}");

    let human = ctx(&temp)
        .args(["sources", "--provider", "codex"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    let concise_path = Path::new("~").join("external-codex-history");
    assert!(
        !human.contains(&concise_path.display().to_string()),
        "{human}"
    );
    assert!(!human.contains(explicit_path), "{human}");
    assert!(!human.contains(temp.path().to_str().unwrap()), "{human}");
}

#[test]
fn sources_provider_filter_rejects_unsupported_providers() {
    let temp = tempdir();

    ctx(&temp)
        .args([
            "sources",
            "--provider",
            "not-a-real-provider",
            "--format=json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown provider"));
}

#[test]
fn sources_json_reports_typed_discovery_issues_additively() {
    let temp = tempdir();

    let sources = json_output(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", "relative-account")
            .args(["sources", "--provider", "claude", "--format=json"]),
    );

    assert_eq!(sources["schema_version"], 1);
    assert_eq!(sources["scope"], "all");
    assert_eq!(sources["hidden_missing_sources"], 0);
    assert!(sources["sources"].as_array().unwrap().is_empty());
    assert_eq!(sources["issues_truncated"], false);
    let issues = sources["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["provider"], "claude");
    assert_eq!(issues[0]["path"], "relative-account");
    assert_eq!(issues[0]["code"], "selector_unreconstructible");
    assert_eq!(
        issues[0]["message"],
        "the selected provider root cannot be reconstructed safely; use an exact --path"
    );
    assert_eq!(issues[0]["message_truncated"], false);
}

#[test]
fn sources_lists_supported_personal_agent_provider_defaults() {
    let temp = tempdir();
    install_default_hermes_fixture(&temp, "hermes-sources-oracle");
    install_default_kilo_fixture(&temp, "kilo-sources-oracle");
    install_default_kiro_fixture(&temp, "kiro-sources-oracle");
    install_default_astrbot_fixture(&temp, "astrbot-sources-oracle");
    install_default_continue_fixture(&temp, "continue-sources-oracle");
    install_default_forgecode_fixture(&temp, "forgecode-sources-oracle");
    install_default_mistral_vibe_fixture(&temp, "mistral-vibe-sources-oracle");
    install_default_mux_fixture(&temp, "mux-sources-oracle");
    install_default_lingma_fixture(&temp, "lingma-sources-oracle");
    install_default_qoder_fixture(&temp, "qoder-sources-oracle");
    install_default_auggie_fixture(&temp, "auggie-sources-oracle");
    install_default_junie_fixture(&temp, "junie-sources-oracle");
    install_default_warp_fixture(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    for (provider, source_format, import_support, native_import) in [
        ("hermes", "hermes_state_sqlite", "native", true),
        ("kilo", "kilo_sqlite", "native", true),
        ("kiro_cli", "kiro_cli_sqlite", "native", true),
        ("astrbot", "astrbot_data_v4_sqlite", "native", true),
        ("continue", "continue_cli_sessions_json", "native", true),
        ("forgecode", "forgecode_sqlite", "native", true),
        (
            "mistral_vibe",
            "mistral_vibe_session_jsonl_tree",
            "native",
            true,
        ),
        ("mux", "mux_session_jsonl_tree", "native", true),
        ("lingma", "lingma_sqlite", "native", true),
        ("qoder", "qoder_transcript_jsonl_tree", "native", true),
        ("auggie", "auggie_session_json", "native", true),
        ("junie", "junie_session_events_jsonl_tree", "native", true),
        ("warp", "warp_sqlite", "native", true),
    ] {
        let source = sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| {
                source["provider"] == provider && source["source_format"] == source_format
            })
            .unwrap_or_else(|| panic!("missing {provider} source in {sources:#}"));
        assert_eq!(source["status"], "available");
        assert_eq!(source["import_support"], import_support);
        assert_eq!(source["native_import"], native_import);
        assert_eq!(source["importable"], true);
        assert!(source["unsupported_reason"].is_null());
    }
}

#[test]
fn sources_uses_exact_cwd_and_ignores_shelley_db_env_override() {
    let temp = tempdir();
    let fixture = PathBuf::from(write_native_shelley_fixture(&temp, "shelley-cwd-oracle"));
    let cwd_db = temp.path().join("shelley.db");
    fs::copy(fixture, &cwd_db).unwrap();
    let ignored_env_db = temp.path().join("custom-shelley.db");
    fs::write(&ignored_env_db, b"sqlite fixture marker").unwrap();

    let sources = json_output(
        ctx(&temp)
            .current_dir(temp.path())
            .env("SHELLEY_DB", &ignored_env_db)
            .args(["sources", "--format=json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "shelley" && source["path"] == cwd_db.to_str().unwrap()
        })
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert!(sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| source["path"] != ignored_env_db.to_str().unwrap()));
}

#[test]
fn sources_falls_back_to_userprofile_when_home_unset() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );

    let sources = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .args(["sources", "--format=json"]),
    );
    let codex = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "codex" && source["status"] == "available")
        .unwrap_or_else(|| panic!("missing codex source in {sources:#}"));
    assert!(Path::new(codex["path"].as_str().unwrap()).starts_with(temp.path()));
}

#[test]
fn sources_discovers_forgecode_env_and_legacy_db() {
    let temp = tempdir();
    let fixture = PathBuf::from(write_native_forgecode_fixture(
        &temp,
        "forgecode-env-sources-oracle",
    ));
    let env_root = temp.path().join("custom-forge");
    fs::create_dir_all(&env_root).unwrap();
    let env_db = env_root.join(".forge.db");
    fs::copy(&fixture, &env_db).unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("FORGE_CONFIG", &env_root)
            .args(["sources", "--format=json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "forgecode")
        .unwrap_or_else(|| panic!("missing ForgeCode env source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "forgecode_sqlite");
    assert_eq!(source["path"], env_db.to_str().unwrap());

    let legacy_temp = tempdir();
    let legacy_fixture = PathBuf::from(write_native_forgecode_fixture(
        &legacy_temp,
        "forgecode-legacy-sources-oracle",
    ));
    let legacy_root = legacy_temp.path().join("forge");
    fs::create_dir_all(&legacy_root).unwrap();
    let legacy_db = legacy_root.join(".forge.db");
    fs::copy(legacy_fixture, &legacy_db).unwrap();

    let sources = json_output(ctx(&legacy_temp).args(["sources", "--format=json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "forgecode")
        .unwrap_or_else(|| panic!("missing ForgeCode legacy source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "forgecode_sqlite");
    assert_eq!(source["path"], legacy_db.to_str().unwrap());
}
#[test]
fn nanoclaw_exact_cwd_sources_are_native_and_auto_imported() {
    let temp = tempdir();
    let query = "nanoclaw-exact-cwd-auto-refresh-oracle";
    let project = PathBuf::from(write_native_nanoclaw_fixture(&temp, query));

    let mut sources_command = ctx(&temp);
    sources_command.current_dir(&project);
    let sources = json_output(sources_command.args(["sources", "--format=json"]));
    let nanoclaw = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "nanoclaw")
        .unwrap();
    assert_eq!(nanoclaw["status"], "available");
    assert_eq!(nanoclaw["import_support"], "native");
    assert_eq!(nanoclaw["native_import"], true);
    assert_eq!(nanoclaw["importable"], true);
    assert!(nanoclaw["unsupported_reason"].is_null());

    let mut setup_command = ctx(&temp);
    setup_command.current_dir(&project);
    let imported_generation =
        json_output(setup_command.args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(
        imported_generation["refresh_request"]["receipt"]["current"]["current_source_count"], 1,
        "{imported_generation:#}"
    );
    assert_eq!(
        imported_generation["refresh_request"]["receipt"]["current"]["current_indexed_documents"],
        2,
        "{imported_generation:#}"
    );

    ctx(&temp).args(["daemon", "enable"]).assert().success();
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        project.to_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));

    let search_after_import = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search_after_import, "nanoclaw", query, 2, "message");
}

#[test]
fn nanoclaw_service_registration_import_all_indexes_without_exact_cwd() {
    let temp = tempdir();
    let query = "nanoclaw-service-registration-import-all-oracle";
    let fixture = PathBuf::from(write_native_nanoclaw_fixture(&temp, query));
    let project = temp.path().join("registered NanoClaw checkout");
    fs::rename(&fixture, &project).unwrap();
    write_nanoclaw_systemd_registration(&temp, &project);
    let unrelated_cwd = temp.path().join("unrelated-cwd");
    fs::create_dir_all(&unrelated_cwd).unwrap();

    let mut sources_command = ctx(&temp);
    sources_command.current_dir(&unrelated_cwd);
    let sources =
        json_output(sources_command.args(["sources", "--provider", "nanoclaw", "--format=json"]));
    let nanoclaw = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "nanoclaw")
        .unwrap_or_else(|| panic!("missing registered NanoClaw source in {sources:#}"));
    assert_eq!(nanoclaw["path"], project.to_str().unwrap());
    assert_eq!(nanoclaw["status"], "available");
    assert_eq!(nanoclaw["import_support"], "native");

    let mut import_command = ctx(&temp);
    import_command.current_dir(&unrelated_cwd);
    let imported = json_output(import_command.args([
        "import",
        "--all",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        imported["totals"]["current_source_count"], 1,
        "{imported:#}"
    );
    assert_eq!(
        imported["totals"]["current_indexed_documents"], 2,
        "{imported:#}"
    );

    // Import waits for the published receipt. Stop the isolated daemon before
    // searching so no background refresh can race the refresh-off assertion.
    json_output(ctx(&temp).args(["daemon", "disable", "--format=json"]));

    let mut search_command = ctx(&temp);
    search_command.current_dir(&unrelated_cwd);
    let search = json_output(search_command.args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "nanoclaw", query, 1, "message");
}

fn write_nanoclaw_systemd_registration(temp: &TempDir, project: &Path) {
    let slug = nanoclaw_test_sha1_slug(project.to_string_lossy().as_bytes());
    let unit = temp
        .path()
        .join(".config/systemd/user")
        .join(format!("nanoclaw-v2-{slug}.service"));
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(
        unit,
        format!(
            "[Unit]\nDescription=NanoClaw Personal Assistant\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/usr/bin/node {}/dist/index.js\nWorkingDirectory={}\nRestart=always\nRestartSec=5\nKillMode=process\nEnvironment=HOME={}\nEnvironment=PATH=/usr/local/bin:/usr/bin:/bin:{}/.local/bin\nStandardOutput=append:{}/logs/nanoclaw.log\nStandardError=append:{}/logs/nanoclaw.error.log\n\n[Install]\nWantedBy=default.target",
            project.display(),
            project.display(),
            temp.path().display(),
            temp.path().display(),
            project.display(),
            project.display(),
        ),
    )
    .unwrap();
}

fn nanoclaw_test_sha1_slug(input: &[u8]) -> String {
    let mut message = input.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut hash = [
        0x6745_2301_u32,
        0xefcd_ab89_u32,
        0x98ba_dcfe_u32,
        0x1032_5476_u32,
        0xc3d2_e1f0_u32,
    ];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = hash;
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        hash[0] = hash[0].wrapping_add(a);
        hash[1] = hash[1].wrapping_add(b);
        hash[2] = hash[2].wrapping_add(c);
        hash[3] = hash[3].wrapping_add(d);
        hash[4] = hash[4].wrapping_add(e);
    }

    let bytes = hash[0].to_be_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}
