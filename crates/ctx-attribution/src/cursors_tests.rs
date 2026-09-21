use super::*;
use crate::protocol::{LineRange, ResourceKind, ResourceRef};
use crate::query::BlameFactPosition;

fn backend() -> (tempfile::TempDir, SegmentGraph) {
    crate::test_support::empty_graph()
}

fn repository() -> ResourceRef {
    ResourceRef {
        id: "resource_0123456789abcdef0123456789abcdef".to_owned(),
        kind: ResourceKind::Repository,
        display: "forge:github.com/ctxrs/ctx".to_owned(),
    }
}

#[test]
fn maximum_sha256_file_cursor_is_below_the_frozen_cap() {
    let position = FileBlamePosition {
        head_oid: "f".repeat(64),
        requested_start: 1,
        requested_end: Some(u32::MAX),
        window_start: u32::MAX - 499,
        window_end: u32::MAX,
        next_line: u32::MAX,
    };
    let mut payload = vec![0_u8; 3 + 8 + TARGET_DIGEST_BYTES];
    encode_file(&mut payload, &position).unwrap_or_else(|_| unreachable!());
    payload.extend_from_slice(&[0_u8; GENERATION_ID_BYTES]);
    assert!(URL_SAFE_NO_PAD.encode(payload).len() <= MAX_BLAME_CURSOR_BYTES);
}

#[test]
fn repository_and_real_stable_ids_do_not_expand_fact_cursor() {
    let target = ResolvedBlameTarget::Commit {
        commit: ResourceRef {
            id: format!("resource_{}", "a".repeat(32)),
            kind: ResourceKind::Commit,
            display: "f".repeat(64),
        },
        repository: ResourceRef {
            id: format!("resource_{}", "b".repeat(32)),
            kind: ResourceKind::Repository,
            display: "forge.example/".to_owned() + &"nested/".repeat(100),
        },
    };
    let mut payload = vec![0_u8; 3 + 8];
    payload.extend_from_slice(&target_fingerprint(&target).unwrap_or_else(|_| unreachable!()));
    payload.extend_from_slice(
        &compact_fact_id(&format!("fact_{}", "c".repeat(32))).unwrap_or_else(|_| unreachable!()),
    );
    payload.extend_from_slice(&[0_u8; GENERATION_ID_BYTES]);
    assert!(URL_SAFE_NO_PAD.encode(payload).len() <= MAX_BLAME_CURSOR_BYTES);
}

#[test]
fn resolved_file_range_changes_target_identity() {
    let resource = repository();
    let first = ResolvedBlameTarget::File {
        path: "src/lib.rs".to_owned(),
        repository: resource.clone(),
        requested_lines: Some(LineRange { start: 1, end: 1 }),
    };
    let second = ResolvedBlameTarget::File {
        path: "src/lib.rs".to_owned(),
        repository: resource,
        requested_lines: Some(LineRange { start: 2, end: 2 }),
    };
    assert_ne!(
        target_fingerprint(&first).unwrap_or_else(|_| unreachable!()),
        target_fingerprint(&second).unwrap_or_else(|_| unreachable!())
    );
}

#[test]
fn cursor_rejects_cross_target_replay_and_changed_graph_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, backend) = backend();
    let first = ResolvedBlameTarget::File {
        path: "src/lib.rs".to_owned(),
        repository: repository(),
        requested_lines: Some(LineRange {
            start: 1,
            end: 1_000,
        }),
    };
    let second = ResolvedBlameTarget::File {
        path: "src/other.rs".to_owned(),
        repository: repository(),
        requested_lines: Some(LineRange {
            start: 1,
            end: 1_000,
        }),
    };
    let position = BlamePosition::File(FileBlamePosition {
        head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        requested_start: 1,
        requested_end: Some(1_000),
        window_start: 501,
        window_end: 1_000,
        next_line: 501,
    });
    let cursor = backend.encode_blame_cursor(&first, 7, &position)?;
    assert!(matches!(
        backend.decode_blame_cursor(&cursor, &first, 7),
        Ok(decoded) if decoded == position
    ));
    assert!(matches!(
        backend.decode_blame_cursor(&cursor, &second, 7),
        Err(SegmentGraphError::InvalidCursor)
    ));
    assert!(matches!(
        backend.decode_blame_cursor(&cursor, &first, 8),
        Err(SegmentGraphError::StaleCursor)
    ));
    let (_other_directory, other_backend) = crate::test_support::empty_graph();
    assert_eq!(backend.graph_generation(), other_backend.graph_generation());
    assert_ne!(backend.generation_id(), other_backend.generation_id());
    assert!(matches!(
        other_backend.decode_blame_cursor(&cursor, &first, 7),
        Err(SegmentGraphError::StaleCursor)
    ));
    Ok(())
}

#[test]
fn fact_cursor_fails_closed_when_the_bound_fact_is_absent() -> Result<(), Box<dyn std::error::Error>>
{
    let (_directory, backend) = backend();
    let target = ResolvedBlameTarget::Commit {
        commit: ResourceRef {
            id: "resource_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            kind: ResourceKind::Commit,
            display: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        },
        repository: repository(),
    };
    let position = BlamePosition::Commit(BlameFactPosition {
        class: 0,
        rank: 5,
        occurred_at_ms: Some(1),
        resource_id: "resource_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        fact_id: "fact_cccccccccccccccccccccccccccccccc".to_owned(),
    });
    let cursor = backend.encode_blame_cursor(&target, 9, &position)?;
    assert!(matches!(
        backend.decode_blame_cursor(&cursor, &target, 9),
        Err(SegmentGraphError::InvalidCursor)
    ));
    Ok(())
}
