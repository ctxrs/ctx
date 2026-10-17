use super::*;
use ctx_history_provider_runtime::set_before_jsonl_terminal_physical_revalidation_hook;
use std::{
    io::{Seek, SeekFrom},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

const TITLE_SLOT_BYTES: usize = 256;

fn title_slot(title: &str) -> Vec<u8> {
    let mut value = json!({
        "type": "title",
        "v": 1,
        "title": title,
        "source": "auto",
        "updatedAt": "2026-10-17T00:00:00.000Z",
        "pad": "",
    });
    let padding = TITLE_SLOT_BYTES - 1 - serde_json::to_vec(&value).unwrap().len();
    value["pad"] = json!(" ".repeat(padding));
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    assert_eq!(bytes.len(), TITLE_SLOT_BYTES);
    bytes
}

fn overwrite_title(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn title_update_at_publication(root: &Path, transcript: &Path) -> Arc<AtomicBool> {
    let transcript = transcript.to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&fired);
    set_before_jsonl_terminal_physical_revalidation_hook(root.to_path_buf(), move || {
        overwrite_title(&transcript, &title_slot("updated title 日本語"));
        observed.store(true, Ordering::SeqCst);
    });
    fired
}

#[test]
fn pi_omp_title_update_during_cold_publication_preserves_the_message() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    write_omp_pi_session(&transcript, "initial title", "original body");
    overwrite_title(&transcript, &title_slot("initial title"));
    let fired = title_update_at_publication(temp.path(), &transcript);

    let index = index_temp.path().join("index");
    let publication =
        refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
            .unwrap();

    assert!(fired.load(Ordering::SeqCst));
    assert!(publication.failed_routes.is_empty());
    let records = all_indexed_records(&index);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("original body")
    );
}

#[test]
fn pi_omp_title_only_changes_keep_existing_event_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    write_omp_pi_session(&transcript, "initial title", "original body");
    overwrite_title(&transcript, &title_slot("initial title"));
    let index = index_temp.path().join("index");
    refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options()).unwrap();
    let before = all_indexed_records(&index);
    overwrite_title(&transcript, &title_slot("between imports"));
    let fired = title_update_at_publication(temp.path(), &transcript);

    let publication =
        refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
            .unwrap();

    assert!(fired.load(Ordering::SeqCst));
    assert!(publication.failed_routes.is_empty());
    assert_eq!(all_indexed_records(&index), before);
}

#[test]
fn pi_omp_append_with_a_title_update_preserves_prior_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    write_omp_pi_session(&transcript, "initial title", "original body");
    overwrite_title(&transcript, &title_slot("initial title"));
    let index = index_temp.path().join("index");
    refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options()).unwrap();
    let before = all_indexed_records(&index);
    append_custom_record(
        &transcript,
        &json!({
            "type": "message",
            "id": "second-message",
            "timestamp": "2026-08-20T15:12:22.000Z",
            "message": {"role": "assistant", "content": "appended answer"},
        }),
    );
    overwrite_title(&transcript, &title_slot("between imports"));
    let fired = title_update_at_publication(temp.path(), &transcript);

    let publication =
        refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
            .unwrap();

    assert!(fired.load(Ordering::SeqCst));
    assert!(publication.failed_routes.is_empty());
    let records = all_indexed_records(&index);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0], before[0]);
    assert_eq!(
        records[1].content.normalized_body.as_deref(),
        Some("appended answer")
    );
}

#[test]
fn pi_omp_incomplete_message_completion_with_title_update_preserves_prior_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    write_omp_pi_session(&transcript, "initial title", "original body");
    overwrite_title(&transcript, &title_slot("initial title"));
    let pending = serde_json::to_vec(&json!({
        "type": "message",
        "id": "completed-message",
        "timestamp": "2026-08-20T15:12:22.000Z",
        "message": {"role": "assistant", "content": "completed answer"},
    }))
    .unwrap();
    let split = pending.len() / 2;
    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(&pending[..split])
        .unwrap();
    let index = index_temp.path().join("index");
    refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options()).unwrap();
    let before = all_indexed_records(&index);
    assert_eq!(before.len(), 1);
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    file.write_all(&pending[split..]).unwrap();
    file.write_all(b"\n").unwrap();
    overwrite_title(&transcript, &title_slot("completed title"));
    let fired = title_update_at_publication(temp.path(), &transcript);
    let publication =
        refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
            .unwrap();
    assert!(fired.load(Ordering::SeqCst));
    assert!(publication.failed_routes.is_empty());
    let records = all_indexed_records(&index);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0], before[0]);
    assert_eq!(
        records[1].content.normalized_body.as_deref(),
        Some("completed answer")
    );
}

