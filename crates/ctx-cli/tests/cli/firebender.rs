#[allow(unused_imports)]
use super::*;

pub(crate) fn write_native_firebender_fixture(temp: &TempDir, query: &str) -> String {
    let project = temp.path().join("native-firebender/project");
    let db = project
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::copy(
        provider_history_fixture("firebender/v1/.idea/firebender/chat_history.db"),
        &db,
    )
    .unwrap();
    let conn = Connection::open(&db).unwrap();
    let messages = sqlite_column_text(
        &conn,
        "SELECT messages_json FROM chat_sessions WHERE id = 'firebender-fixture-session'",
    )
    .replace("firebender fixture oracle prompt", query);
    conn.execute(
        "UPDATE chat_sessions SET messages_json = ?1 WHERE id = 'firebender-fixture-session'",
        params![messages],
    )
    .unwrap();
    project.to_str().unwrap().to_owned()
}
