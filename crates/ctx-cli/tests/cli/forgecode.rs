#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn sources_discovers_forgecode_env_and_legacy_db() {
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
            .args(["sources", "--json"]),
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

    let sources = json_output(ctx(&legacy_temp).args(["sources", "--json"]));
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

pub(crate) fn install_default_forgecode_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_forgecode_fixture(temp, query));
    let target = temp.path().join(".forge");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join(".forge.db")).unwrap();
}

pub(crate) fn write_native_forgecode_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-forgecode.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE conversations (
            conversation_id TEXT PRIMARY KEY NOT NULL,
            title TEXT,
            workspace_id BIGINT NOT NULL,
            context TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP,
            metrics TEXT
        );",
    )
    .unwrap();
    let context = json!({
        "conversation_id": "forgecode-cli-native",
        "initiator": "forgecode",
        "messages": [
            {
                "message": {
                    "text": {
                        "role": "User",
                        "content": query
                    }
                }
            },
            {
                "message": {
                    "text": {
                        "role": "Assistant",
                        "content": "forgecode native import ok",
                        "tool_calls": [{
                            "name": "write",
                            "call_id": "call-forgecode-cli",
                            "arguments": {
                                "path": "src/forgecode_cli_native.rs",
                                "content": "proof"
                            }
                        }],
                        "model": "forge/test-model"
                    }
                }
            },
            {
                "message": {
                    "tool": {
                        "name": "write",
                        "call_id": "call-forgecode-cli",
                        "output": {
                            "is_error": false,
                            "values": [{"text": "wrote src/forgecode_cli_native.rs"}]
                        }
                    }
                }
            }
        ],
        "tools": [{"name": "write", "input_schema": {"type": "object"}}],
        "tool_choice": {"Call": "write"},
        "stream": true
    });
    let metrics = json!({
        "started_at": "2026-06-24T12:00:01Z",
        "files_changed": {
            "src/forgecode_cli_native.rs": {
                "lines_added": 1,
                "lines_removed": 0,
                "tool": "write",
                "content_hash": "cli-fixture"
            }
        },
        "files_accessed": ["src/forgecode_cli_input.rs"]
    });
    conn.execute(
        "INSERT INTO conversations (
            conversation_id, title, workspace_id, context, created_at, updated_at, metrics
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "forgecode-cli-native",
            "ForgeCode CLI native",
            42_i64,
            serde_json::to_string(&context).unwrap(),
            "2026-06-24 12:00:00",
            "2026-06-24 12:00:03",
            serde_json::to_string(&metrics).unwrap()
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}
