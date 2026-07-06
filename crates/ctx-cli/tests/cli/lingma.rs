#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn lingma_cli_default_source_imports_home_local_db() {
    let temp = tempdir();
    let query = "lingma-default-import-oracle";
    install_default_lingma_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "lingma")
        .unwrap_or_else(|| panic!("missing Lingma source in {sources:#}"));
    assert_eq!(source["source_format"], "lingma_sqlite");
    assert_eq!(source["status"], "available");
    assert_eq!(source["importable"], true);

    let imported = json_output(ctx(&temp).args(["import", "--provider", "lingma", "--json"]));
    assert_eq!(imported["sources"][0]["provider"], "lingma");
    assert_eq!(imported["sources"][0]["source_format"], "lingma_sqlite");
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args(["search", query, "--provider", "lingma", "--json"]));
    assert_search_provider_oracle(&search, "lingma", query, 1, "message");

    let alias_search =
        json_output(ctx(&temp).args(["search", query, "--provider", "qoder-cn", "--json"]));
    assert_search_provider_oracle(&alias_search, "lingma", query, 1, "message");

    let second = json_output(ctx(&temp).args(["import", "--provider", "lingma", "--json"]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

pub(crate) fn install_default_lingma_fixture(temp: &TempDir, query: &str) {
    let target = temp
        .path()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    write_lingma_sqlite_fixture(&target, query);
}

pub(crate) fn write_native_lingma_fixture(temp: &TempDir, query: &str) -> String {
    let db = temp.path().join("native-lingma/local.db");
    write_lingma_sqlite_fixture(&db, query);
    db.to_str().unwrap().to_owned()
}

pub(crate) fn write_lingma_sqlite_fixture(path: &Path, query: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE chat_record (
            session_id TEXT NOT NULL,
            request_id TEXT,
            chat_prompt TEXT,
            summary TEXT,
            error_result TEXT,
            gmt_create INTEGER,
            extra TEXT
        );
        "#,
    )
    .unwrap();
    conn.execute(
        r#"
        INSERT INTO chat_record
            (session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            "lingma-cli-session",
            "lingma-cli-request",
            query,
            "Lingma CLI assistant summary import ok",
            "{}",
            1_783_166_400_000_i64,
            json!({"model": "lingma-cli-fixture"}).to_string(),
        ],
    )
    .unwrap();
}
