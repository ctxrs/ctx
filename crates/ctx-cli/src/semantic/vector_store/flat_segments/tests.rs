use super::*;

fn contract() -> FlatModelContract {
    FlatModelContract {
        contract_version: 2,
        model_id: "test/e5".to_owned(),
        model_revision: "revision-1".to_owned(),
        tokenizer: "tokenizer-sha256".to_owned(),
        pooling: "attention-mask-mean".to_owned(),
        dimensions: 4,
        normalization: "l2".to_owned(),
    }
}

fn hash(byte: u8) -> FlatSourceHash {
    FlatSourceHash::from_bytes([byte; 32])
}

fn chunk(index: u32, vector: [f32; 4]) -> FlatChunk {
    FlatChunk {
        chunk_index: index,
        start_char: index * 10,
        end_char: index * 10 + 9,
        vector: vector.to_vec(),
    }
}

fn replacement(
    event_id: Uuid,
    seq: u64,
    hash_byte: u8,
    chunks: Vec<FlatChunk>,
) -> FlatEventReplacement {
    FlatEventReplacement {
        event_id,
        seq,
        source_text_hash: hash(hash_byte),
        chunks,
    }
}

fn visible_chunks(pinned: &PinnedFlatGeneration) -> Vec<(Uuid, u64, u32, Vec<f32>)> {
    pinned
        .scan_segments()
        .iter()
        .flat_map(PinnedScanSegment::chunks)
        .map(|chunk| {
            (
                chunk.event_id,
                chunk.seq,
                chunk.chunk_index,
                chunk.vector.to_vec(),
            )
        })
        .collect()
}

#[test]
fn replacement_tombstone_and_read_only_enumeration_are_exact() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let store = FlatSegmentStore::open(temporary.path(), contract())?;
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    store.publish_replacement_event_chunks(
        &[
            replacement(
                first,
                10,
                1,
                vec![
                    chunk(0, [1.0, 0.0, 0.0, 0.0]),
                    chunk(1, [0.0, 1.0, 0.0, 0.0]),
                ],
            ),
            replacement(second, 20, 2, vec![chunk(0, [0.0, 0.0, 1.0, 0.0])]),
        ],
        &[],
    )?;
    store.publish_replacement_event_chunks(
        &[replacement(
            first,
            30,
            3,
            vec![chunk(7, [0.0, 0.0, 0.0, 1.0])],
        )],
        &[second],
    )?;

    let read_only = FlatSegmentStore::open_read_only(temporary.path(), contract())?;
    let pinned = read_only
        .pin_generation()?
        .ok_or_else(|| FlatStoreError::Corrupt("expected a published generation".to_owned()))?;
    assert_eq!(pinned.generation(), 2);
    assert_eq!(pinned.stats().active_events, 1);
    assert_eq!(pinned.stats().active_chunks, 1);
    assert_eq!(pinned.stats().deleted_events, 1);
    assert_eq!(
        pinned.active_events(),
        &[FlatActiveEvent {
            event_id: first,
            seq: 30,
            source_text_hash: hash(3),
            chunk_count: 1,
        }]
    );
    assert_eq!(
        visible_chunks(&pinned),
        vec![(first, 30, 7, vec![0.0, 0.0, 0.0, 1.0])]
    );
    assert_eq!(
        pinned
            .scan_segments()
            .iter()
            .map(PinnedScanSegment::active_chunk_count)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(pinned.scan_segments()[0].chunk_at(0).is_none());
    let scoring = pinned.scan_segments()[1]
        .scoring_chunks()
        .next()
        .ok_or_else(|| FlatStoreError::Corrupt("expected scoring metadata".to_owned()))?;
    assert_eq!(scoring.ordinal, 0);
    assert_eq!(scoring.event_id, first);
    assert_eq!(scoring.chunk_index, 7);
    let resolved = pinned.scan_segments()[1]
        .chunk_at(scoring.ordinal)
        .ok_or_else(|| FlatStoreError::Corrupt("expected direct metadata lookup".to_owned()))?;
    assert_eq!(resolved.event_id, scoring.event_id);
    assert_eq!(resolved.chunk_index, scoring.chunk_index);
    assert_eq!(resolved.source_text_hash, hash(3));
    let active_vector = pinned
        .scan_segments()
        .iter()
        .flat_map(PinnedScanSegment::chunks)
        .next()
        .ok_or_else(|| FlatStoreError::Corrupt("expected active vector".to_owned()))?
        .vector;
    assert_eq!(active_vector.as_ptr() as usize % VECTOR_ALIGNMENT, 0);
    assert!(matches!(
        read_only.delete_events(&[first]),
        Err(FlatStoreError::ReadOnly)
    ));
    Ok(())
}

