use super::*;

#[test]
fn flat_publication_uses_its_random_read_chunk_policy() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    super::super::super::locking::prepare_private_root(&root)?;
    let reference = write_flat_segment(&root, 7, Vec::new(), Vec::new())?;
    let (segment, pinned) = open_segment(&root, &reference)?;
    assert_eq!(segment.chunk_bytes(), FLAT_CHUNK_BYTES);
    pinned.verify_identity()?;
    Ok(())
}
