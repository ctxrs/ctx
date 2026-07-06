#[allow(unused_imports)]
use super::*;

pub(crate) fn install_default_kilo_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_kilo_fixture(temp, query));
    let target = temp.path().join(".local/share/kilo");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("kilo.db")).unwrap();
}

pub(crate) fn write_native_kilo_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-kilo.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key,
            project_id text not null,
            parent_id text,
            slug text not null,
            directory text not null,
            title text not null,
            version text not null,
            model text,
            agent text,
            cost real not null default 0,
            tokens_input integer not null default 0,
            tokens_output integer not null default 0,
            tokens_reasoning integer not null default 0,
            tokens_cache_read integer not null default 0,
            tokens_cache_write integer not null default 0,
            time_created integer not null,
            time_updated integer not null
        );
        create table session_message (
            id text primary key,
            session_id text not null,
            type text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table message (
            id text primary key,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table part (
            id text primary key,
            message_id text not null,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table todo (
            session_id text not null,
            content text not null,
            status text not null,
            priority text not null,
            position integer not null,
            time_created integer not null,
            time_updated integer not null
        );
        create table permission (
            project_id text primary key,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
            id, project_id, parent_id, slug, directory, title, version, model, agent,
            time_created, time_updated
        ) values (?1, 'project-1', null, 'native', '/workspace', 'native', '0.8.0',
            '{\"id\":\"kilo-auto/free\",\"providerID\":\"kilo\"}', 'build',
            1782259200000, 1782259200000)",
        ["kilo-cli-native"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
        [
            "kilo-cli-native-user",
            "kilo-cli-native",
            &format!(r#"{{"time":{{"created":1782259200000}},"text":"{query}"}}"#),
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}
