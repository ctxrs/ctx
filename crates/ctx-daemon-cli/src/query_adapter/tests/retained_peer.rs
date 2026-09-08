use super::*;

#[test]
fn semantic_supersession_preserves_the_requested_reader_pair_policy() -> Result<()> {
    for retain_peer in [false, true] {
        let temp = semantic_tempdir()?;
        let root = ctx_history_refresh::source_backed_index_root(temp.path());
        let (first, _) = semantic_index_revision_at(&root, 1, true)?;
        let first_id = first.generation_id().to_owned();
        let (second, _) = semantic_index_revision_at(&root, 2, true)?;
        let second_id = second.generation_id().to_owned();
        drop(second);
        let initial = PinnedSourceBackedGeneration::from_index(first);
        let pin = if retain_peer {
            wait_for_daemon_semantic_generation_with_retained_peer(
                temp.path(),
                initial,
                Duration::ZERO,
            )?
        } else {
            wait_for_daemon_semantic_generation(temp.path(), initial, Duration::ZERO)?
        };
        assert_eq!(pin.generation_id(), second_id);

        let (_third, _) = semantic_index_revision_at(&root, 3, true)?;
        let mut index = pin.into_index();
        let peer = index.take_retained_generation_peer_for_reader()?;
        if retain_peer {
            assert_eq!(peer.unwrap().generation_id(), first_id);
        } else {
            assert!(peer.is_none());
            assert!(VerifiedIndex::open_pinned_generation(&root, &first_id).is_err());
        }
    }
    Ok(())
}
