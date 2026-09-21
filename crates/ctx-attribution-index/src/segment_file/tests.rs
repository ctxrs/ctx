use super::*;
use std::fs;

const GENERATION: [u8; 32] = [0x31; 32];
const ROLE: u32 = 7;

fn write(path: &Path, bytes: &[u8]) -> Result<(), SegmentFileError> {
    let mut writer = SegmentWriter::create(path, GENERATION, ROLE, SEGMENT_CHUNK_BYTES)?;
    writer.write_all(bytes)?;
    writer.finish()
}

#[test]
fn plaintext_round_trip_crosses_block_boundaries_with_bounded_reads()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    for blocks in [4, 256] {
        let path = temp.path().join(blocks.to_string());
        let bytes: Vec<_> = (0..blocks * SEGMENT_CHUNK_BYTES as usize)
            .map(|index| (index % 251) as u8)
            .collect();
        write(&path, &bytes)?;
        let mut file = SegmentFile::open(&path, GENERATION, ROLE)?;
        assert_eq!(file.chunk_reads(), 0);
        let offset = SEGMENT_CHUNK_BYTES as usize - 5;
        assert_eq!(
            file.read_range(offset as u64, 15)?,
            bytes[offset..offset + 15]
        );
        assert_eq!(
            file.chunk_reads(),
            2,
            "unrelated file growth must not increase query work"
        );
        assert!(file.cached_plaintext_bytes() <= SEGMENT_CHUNK_BYTES as usize);
        assert!(file.read_range(u64::MAX, 1).is_err());
        assert!(file.read_range(0, MAX_RANGE_BYTES + 1).is_err());
        let raw = fs::read(&path)?;
        assert_eq!(&raw[96..116], &bytes[..20], "payload is ordinary plaintext");
    }
    Ok(())
}

#[test]
fn consumed_block_changes_fail_but_unread_corruption_is_not_scanned()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("blocks");
    write(&path, &vec![b'a'; 4 * SEGMENT_CHUNK_BYTES as usize])?;
    let mut raw = fs::OpenOptions::new().write(true).open(&path)?;
    raw.seek(SeekFrom::Start(
        SEGMENT_HEADER_BYTES + 3 * (u64::from(SEGMENT_CHUNK_BYTES) + 32),
    ))?;
    raw.write_all(b"b")?;
    let mut file = SegmentFile::open(&path, GENERATION, ROLE)?;
    assert_eq!(file.read_range(10, 10)?, b"aaaaaaaaaa");
    assert_eq!(file.chunk_reads(), 1);
    assert!(matches!(
        file.read_range(3 * u64::from(SEGMENT_CHUNK_BYTES), 1),
        Err(SegmentFileError::Corrupt("block checksum"))
    ));
    Ok(())
}

#[test]
fn header_identity_length_and_interrupted_writes_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("segment");
    write(&path, b"ordinary derived facts")?;
    assert!(SegmentFile::open(&path, [0x32; 32], ROLE).is_err());
    assert!(SegmentFile::open(&path, GENERATION, ROLE + 1).is_err());
    assert!(
        SegmentWriter::create(
            &temp.path().join("wrong-size"),
            GENERATION,
            ROLE,
            1024 * 1024
        )
        .is_err()
    );
    let mut raw = fs::read(&path)?;
    raw[56] ^= 1;
    fs::write(&path, &raw)?;
    assert!(SegmentFile::open(&path, GENERATION, ROLE).is_err());
    raw.pop();
    fs::write(&path, &raw)?;
    assert!(SegmentFile::open(&path, GENERATION, ROLE).is_err());
    let interrupted = temp.path().join("interrupted");
    let mut writer = SegmentWriter::create(&interrupted, GENERATION, ROLE, SEGMENT_CHUNK_BYTES)?;
    writer.write_all(b"never finished")?;
    drop(writer);
    assert!(SegmentFile::open(&interrupted, GENERATION, ROLE).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn open_descriptor_survives_path_replacement_and_unlink() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("segment");
    write(&path, b"original")?;
    let mut pin = SegmentFile::open(&path, GENERATION, ROLE)?;
    let replacement = temp.path().join("replacement");
    write(&replacement, b"replaced")?;
    fs::rename(&replacement, &path)?;
    assert_eq!(pin.read_all()?, b"original");
    assert_eq!(
        SegmentFile::open(&path, GENERATION, ROLE)?.read_all()?,
        b"replaced"
    );
    fs::remove_file(&path)?;
    pin.clear_chunk_cache();
    assert_eq!(pin.read_all()?, b"original");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_and_hardlink_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let original = temp.path().join("segment");
    write(&original, b"facts")?;
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&original, &link)?;
    assert!(SegmentFile::open(&link, GENERATION, ROLE).is_err());
    fs::remove_file(&link)?;
    fs::hard_link(&original, &link)?;
    assert!(SegmentFile::open(&link, GENERATION, ROLE).is_err());
    Ok(())
}
