#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn sources_discovers_shelley_db_env_override() {
    let temp = tempdir();
    let db_path = temp.path().join("custom-shelley.db");
    fs::write(&db_path, b"sqlite fixture marker").unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("SHELLEY_DB", &db_path)
            .args(["sources", "--json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "shelley" && source["path"] == db_path.to_str().unwrap()
        })
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#}"));
    assert_eq!(source["source_format"], "shelley_sqlite");
    assert_eq!(source["status"], "available");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["path"], db_path.to_str().unwrap());
}

pub(crate) fn install_default_shelley_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_shelley_fixture(temp, query));
    let target = temp.path().join(".config/shelley");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("shelley.db")).unwrap();
}

pub(crate) fn write_native_shelley_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-shelley.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            conversation_id text primary key,
            slug text,
            user_initiated boolean not null default true,
            created_at datetime not null default current_timestamp,
            updated_at datetime not null default current_timestamp,
            cwd text,
            archived boolean not null default false,
            parent_conversation_id text,
            model text,
            conversation_options text not null default '{}',
            current_generation integer not null default 1,
            agent_working boolean not null default false,
            tags text not null default '[]',
            is_draft boolean not null default false,
            draft text not null default ''
        );
        create table messages (
            message_id text primary key,
            conversation_id text not null,
            sequence_id integer not null,
            type text not null,
            llm_data text,
            user_data text,
            usage_data text,
            created_at datetime not null default current_timestamp,
            display_data text,
            excluded_from_context boolean not null default false,
            generation integer not null default 1,
            llm_api_url text,
            model_name text,
            forked_from_message_id text
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            'shelley-cli-native', 'native shelley', 1, '2026-06-24 12:00:00',
            '2026-06-24 12:00:01', '/workspace', 0, null, 'claude-opus-4-7',
            '{}', 1, 0, '[]', 0, ''
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
        ) values (
            'shelley-cli-native-user', 'shelley-cli-native', 1, 'user', ?1,
            '2026-06-24 12:00:01'
        )",
        [json!({"Content": [{"Type": 2, "Text": query}]}).to_string()],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, llm_data, usage_data,
            created_at, llm_api_url, model_name
        ) values (
            'shelley-cli-native-agent', 'shelley-cli-native', 2, 'agent', ?1, ?2,
            '2026-06-24 12:00:02', 'https://api.anthropic.com/v1/messages',
            'claude-opus-4-7'
        )",
        [
            json!({"Content": [{"Type": 2, "Text": "native Shelley import ok"}]}).to_string(),
            json!({"input_tokens": 12, "output_tokens": 8, "cost_usd": 0.001}).to_string(),
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn append_native_shelley_event(path: &str, query: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
        ) values (
            'shelley-cli-native-user-2', 'shelley-cli-native', 3, 'user', ?1,
            '2026-06-24 12:00:03'
        )",
        [json!({"Content": [{"Type": 2, "Text": query}]}).to_string()],
    )
    .unwrap();
}
