use super::*;

fn exact_tombstone_ceiling() -> TombstoneQueryWork {
    TombstoneQueryWork {
        pages: MAX_GRAPH_TOMBSTONE_PAGES,
        page_bytes: MAX_GRAPH_TOMBSTONE_PAGE_BYTES,
        checked_chunks: MAX_GRAPH_TOMBSTONE_AUTHENTICATED_CHUNKS,
        checked_bytes: MAX_GRAPH_TOMBSTONE_AUTHENTICATED_BYTES,
    }
}

#[test]
fn aggregate_tombstone_work_accepts_the_exact_release_ceiling() {
    let mut work = QueryWork::new();
    assert!(
        work.charge_candidates(usize::try_from(MAX_GRAPH_QUERY_CANDIDATES).unwrap())
            .is_ok()
    );
    assert!(
        work.charge_tombstones(
            usize::try_from(MAX_GRAPH_TOMBSTONE_PROBES).unwrap(),
            exact_tombstone_ceiling(),
        )
        .is_ok()
    );
}

#[test]
fn aggregate_tombstone_work_rejects_every_one_over_before_membership() {
    let one_over = [
        TombstoneQueryWork {
            pages: MAX_GRAPH_TOMBSTONE_PAGES + 1,
            ..TombstoneQueryWork::default()
        },
        TombstoneQueryWork {
            page_bytes: MAX_GRAPH_TOMBSTONE_PAGE_BYTES + 1,
            ..TombstoneQueryWork::default()
        },
        TombstoneQueryWork {
            checked_chunks: MAX_GRAPH_TOMBSTONE_AUTHENTICATED_CHUNKS + 1,
            ..TombstoneQueryWork::default()
        },
        TombstoneQueryWork {
            checked_bytes: MAX_GRAPH_TOMBSTONE_AUTHENTICATED_BYTES + 1,
            ..TombstoneQueryWork::default()
        },
    ];
    for planned in one_over {
        assert!(QueryWork::new().charge_tombstones(0, planned).is_err());
    }
    assert!(
        QueryWork::new()
            .charge_tombstones(
                usize::try_from(MAX_GRAPH_TOMBSTONE_PROBES + 1).unwrap(),
                TombstoneQueryWork::default(),
            )
            .is_err()
    );
    assert!(
        QueryWork::new()
            .charge_candidates(usize::try_from(MAX_GRAPH_QUERY_CANDIDATES + 1).unwrap())
            .is_err()
    );
}
