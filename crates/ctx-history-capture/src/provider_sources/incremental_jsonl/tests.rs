mod tests {
    use std::{
        cell::Cell,
        fs,
        io::{Seek, SeekFrom, Write},
    };

    use tempfile::tempdir;

    use super::*;
    use crate::{
        common::io::{install_secure_open_test_hook, SecureOpenTestPhase},
        MAX_PROVIDER_JSONL_LINE_BYTES,
    };

    fn ready(path: &Path, mode: ProviderJsonlOpenMode) -> ProviderJsonlReader {
        match open_provider_jsonl(path, mode).unwrap() {
            ProviderJsonlOpenDecision::Ready(reader) => reader,
            ProviderJsonlOpenDecision::ReplacementRequired(reason) => {
                panic!("unexpected replacement decision: {reason}")
            }
        }
    }

    fn read_all(reader: &mut ProviderJsonlReader) -> Vec<(Vec<u8>, ProviderJsonlRecordRead)> {
        let mut records = Vec::new();
        let mut line = Vec::new();
        loop {
            let read = reader.read_record(&mut line).unwrap();
            if read == ProviderJsonlRecordRead::Eof {
                break;
            }
            records.push((line.clone(), read));
        }
        records
    }

    fn initial_checkpoint(path: &Path) -> ProviderJsonlAppendCheckpoint {
        let mut reader = ready(path, ProviderJsonlOpenMode::WholeReplacement);
        read_all(&mut reader);
        reader.safe_checkpoint().unwrap().unwrap()
    }

    #[test]
    fn equal_length_observation_is_never_append_admitted() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\ntwo\n").unwrap();
        let checkpoint = initial_checkpoint(&path);

        assert!(provider_jsonl_checkpoint_matches_file(&path, &checkpoint).unwrap());

        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint.clone())).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::EqualLengthObservation
            )
        ));

        fs::write(&path, b"uno\ndos\n").unwrap();
        assert!(!provider_jsonl_checkpoint_matches_file(&path, &checkpoint).unwrap());
    }

    #[test]
    fn valid_append_advances_the_safe_boundary() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let checkpoint = initial_checkpoint(&path);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"two\n")
            .unwrap();

        let mut reader = ready(&path, ProviderJsonlOpenMode::Append(checkpoint));
        let records = read_all(&mut reader);
        assert_eq!(records[0].0, b"two\n");
        assert!(matches!(
            records[0].1,
            ProviderJsonlRecordRead::Record {
                line_number: 2,
                newline_terminated: true,
                ..
            }
        ));
        let checkpoint = reader.safe_checkpoint().unwrap().unwrap();
        assert_eq!(checkpoint.committed_offset, 8);
        assert_eq!(checkpoint.complete_line_count, 2);
    }

    #[test]
    fn partial_final_line_is_deferred_until_completed() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let checkpoint = initial_checkpoint(&path);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"tw")
            .unwrap();

        let mut reader = ready(&path, ProviderJsonlOpenMode::Append(checkpoint.clone()));
        assert!(matches!(
            reader.read_record(&mut Vec::new()).unwrap(),
            ProviderJsonlRecordRead::DeferredPartial {
                newline_terminated: false,
                ..
            }
        ));
        assert_eq!(
            reader.safe_checkpoint().unwrap().unwrap(),
            checkpoint,
            "a partial record must not advance the persisted boundary"
        );

        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"o\n")
            .unwrap();
        let mut reader = ready(&path, ProviderJsonlOpenMode::Append(checkpoint));
        let records = read_all(&mut reader);
        assert_eq!(records[0].0, b"two\n");
    }

    #[test]
    fn append_capable_initial_import_materializes_only_the_checkpointed_prefix() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\npart").unwrap();

        let mut reader = ProviderJsonlReader::open_append_capable_replacement(&path).unwrap();
        let mut line = Vec::new();
        assert!(matches!(
            reader.read_record(&mut line).unwrap(),
            ProviderJsonlRecordRead::Record {
                bytes: 4,
                line_number: 1,
                newline_terminated: true,
            }
        ));
        assert_eq!(line, b"one\n");
        assert!(matches!(
            reader.read_record(&mut line).unwrap(),
            ProviderJsonlRecordRead::DeferredPartial {
                bytes: 4,
                line_number: 2,
                newline_terminated: false,
                ..
            }
        ));
        let checkpoint = reader.safe_checkpoint().unwrap().unwrap();
        assert_eq!(checkpoint.committed_offset, 4);
        assert_eq!(checkpoint.complete_line_count, 1);

        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"ial\n")
            .unwrap();
        let mut incremental = ready(&path, ProviderJsonlOpenMode::Append(checkpoint));
        let records = read_all(&mut incremental);
        assert_eq!(records[0].0, b"partial\n");
    }

    #[test]
    fn append_capable_reseed_defers_a_new_partial_tail() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\ntwo\nthree").unwrap();

        let mut reader = ProviderJsonlReader::open_append_capable_replacement(&path).unwrap();
        let records = read_all(&mut reader);
        assert_eq!(records[0].0, b"one\n");
        assert_eq!(records[1].0, b"two\n");
        assert!(matches!(
            records[2].1,
            ProviderJsonlRecordRead::DeferredPartial {
                newline_terminated: false,
                ..
            }
        ));
        let checkpoint = reader.safe_checkpoint().unwrap().unwrap();
        assert_eq!(checkpoint.committed_offset, 8);
        assert_eq!(checkpoint.complete_line_count, 2);
    }

    #[test]
    fn ordinary_replacement_may_materialize_an_eof_final_record() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("replacement.jsonl");
        fs::write(&path, b"one").unwrap();

        let mut reader = ProviderJsonlReader::open_replacement(&path).unwrap();
        assert!(matches!(
            reader.read_record(&mut Vec::new()).unwrap(),
            ProviderJsonlRecordRead::Record {
                newline_terminated: false,
                ..
            }
        ));
    }

    #[test]
    fn shrink_and_prefix_mutation_require_replacement() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\ntwo\n").unwrap();
        let checkpoint = initial_checkpoint(&path);

        fs::write(&path, b"one\n").unwrap();
        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint.clone())).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::FileShrank
            )
        ));

        fs::write(&path, b"ONE\ntwo\nthree\n").unwrap();
        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint)).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::HeadHashMismatch
            )
        ));
    }

    #[test]
    fn large_equal_length_middle_rewrite_requires_replacement_without_sparse_hashing() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("large-equal-length.jsonl");
        let line = format!("{}\n", "x".repeat(127));
        fs::write(&path, line.repeat(256)).unwrap();
        let checkpoint = initial_checkpoint(&path);
        assert!(checkpoint.committed_offset > 20 * 1024);

        let middle = checkpoint.committed_offset / 2;
        assert!(middle > SENTINEL_BYTES);
        assert!(middle < checkpoint.committed_offset - SENTINEL_BYTES);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(middle)).unwrap();
        file.write_all(b"Z").unwrap();
        file.flush().unwrap();

        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint)).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::EqualLengthObservation
            )
        ));
    }

    #[test]
    fn large_prefix_boundary_mutation_followed_by_append_is_detected() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("large-boundary.jsonl");
        let line = format!("{}\n", "x".repeat(127));
        fs::write(&path, line.repeat(1024)).unwrap();
        let checkpoint = initial_checkpoint(&path);
        assert!(checkpoint.committed_offset > SENTINEL_BYTES * 3);

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(
            checkpoint.committed_offset - SENTINEL_BYTES / 2,
        ))
        .unwrap();
        file.write_all(b"Z").unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(b"tail\n").unwrap();
        file.flush().unwrap();

        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint)).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::BoundaryHashMismatch
            )
        ));
    }

    #[test]
    fn rewrite_plus_append_is_explicitly_outside_the_provider_append_only_fact() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("out-of-contract-middle.jsonl");
        let line = format!("{}\n", "x".repeat(127));
        fs::write(&path, line.repeat(1024)).unwrap();
        let checkpoint = initial_checkpoint(&path);
        let middle = SENTINEL_BYTES * 2;
        assert!(middle + SENTINEL_BYTES < checkpoint.committed_offset - SENTINEL_BYTES);

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(middle)).unwrap();
        file.write_all(b"Z").unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(b"tail\n").unwrap();
        file.flush().unwrap();

        // This is deliberately not a mutation-detector test. There is no exact
        // portable O(delta) observation for an arbitrary old-prefix edit
        // followed by growth. Ready is sound only under the provider mutation
        // contract: the provider, rather than an external editor, owns these
        // bytes and only appends. Callers without that fact must never select
        // ProviderJsonlOpenMode::Append.
        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint)).unwrap(),
            ProviderJsonlOpenDecision::Ready(_)
        ));
    }

    #[test]
    fn identity_change_requires_replacement() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let old = temp.path().join("old.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let checkpoint = initial_checkpoint(&path);
        fs::rename(&path, old).unwrap();
        fs::write(&path, b"one\ntwo\n").unwrap();

        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint)).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::StableIdentityChanged
            )
        ));
    }

    #[cfg(unix)]
    #[test]
    fn secure_open_reads_the_opened_parent_when_the_path_is_swapped_at_a_barrier() {
        use std::{
            os::unix::fs::symlink,
            sync::{Arc, Barrier},
            thread,
        };

        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        let held = temp.path().join("held-source");
        let outside = temp.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&outside).unwrap();
        let path = source.join("history.jsonl");
        fs::write(&path, b"inside\n").unwrap();
        fs::write(outside.join("history.jsonl"), b"outside\n").unwrap();

        let opened = Arc::new(Barrier::new(2));
        let swapped = Arc::new(Barrier::new(2));
        let attacker_opened = Arc::clone(&opened);
        let attacker_swapped = Arc::clone(&swapped);
        let attacker_source = source.clone();
        let attacker_held = held.clone();
        let attacker_outside = outside.clone();
        let attacker = thread::spawn(move || {
            attacker_opened.wait();
            fs::rename(&attacker_source, &attacker_held).unwrap();
            symlink(&attacker_outside, &attacker_source).unwrap();
            attacker_swapped.wait();
        });
        let hook_opened = Arc::clone(&opened);
        let hook_swapped = Arc::clone(&swapped);
        let source_for_hook = source.clone();
        let _hook = install_secure_open_test_hook(move |opened_path, phase| {
            if phase == SecureOpenTestPhase::AfterParentOpen && opened_path == source_for_hook {
                hook_opened.wait();
                hook_swapped.wait();
            }
        });

        let mut reader = ready(&path, ProviderJsonlOpenMode::WholeReplacement);
        assert_eq!(read_all(&mut reader)[0].0, b"inside\n");
        attacker.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secure_open_reads_the_opened_file_when_the_final_path_is_swapped_at_a_barrier() {
        use std::{
            os::unix::fs::symlink,
            sync::{Arc, Barrier},
            thread,
        };

        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let held = temp.path().join("held.jsonl");
        let outside = temp.path().join("outside.jsonl");
        fs::write(&path, b"inside\n").unwrap();
        fs::write(&outside, b"outside\n").unwrap();

        let opened = Arc::new(Barrier::new(2));
        let swapped = Arc::new(Barrier::new(2));
        let attacker_opened = Arc::clone(&opened);
        let attacker_swapped = Arc::clone(&swapped);
        let attacker_path = path.clone();
        let attacker_held = held.clone();
        let attacker_outside = outside.clone();
        let attacker = thread::spawn(move || {
            attacker_opened.wait();
            fs::rename(&attacker_path, &attacker_held).unwrap();
            symlink(&attacker_outside, &attacker_path).unwrap();
            attacker_swapped.wait();
        });
        let hook_opened = Arc::clone(&opened);
        let hook_swapped = Arc::clone(&swapped);
        let path_for_hook = path.clone();
        let _hook = install_secure_open_test_hook(move |opened_path, phase| {
            if phase == SecureOpenTestPhase::AfterFinalOpen && opened_path == path_for_hook {
                hook_opened.wait();
                hook_swapped.wait();
            }
        });

        let mut reader = ready(&path, ProviderJsonlOpenMode::WholeReplacement);
        assert_eq!(read_all(&mut reader)[0].0, b"inside\n");
        attacker.join().unwrap();
    }

    #[test]
    fn oversized_complete_line_is_bounded_and_committable() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut bytes = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1];
        bytes.push(b'\n');
        fs::write(&path, bytes).unwrap();

        let mut reader = ready(&path, ProviderJsonlOpenMode::WholeReplacement);
        let mut line = Vec::new();
        assert!(matches!(
            reader.read_record(&mut line).unwrap(),
            ProviderJsonlRecordRead::Oversized {
                newline_terminated: true,
                ..
            }
        ));
        assert!(line.is_empty());
        assert_eq!(reader.complete_line_count(), 1);
    }

    #[test]
    fn malformed_appended_record_is_returned_as_one_bounded_complete_record() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"{\"ok\":true}\n").unwrap();
        let checkpoint = initial_checkpoint(&path);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{malformed}\n")
            .unwrap();

        let mut reader = ready(&path, ProviderJsonlOpenMode::Append(checkpoint));
        let mut line = Vec::new();
        assert!(matches!(
            reader.read_record(&mut line).unwrap(),
            ProviderJsonlRecordRead::Record {
                line_number: 2,
                newline_terminated: true,
                ..
            }
        ));
        assert!(serde_json::from_slice::<serde_json::Value>(&line).is_err());
        assert_eq!(
            reader
                .safe_checkpoint()
                .unwrap()
                .unwrap()
                .complete_line_count,
            2
        );
    }

    #[test]
    fn semantic_checkpoint_can_stop_before_the_physical_complete_boundary() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\ntwo\nthree\n").unwrap();

        let mut reader = ProviderJsonlReader::open_append_capable_replacement(&path).unwrap();
        read_all(&mut reader);
        let checkpoint = reader.checkpoint_at(8, 2).unwrap().unwrap();

        assert_eq!(checkpoint.committed_offset, 8);
        assert_eq!(checkpoint.complete_line_count, 2);
    }

    #[test]
    fn checkpoint_construction_rejects_a_post_read_shrink() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\ntwo\n").unwrap();

        let mut reader = ProviderJsonlReader::open_append_capable_replacement(&path).unwrap();
        read_all(&mut reader);
        fs::write(&path, b"one\n").unwrap();

        assert_eq!(
            reader.safe_checkpoint().unwrap(),
            Err(ProviderJsonlReplacementReason::FileShrank)
        );
    }

    #[test]
    fn unavailable_identity_and_legacy_checkpoint_require_replacement() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let checkpoint = initial_checkpoint(&path);

        assert!(matches!(
            open_provider_jsonl_with_identity(
                &path,
                ProviderJsonlOpenMode::Append(checkpoint),
                |_, _| None
            )
            .unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::StableIdentityUnavailable
            )
        ));
        assert!(matches!(
            open_provider_jsonl(&path, ProviderJsonlOpenMode::LegacyCodexPrefixCheckpoint).unwrap(),
            ProviderJsonlOpenDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::LegacyCodexPrefixCheckpoint
            )
        ));
    }

    #[test]
    fn post_read_identity_unavailability_is_distinct_from_identity_change() {
        let expected = ProviderFileStableIdentity::Unix {
            device: 7,
            inode: 11,
        };
        assert_eq!(
            validate_stable_identity(&expected, None),
            Err(ProviderJsonlReplacementReason::StableIdentityUnavailable)
        );
        assert_eq!(
            validate_stable_identity(
                &expected,
                Some(ProviderFileStableIdentity::Unix {
                    device: 7,
                    inode: 12,
                }),
            ),
            Err(ProviderJsonlReplacementReason::StableIdentityChanged)
        );
        assert_eq!(
            validate_stable_identity(&expected, Some(expected.clone())),
            Ok(())
        );

        let temp = tempdir().unwrap();
        let path = temp.path().join("post-read-identity.jsonl");
        fs::write(&path, b"one\n").unwrap();
        let mut reader = ProviderJsonlReader::open_append_capable_replacement(&path).unwrap();
        read_all(&mut reader);
        let expected = reader.stable_identity.clone().unwrap();
        let committed_offset = reader.committed_offset();
        let complete_line_count = reader.complete_line_count();
        let identity_reads = Cell::new(0usize);
        assert_eq!(
            reader
                .checkpoint_at_with_identity(committed_offset, complete_line_count, |_, _| {
                    let read = identity_reads.get();
                    identity_reads.set(read + 1);
                    (read == 0).then(|| expected.clone())
                },)
                .unwrap(),
            Err(ProviderJsonlReplacementReason::StableIdentityUnavailable)
        );
        assert_eq!(identity_reads.get(), 2);
    }

    #[test]
    fn resume_state_persistence_is_tagged_strict_and_version_validated() {
        let state = ProviderJsonlResumeState::ClaudeProjects(ClaudeProjectsJsonlResumeState::new(
            "claude-session".to_owned(),
            "2026-07-14T12:00:00Z".parse().unwrap(),
        ));
        let encoded = state.encode_persisted_json().unwrap();
        assert_eq!(
            encoded,
            r#"{"provider":"claude_projects","state":{"version":1,"authoritative_session_id":"claude-session","authoritative_started_at":"2026-07-14T12:00:00Z"}}"#
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(&encoded),
            Ok(state)
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"tabnine_cli","state":{"version":2,"authoritative_session_id":"tabnine-session","authoritative_started_at":"2026-07-14T12:00:00Z"}}"#
            ),
            Ok(ProviderJsonlResumeState::TabnineCli(
                TabnineJsonlResumeState::new(
                    "tabnine-session".to_owned(),
                    "2026-07-14T12:00:00Z".parse().unwrap(),
                )
            ))
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"tabnine_cli","state":{"version":1,"authoritative_session_id":"tabnine-session"}}"#
            ),
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"tabnine_cli","state":{"version":1,"authoritative_session_id":"tabnine-session","authoritative_started_at":"2026-07-14T12:00:00Z"}}"#
            ),
            Err(ProviderJsonlReplacementReason::UnsupportedAdapterResumeStateVersion)
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"claude_projects","state":{"version":2,"authoritative_session_id":"claude-session","authoritative_started_at":"2026-07-14T12:00:00Z"}}"#
            ),
            Err(ProviderJsonlReplacementReason::UnsupportedAdapterResumeStateVersion)
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"tabnine_cli","state":{"version":2,"authoritative_session_id":"tabnine-session","authoritative_started_at":"2026-07-14T12:00:00Z","future_field":true}}"#
            ),
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"tabnine_cli","state":{"version":2,"authoritative_session_id":" ","authoritative_started_at":"2026-07-14T12:00:00Z"}}"#
            ),
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        );

        let codex = ProviderJsonlResumeState::CodexSession(CodexSessionJsonlResumeState::new(
            vec![CodexToolCallResumeContext {
                call_id: "call-1".to_owned(),
                tool_name: "exec_command".to_owned(),
                command_preview: Some("cargo test".to_owned()),
                arguments_preview: None,
            }],
            3,
        ));
        let encoded = codex.encode_persisted_json().unwrap();
        assert_eq!(
            encoded,
            r#"{"provider":"codex_session","state":{"version":1,"pending_tool_calls":[{"call_id":"call-1","tool_name":"exec_command","command_preview":"cargo test","arguments_preview":null}],"dropped_tool_calls":3}}"#
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(&encoded),
            Ok(codex)
        );
        assert_eq!(
            ProviderJsonlResumeState::decode_persisted_json(
                r#"{"provider":"codex_session","state":{"version":1,"pending_tool_calls":[{"call_id":"duplicate","tool_name":"a","command_preview":null,"arguments_preview":null},{"call_id":"duplicate","tool_name":"b","command_preview":null,"arguments_preview":null}],"dropped_tool_calls":0}}"#
            ),
            Err(ProviderJsonlReplacementReason::AdapterResumeStateIncompatible)
        );
    }

    #[test]
    fn windows_stable_identity_storage_key_preserves_the_full_file_id() {
        let identity = ProviderFileStableIdentity::Windows {
            volume_serial: 18_446_744_073_709_551_557,
            file_id: [
                0x00, 0x01, 0x02, 0x03, 0x10, 0x20, 0x30, 0x40, 0x7f, 0x80, 0x90, 0xa0, 0xb0, 0xc0,
                0xfe, 0xff,
            ],
        };
        let key = identity.to_storage_key();
        assert_eq!(
            key,
            "windows:18446744073709551557:00010203102030407f8090a0b0c0feff"
        );
        assert_eq!(
            ProviderFileStableIdentity::from_storage_key(&key),
            Some(identity)
        );
        assert_eq!(
            ProviderFileStableIdentity::from_storage_key(
                "windows:18446744073709551557:00010203102030407F8090A0B0C0FEFF"
            )
            .unwrap()
            .to_storage_key(),
            key
        );
        assert_eq!(
            ProviderFileStableIdentity::from_storage_key("windows:42:4294967297"),
            None
        );
    }

    #[test]
    fn crash_replay_restarts_from_the_last_persisted_boundary() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"one\ntwo\n").unwrap();
        let mut initial = ready(&path, ProviderJsonlOpenMode::WholeReplacement);
        let mut line = Vec::new();
        initial.read_record(&mut line).unwrap();
        let persisted = initial.safe_checkpoint().unwrap().unwrap();

        initial.read_record(&mut line).unwrap();
        drop(initial);
        let mut replay = ready(&path, ProviderJsonlOpenMode::Append(persisted));
        assert_eq!(read_all(&mut replay)[0].0, b"two\n");
    }
}
