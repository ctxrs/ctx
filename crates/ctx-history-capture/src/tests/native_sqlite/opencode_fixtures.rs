use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::TempDir;

pub(super) fn write_opencode_session_message_without_seq_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("opencode-no-seq.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key, title text not null, directory text not null,
                time_created integer not null, time_updated integer not null
            );
            create table session_message (
                id text primary key, session_id text not null, type text not null,
                time_created integer not null, time_updated integer not null, data text not null
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session values (?1, 'no seq', '/workspace', 1782259200000, 1782259200000)",
        ["opencode-no-seq"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
        [
            "msg-no-seq-user",
            "opencode-no-seq",
            "{\"time\":{\"created\":1782259200000},\"text\":\"first no seq\"}",
        ],
    )
    .unwrap();
    conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 1782259201000, 1782259201000, ?3)",
            [
                "msg-no-seq-assistant",
                "opencode-no-seq",
                "{\"time\":{\"created\":1782259201000},\"text\":\"second no seq\"}",
            ],
        )
        .unwrap();
    path
}

pub(super) fn write_opencode_current_schema_db(temp: &TempDir, with_message: bool) -> PathBuf {
    let path = temp.path().join(if with_message {
        "opencode-current-message.db"
    } else {
        "opencode-current-empty.db"
    });
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
                share_url text,
                summary_additions integer,
                summary_deletions integer,
                summary_files integer,
                summary_diffs text,
                revert text,
                permission text,
                time_created integer not null,
                time_updated integer not null,
                time_compacting integer,
                time_archived integer,
                workspace_id text
            );
            create table session_entry (
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
                type text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );",
    )
    .unwrap();

    if with_message {
        conn.execute(
            "insert into session (
                    id, project_id, parent_id, slug, directory, title, version, permission,
                    time_created, time_updated
                ) values (?1, 'project-1', null, 'current-root', '/workspace', 'current root',
                    '0.8.0', 'default', 1782259200000, 1782259200000)",
            ["current-root"],
        )
        .unwrap();
        conn.execute(
                "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
                [
                    "current-message-1",
                    "current-root",
                    "{\"role\":\"user\",\"time\":{\"created\":1782259200000},\"text\":\"legacy hello\"}",
                ],
            )
            .unwrap();
    }

    path
}

pub(super) fn write_opencode_session_message_metadata_with_legacy_message_db(
    temp: &TempDir,
) -> PathBuf {
    write_opencode_strict_real_content_db(
        temp,
        "opencode-session-message-metadata-legacy.db",
        true,
        false,
        true,
        false,
    )
}

pub(super) fn write_opencode_session_message_malformed_with_legacy_message_db(
    temp: &TempDir,
) -> PathBuf {
    let path = write_opencode_strict_real_content_db(
        temp,
        "opencode-session-message-malformed-legacy.db",
        true,
        false,
        true,
        false,
    );
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update session_message set data = ?1 where id = 'metadata-session-message'",
        ["{\"time\":{\"created\":1782259200000},\"text\":"],
    )
    .unwrap();
    path
}

pub(super) fn write_opencode_session_message_metadata_bad_seq_with_legacy_message_db(
    temp: &TempDir,
) -> PathBuf {
    let path = write_opencode_session_message_metadata_with_legacy_message_db(temp);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update session_message set seq = -1 where id = 'metadata-session-message'",
        [],
    )
    .unwrap();
    path
}

pub(super) fn write_opencode_session_entry_metadata_with_legacy_message_db(
    temp: &TempDir,
) -> PathBuf {
    write_opencode_strict_real_content_db(
        temp,
        "opencode-session-entry-metadata-legacy.db",
        false,
        true,
        true,
        false,
    )
}

pub(super) fn write_opencode_all_metadata_db(temp: &TempDir, name: &str) -> PathBuf {
    write_opencode_strict_real_content_db(temp, name, true, true, false, true)
}

pub(super) fn write_opencode_tool_only_db(temp: &TempDir, name: &str) -> PathBuf {
    let path = write_opencode_strict_real_content_db(temp, name, false, false, false, false);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "insert into session_message values (
                'tool-only-session-message', 'strict-root', 'assistant', 1,
                1782259200000, 1782259200000, ?1
            )",
        ["{\"time\":{\"created\":1782259200000},\"content\":[{\"type\":\"tool\",\"name\":\"bash\",\"input\":{\"command\":\"true\"}}]}"],
    )
    .unwrap();
    path
}

fn write_opencode_strict_real_content_db(
    temp: &TempDir,
    name: &str,
    session_message_metadata: bool,
    session_entry_metadata: bool,
    legacy_real_message: bool,
    legacy_metadata_message: bool,
) -> PathBuf {
    let path = temp.path().join(name);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key,
                title text not null,
                directory text not null,
                time_created integer not null,
                time_updated integer not null
            );
            create table session_message (
                id text primary key,
                session_id text not null,
                type text not null,
                seq integer not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table session_entry (
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
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session values (
                'strict-root', 'strict root', '/workspace', 1782259200000, 1782259200000
            )",
        [],
    )
    .unwrap();
    if session_message_metadata {
        conn.execute(
                "insert into session_message values (
                    'metadata-session-message', 'strict-root', 'model_change', 1,
                    1782259200000, 1782259200000, ?1
                )",
                ["{\"time\":{\"created\":1782259200000},\"provider\":\"openai\",\"model\":\"metadata-only\"}"],
            )
            .unwrap();
    }
    if session_entry_metadata {
        conn.execute(
            "insert into session_entry values (
                    'metadata-session-entry', 'strict-root', 'label',
                    1782259200001, 1782259200001, ?1
                )",
            ["{\"time\":{\"created\":1782259200001},\"label\":\"metadata-only\"}"],
        )
        .unwrap();
    }
    if legacy_real_message {
        conn.execute(
            "insert into message values (
                    'legacy-real-message', 'strict-root', 1782259200002, 1782259200002, ?1
                )",
            ["{\"role\":\"user\",\"time\":{\"created\":1782259200002},\"text\":\"legacy fallback prompt\"}"],
        )
        .unwrap();
    }
    if legacy_metadata_message {
        conn.execute(
            "insert into message values (
                    'legacy-metadata-message', 'strict-root', 1782259200002, 1782259200002, ?1
                )",
            ["{\"type\":\"model_change\",\"time\":{\"created\":1782259200002},\"model\":\"metadata-only-legacy\"}"],
        )
        .unwrap();
    }
    path
}

pub(super) fn write_opencode_future_incomplete_schema_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("opencode-future-incomplete.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key,
                project_id text not null,
                slug text not null,
                directory text not null,
                title text not null,
                version text not null,
                time_created integer not null,
                time_updated integer not null
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
                id, project_id, slug, directory, title, version, time_created, time_updated
            ) values ('future-root', 'project-1', 'future-root', '/workspace', 'future root',
                '0.9.0', 1782259200000, 1782259200000)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into message values ('future-message-1', 'future-root', 1782259200000,
                1782259200000)",
        [],
    )
    .unwrap();
    path
}