#[test]
fn compaction_is_sequential_and_old_pin_remains_readable() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let store = FlatSegmentStore::open(temporary.path(), contract())?;
    // Deliberately publish descending event IDs across generations. A
    // sequential compactor must not need a corpus-wide vector reorder.
    let first = Uuid::from_u128(12);
    let second = Uuid::from_u128(11);
    store.publish_replacement_event_chunks(
        &[replacement(
            first,
            120,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    let old_pin = store
        .pin_generation()?
        .ok_or_else(|| FlatStoreError::Corrupt("expected first generation".to_owned()))?;
    store.publish_replacement_event_chunks(
        &[replacement(
            second,
            110,
            2,
            vec![chunk(0, [0.0, 1.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    let before = visible_chunks(
        &store
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected generation".to_owned()))?,
    );
    let compacted = store.compact()?;
    assert!(compacted.published);
    let current = store
        .pin_generation()?
        .ok_or_else(|| FlatStoreError::Corrupt("expected compacted generation".to_owned()))?;
    assert_eq!(current.scan_segments().len(), 1);
    assert_eq!(visible_chunks(&current), before);
    assert_eq!(
        visible_chunks(&old_pin),
        vec![(first, 120, 0, vec![1.0, 0.0, 0.0, 0.0])]
    );
    Ok(())
}

#[test]
fn restart_removes_only_owned_temporary_and_orphan_files() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let root = temporary.path();
    let store = FlatSegmentStore::open(root, contract())?;
    let event = Uuid::from_u128(21);
    store.publish_replacement_event_chunks(
        &[replacement(
            event,
            210,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    let state_path = root.join("state.sqlite");
    fs::write(&state_path, b"parent-owned")
        .map_err(|source| io_error("write parent state fixture", &state_path, source))?;
    let temporary_segment = segments_directory(root).join(format!("{TEMP_PREFIX}crash"));
    fs::write(&temporary_segment, b"partial")
        .map_err(|source| io_error("write temporary fixture", &temporary_segment, source))?;
    let unknown = segments_directory(root).join("parent-owned.file");
    fs::write(&unknown, b"keep")
        .map_err(|source| io_error("write unknown fixture", &unknown, source))?;

    let reopened = FlatSegmentStore::open(root, contract())?;
    assert_eq!(reopened.recovery_report().removed_temporary_files, 1);
    assert_eq!(
        fs::read(&state_path).map_err(|source| io_error(
            "read parent state fixture",
            &state_path,
            source
        ))?,
        b"parent-owned"
    );
    assert!(unknown.exists());
    assert!(!temporary_segment.exists());
    assert_eq!(reopened.active_stats()?.active_events, 1);
    Ok(())
}

#[test]
fn interrupted_segment_commit_keeps_previous_manifest_active() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let root = temporary.path();
    let store = FlatSegmentStore::open(root, contract())?;
    let first = Uuid::from_u128(25);
    store.publish_replacement_event_chunks(
        &[replacement(
            first,
            250,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )?;

    let orphan = Uuid::from_u128(26);
    let _staged = write_replacement_segment(
        root,
        &contract(),
        2,
        &[replacement(
            orphan,
            260,
            2,
            vec![chunk(0, [0.0, 1.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    sync_directory(&segments_directory(root))?;
    drop(store);

    let reopened = FlatSegmentStore::open(root, contract())?;
    let pinned = reopened
        .pin_generation()?
        .ok_or_else(|| FlatStoreError::Corrupt("expected prior generation".to_owned()))?;
    assert_eq!(pinned.generation(), 1);
    assert_eq!(
        visible_chunks(&pinned),
        vec![(first, 250, 0, vec![1.0, 0.0, 0.0, 0.0])]
    );
    assert_eq!(reopened.recovery_report().removed_orphan_segments, 3);
    Ok(())
}

#[test]
fn corruption_is_rejected_and_model_change_atomically_resets_empty() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let root = temporary.path();
    let store = FlatSegmentStore::open(root, contract())?;
    let event = Uuid::from_u128(31);
    store.publish_replacement_event_chunks(
        &[replacement(
            event,
            310,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    let selected = select_manifest(root, &contract())?
        .ok_or_else(|| FlatStoreError::Corrupt("expected manifest fixture".to_owned()))?;
    let vector_path =
        segments_directory(root).join(&selected.envelope.manifest.segments[0].vectors.file);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&vector_path)
        .map_err(|source| io_error("open corrupt vector fixture", &vector_path, source))?;
    file.seek(SeekFrom::Start(HEADER_BYTES_U64))
        .map_err(|source| io_error("seek corrupt vector fixture", &vector_path, source))?;
    file.write_all(&[1])
        .map_err(|source| io_error("write corrupt vector fixture", &vector_path, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync corrupt vector fixture", &vector_path, source))?;
    drop(file);
    assert!(matches!(
        FlatSegmentStore::open_read_only(root, contract()),
        Err(FlatStoreError::Corrupt(_))
    ));

    let other = tempfile::tempdir()
        .map_err(|source| io_error("create second test directory", Path::new("."), source))?;
    let store = FlatSegmentStore::open(other.path(), contract())?;
    store.publish_replacement_event_chunks(
        &[replacement(
            event,
            310,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    let old_pin = store
        .pin_generation()?
        .ok_or_else(|| FlatStoreError::Corrupt("expected old model generation".to_owned()))?;
    let mut changed = contract();
    changed.model_revision = "revision-2".to_owned();
    assert!(matches!(
        FlatSegmentStore::open_read_only(other.path(), changed.clone()),
        Err(FlatStoreError::Incompatible(_))
    ));
    let _interrupted_reset =
        write_empty_base_segment(other.path(), &changed, old_pin.generation() + 1)?;
    sync_directory(&segments_directory(other.path()))?;
    let reset = FlatSegmentStore::open(other.path(), changed.clone())?;
    assert!(reset.recovery_report().model_contract_reset);
    assert!(reset.recovery_report().removed_orphan_segments >= 3);
    assert_eq!(reset.active_stats()?.active_events, 0);
    let reset_pin = reset
        .pin_generation()?
        .ok_or_else(|| FlatStoreError::Corrupt("expected empty reset generation".to_owned()))?;
    assert_eq!(reset_pin.generation(), 2);
    assert!(visible_chunks(&reset_pin).is_empty());
    assert_eq!(
        visible_chunks(&old_pin),
        vec![(event, 310, 0, vec![1.0, 0.0, 0.0, 0.0])]
    );
    assert!(matches!(
        store.pin_generation(),
        Err(FlatStoreError::Incompatible(_))
    ));
    let read_only = FlatSegmentStore::open_read_only(other.path(), changed)?;
    assert_eq!(read_only.active_stats()?.active_events, 0);
    Ok(())
}

#[test]
fn manifest_checksum_corruption_is_rejected() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let root = temporary.path();
    let store = FlatSegmentStore::open(root, contract())?;
    let event = Uuid::from_u128(35);
    store.publish_replacement_event_chunks(
        &[replacement(
            event,
            350,
            1,
            vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
        )],
        &[],
    )?;
    let selected = select_manifest(root, &contract())?
        .ok_or_else(|| FlatStoreError::Corrupt("expected manifest fixture".to_owned()))?;
    let mut envelope = read_manifest(&selected.path)?;
    envelope.manifest.created_unix_millis = envelope.manifest.created_unix_millis.saturating_add(1);
    let bytes = serde_json::to_vec(&envelope)?;
    fs::write(&selected.path, bytes)
        .map_err(|source| io_error("corrupt manifest fixture", &selected.path, source))?;
    assert!(matches!(
        FlatSegmentStore::open_read_only(root, contract()),
        Err(FlatStoreError::Corrupt(_))
    ));
    Ok(())
}

#[test]
fn invalid_vectors_and_ambiguous_mutations_never_publish() -> FlatResult<()> {
    let temporary = tempfile::tempdir()
        .map_err(|source| io_error("create test directory", Path::new("."), source))?;
    let store = FlatSegmentStore::open(temporary.path(), contract())?;
    let event = Uuid::from_u128(41);
    let invalid = replacement(event, 410, 1, vec![chunk(0, [1.0, 1.0, 0.0, 0.0])]);
    assert!(matches!(
        store.publish_replacement_event_chunks(&[invalid], &[]),
        Err(FlatStoreError::InvalidInput(_))
    ));
    let valid = replacement(event, 410, 1, vec![chunk(0, [1.0, 0.0, 0.0, 0.0])]);
    assert!(matches!(
        store.publish_replacement_event_chunks(&[valid], &[event]),
        Err(FlatStoreError::InvalidInput(_))
    ));
    assert_eq!(store.active_hash()?, None);
    Ok(())
}
