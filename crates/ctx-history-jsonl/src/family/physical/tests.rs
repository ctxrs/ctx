use ctx_history_source_io::SourceIoError;
use sha2::{Digest, Sha256};

use super::*;

#[test]
fn stream_tracks_complete_prefix_tail_and_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("records.jsonl");
    let contents = b"one\ntwo\nincomplete";
    std::fs::write(&path, contents).unwrap();
    let mut stream = JsonlPhysicalStream::open(
        File::open(&path).unwrap(),
        contents.len() as u64,
        0,
        0,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::full_and_complete(
            JsonlResumableSha256::new(),
            JsonlResumableSha256::new(),
        ),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();

    let first = stream.next_record().unwrap().unwrap();
    assert_eq!(stream.record_bytes(first), b"one");
    let after_first = stream.position();
    let second = stream.next_record().unwrap().unwrap();
    assert_eq!(stream.record_bytes(second), b"two");
    stream.restore(after_first).unwrap();
    assert_eq!(
        stream.complete_prefix_eof_hasher().unwrap().digest(),
        <[u8; 32]>::from(Sha256::digest(b"one\n"))
    );
    assert_eq!(stream.next_record().unwrap().unwrap(), second);
    let tail = stream.next_record().unwrap().unwrap();
    assert!(!tail.complete);
    assert_eq!(stream.record_bytes(tail), b"incomplete");
    let after_tail = stream.position();
    assert!(stream.next_record().unwrap().is_none());
    stream.restore(after_tail).unwrap();
    assert!(stream.next_record().unwrap().is_none());
    assert_eq!(stream.complete_prefix_end(), 8);
    assert_eq!(stream.next_physical_ordinal(), 2);
    assert!(!stream.terminal());
    let frontier = stream.complete_prefix_eof_hasher().unwrap();
    assert_eq!(frontier.bytes_hashed(), 8);
    assert_eq!(
        frontier.digest(),
        <[u8; 32]>::from(Sha256::digest(&contents[..8]))
    );
    let digest = stream.digest();
    let complete = digest.complete_hasher().digest();
    let expected_complete: [u8; 32] = Sha256::digest(&contents[..8]).into();
    assert_eq!(complete, expected_complete);
    let full = digest.full_hasher().unwrap().digest();
    let expected_full: [u8; 32] = Sha256::digest(contents).into();
    assert_eq!(full, expected_full);
}

fn checksummed_frame(plaintext: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
    encoder.include_checksum(true).unwrap();
    std::io::Write::write_all(&mut encoder, plaintext).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn zstd_stream_decodes_concatenated_checksummed_frames_and_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl.zstd");
    let first_bytes = checksummed_frame(b"{\"type\":\"session\"}\n");
    let second_bytes =
        checksummed_frame(b"{\"type\":\"user/message\"}\n{\"type\":\"assistant/message\"}\n");
    let contents = [first_bytes.as_slice(), second_bytes.as_slice()].concat();
    std::fs::write(&path, &contents).unwrap();
    let mut stream = JsonlPhysicalStream::open_with_encoding(
        File::open(&path).unwrap(),
        contents.len() as u64,
        0,
        0,
        JsonlPhysicalEncoding::ChecksummedZstdFrames,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::full_and_complete(
            JsonlResumableSha256::new(),
            JsonlResumableSha256::new(),
        ),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();

    let header = stream.next_record().unwrap().unwrap();
    assert_eq!(stream.record_bytes(header), b"{\"type\":\"session\"}\n");
    let after_header = stream.position();
    let batch = stream.next_record().unwrap().unwrap();
    assert_eq!(
        stream.record_bytes(batch),
        b"{\"type\":\"user/message\"}\n{\"type\":\"assistant/message\"}\n"
    );
    stream.restore(after_header).unwrap();
    assert_eq!(stream.next_record().unwrap().unwrap(), batch);
    assert!(stream.next_record().unwrap().is_none());
    assert!(stream.terminal());
    assert_eq!(stream.complete_prefix_end(), contents.len() as u64);
    assert_eq!(stream.next_physical_ordinal(), 2);
    assert_eq!(
        stream.digest().complete_hasher().digest(),
        <[u8; 32]>::from(Sha256::digest(&contents))
    );
}

#[test]
fn zstd_stream_omits_torn_frame_and_rejects_corrupt_or_unchecked_frames() {
    let temp = tempfile::tempdir().unwrap();
    let complete = checksummed_frame(b"{\"type\":\"session\"}\n");
    let mut torn = checksummed_frame(b"{\"type\":\"user/message\"}\n");
    torn.truncate(torn.len().saturating_sub(2));
    let contents = [complete.as_slice(), torn.as_slice()].concat();
    let path = temp.path().join("torn.jsonl.zstd");
    std::fs::write(&path, &contents).unwrap();
    let mut stream = JsonlPhysicalStream::open_with_encoding(
        File::open(&path).unwrap(),
        contents.len() as u64,
        0,
        0,
        JsonlPhysicalEncoding::ChecksummedZstdFrames,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::full_and_complete(
            JsonlResumableSha256::new(),
            JsonlResumableSha256::new(),
        ),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();
    assert!(stream.next_record().unwrap().unwrap().complete);
    assert!(!stream.next_record().unwrap().unwrap().complete);
    assert!(!stream.terminal());
    assert_eq!(stream.complete_prefix_end(), complete.len() as u64);
    assert_eq!(
        stream.complete_prefix_eof_hasher().unwrap().digest(),
        <[u8; 32]>::from(Sha256::digest(&complete))
    );
    assert_eq!(
        stream.digest().full_hasher().unwrap().digest(),
        <[u8; 32]>::from(Sha256::digest(&contents))
    );

    let mut corrupt = complete.clone();
    let last = corrupt.len().saturating_sub(1);
    corrupt[last] ^= 0xff;
    let corrupt_path = temp.path().join("corrupt.jsonl.zstd");
    std::fs::write(&corrupt_path, &corrupt).unwrap();
    let mut corrupt_stream = JsonlPhysicalStream::open_with_encoding(
        File::open(&corrupt_path).unwrap(),
        corrupt.len() as u64,
        0,
        0,
        JsonlPhysicalEncoding::ChecksummedZstdFrames,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();
    assert!(corrupt_stream.next_record().is_err());

    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
    std::io::Write::write_all(&mut encoder, b"{}\n").unwrap();
    let unchecked = encoder.finish().unwrap();
    let unchecked_path = temp.path().join("unchecked.jsonl.zstd");
    std::fs::write(&unchecked_path, &unchecked).unwrap();
    let mut unchecked_stream = JsonlPhysicalStream::open_with_encoding(
        File::open(&unchecked_path).unwrap(),
        unchecked.len() as u64,
        0,
        0,
        JsonlPhysicalEncoding::ChecksummedZstdFrames,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();
    assert!(unchecked_stream.next_record().is_err());

    let mut reserved = complete.clone();
    reserved[4] |= 0x10;
    let reserved_path = temp.path().join("reserved.jsonl.zstd");
    std::fs::write(&reserved_path, &reserved).unwrap();
    let mut reserved_stream = JsonlPhysicalStream::open_with_encoding(
        File::open(&reserved_path).unwrap(),
        reserved.len() as u64,
        0,
        0,
        JsonlPhysicalEncoding::ChecksummedZstdFrames,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();
    assert!(reserved_stream.next_record().is_err());
}