#[test]
fn pi_omp_mutations_outside_a_valid_title_slot_cannot_publish() {
    for mutation in ["body", "session", "invalid-title", "slot-length"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let index_temp = crate::test_support_paths::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        write_omp_pi_session(&transcript, "initial title", "original body");
        overwrite_title(&transcript, &title_slot("initial title"));
        let index = index_temp.path().join("index");
        let original =
            refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
                .unwrap();
        let before = all_indexed_records(&index);
        let mut bytes = fs::read(&transcript).unwrap();
        bytes[..TITLE_SLOT_BYTES].copy_from_slice(&title_slot("updated title"));
        match mutation {
            "body" | "session" => {
                let (needle, replacement) = if mutation == "body" {
                    (b"original body".as_slice(), b"modified body".as_slice())
                } else {
                    (
                        b"omp-title-slot-session".as_slice(),
                        b"bad-title-slot-session".as_slice(),
                    )
                };
                let offset = bytes
                    .windows(needle.len())
                    .position(|part| part == needle)
                    .unwrap();
                bytes[offset..offset + needle.len()].copy_from_slice(replacement);
            }
            "invalid-title" => {
                let offset = bytes[..TITLE_SLOT_BYTES]
                    .windows(5)
                    .position(|part| part == b"\"v\":1")
                    .unwrap();
                bytes[offset + 4] = b'2';
            }
            "slot-length" => bytes.insert(TITLE_SLOT_BYTES - 3, b' '),
            _ => unreachable!(),
        }
        let fired = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&fired);
        set_before_jsonl_terminal_physical_revalidation_hook(
            temp.path().to_path_buf(),
            move || {
                fs::write(&transcript, bytes).unwrap();
                observed.store(true, Ordering::SeqCst);
            },
        );

        let publication =
            refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
                .unwrap();

        assert!(fired.load(Ordering::SeqCst), "{mutation}");
        assert_carried_route_failure(
            &publication,
            &original.commit.generation_id,
            SourceBackedSourceFailureClass::SourceChanged,
        );
        assert_eq!(all_indexed_records(&index), before, "{mutation}");
    }
}

fn nonmutable_title_slot(kind: &str, title: &str) -> Vec<u8> {
    let mut bytes = title_slot(title);
    match kind {
        "short" | "crlf" => {
            let offset = bytes.iter().position(|byte| *byte == b' ').unwrap();
            bytes.remove(offset);
            if kind == "crlf" {
                bytes.insert(bytes.len() - 1, b'\r');
            }
        }
        "long" => bytes.insert(bytes.len() - 2, b' '),
        "not-first" => bytes.insert(0, b'\n'),
        _ => unreachable!(),
    }
    bytes
}

#[test]
fn pi_omp_noncanonical_title_framing_retains_raw_authentication() {
    for kind in ["short", "long", "crlf", "not-first"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let index_temp = crate::test_support_paths::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        write_omp_pi_session(&transcript, "initial title", "original body");
        let initial = fs::read(&transcript).unwrap();
        let suffix = &initial[TITLE_SLOT_BYTES..];
        let mut initial = nonmutable_title_slot(kind, "initial title");
        initial.extend_from_slice(suffix);
        fs::write(&transcript, initial).unwrap();
        let index = index_temp.path().join("index");
        let original =
            refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
                .unwrap();
        let before = all_indexed_records(&index);
        let replacement = nonmutable_title_slot(kind, "updated title");
        let fired = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&fired);
        set_before_jsonl_terminal_physical_revalidation_hook(
            temp.path().to_path_buf(),
            move || {
                overwrite_title(&transcript, &replacement);
                observed.store(true, Ordering::SeqCst);
            },
        );

        let publication =
            refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options())
                .unwrap();

        assert!(fired.load(Ordering::SeqCst), "{kind}");
        assert_carried_route_failure(
            &publication,
            &original.commit.generation_id,
            SourceBackedSourceFailureClass::SourceChanged,
        );
        assert_eq!(all_indexed_records(&index), before, "{kind}");
    }
}

#[test]
fn pi_omp_title_metadata_does_not_authorize_file_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let index_temp = crate::test_support_paths::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    write_omp_pi_session(&transcript, "initial title", "original body");
    overwrite_title(&transcript, &title_slot("initial title"));
    let index = index_temp.path().join("index");
    refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options()).unwrap();
    let before = all_indexed_records(&index);
    let moved = index_temp.path().join("moved.jsonl");
    let bytes = fs::read(&transcript).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&fired);
    set_before_jsonl_terminal_physical_revalidation_hook(temp.path().to_path_buf(), move || {
        fs::rename(&transcript, moved).unwrap();
        fs::write(&transcript, bytes).unwrap();
        observed.store(true, Ordering::SeqCst);
    });

    let publication =
        refresh_source_backed_generation(&index, &pi_registry(temp.path()), writer_options());

    assert!(fired.load(Ordering::SeqCst));
    assert!(publication
        .as_ref()
        .map_or(true, |receipt| !receipt.failed_routes.is_empty()));
    assert_eq!(all_indexed_records(&index), before);
}
