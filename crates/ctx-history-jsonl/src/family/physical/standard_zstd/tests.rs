use std::{
    fs::{File, OpenOptions},
    io::{Cursor, Write},
};

use ctx_history_capture_runtime::{SourceBackedRouteResourceKind, SourceBackedRouteResources};
use ctx_history_source_io::SourceIoError;
use sha2::{Digest, Sha256};

use super::*;
use crate::family::physical::{
    JsonlPhysicalDigest, JsonlPhysicalEncoding, JsonlPhysicalStream, JsonlResumableSha256,
};

fn standard_stream(plaintext: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(Cursor::new(plaintext), 1).unwrap()
}

#[test]
fn standard_zstd_jsonl_streams_bounded_records_and_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl.zst");
    let plaintext = b"{\"type\":\"session_meta\"}\n{\"type\":\"response_item\"}\n";
    let compressed = standard_stream(plaintext);
    std::fs::write(&path, &compressed).unwrap();
    let mut stream = JsonlPhysicalStream::open_with_encoding(
        File::open(&path).unwrap(),
        compressed.len() as u64,
        0,
        0,
        JsonlPhysicalEncoding::StandardZstdJsonl,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::full_and_complete(
            JsonlResumableSha256::new(),
            JsonlResumableSha256::new(),
        ),
        || SourceIoError::SourceChangedDuringCapture,
    )
    .unwrap();

    let first = stream.next_record().unwrap().unwrap();
    assert_eq!(stream.record_bytes(first), b"{\"type\":\"session_meta\"}");
    let after_first = stream.position();
    let second = stream.next_record().unwrap().unwrap();
    assert_eq!(stream.record_bytes(second), b"{\"type\":\"response_item\"}");
    stream.restore(after_first).unwrap();
    assert_eq!(stream.next_record().unwrap().unwrap(), second);
    assert!(stream.next_record().unwrap().is_none());
    assert!(stream.terminal());
    assert_eq!(stream.complete_prefix_end(), compressed.len() as u64);
    assert_eq!(stream.next_physical_ordinal(), 2);
    assert_eq!(
        stream.digest().complete_hasher().digest(),
        <[u8; 32]>::from(Sha256::digest(&compressed))
    );
    assert_eq!(
        stream.digest().full_hasher().unwrap().digest(),
        <[u8; 32]>::from(Sha256::digest(&compressed))
    );
}

#[test]
fn standard_zstd_jsonl_rejects_corrupt_truncated_and_unterminated_streams() {
    let temp = tempfile::tempdir().unwrap();
    let valid = standard_stream(b"{}\n");
    let cases = [
        ("corrupt", {
            let mut value = valid.clone();
            value[0] ^= 0xff;
            value
        }),
        ("truncated", valid[..valid.len().saturating_sub(2)].to_vec()),
        ("unterminated", standard_stream(b"{}")),
    ];
    for (label, bytes) in cases {
        let path = temp.path().join(format!("{label}.jsonl.zst"));
        std::fs::write(&path, &bytes).unwrap();
        let result = JsonlPhysicalStream::open_with_encoding(
            File::open(&path).unwrap(),
            bytes.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::StandardZstdJsonl,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
            || SourceIoError::SourceChangedDuringCapture,
        );
        assert!(result.is_err(), "{label} stream was accepted");
    }
}

#[test]
fn standard_zstd_jsonl_enforces_physical_logical_and_expansion_limits() {
    let temp = tempfile::tempdir().unwrap();
    let plaintext = b"{}\n".repeat(4096);
    let compressed = standard_stream(&plaintext);
    let path = temp.path().join("bounded.jsonl.zst");
    std::fs::write(&path, &compressed).unwrap();
    let base = StandardZstdLimits {
        compressed_bytes: compressed.len() as u64,
        decompressed_bytes: plaintext.len() as u64,
        expansion_ratio: u64::MAX,
        expansion_slack_bytes: u64::MAX,
    };

    for limits in [
        StandardZstdLimits {
            compressed_bytes: (compressed.len() as u64).saturating_sub(1),
            ..base
        },
        StandardZstdLimits {
            decompressed_bytes: (plaintext.len() as u64).saturating_sub(1),
            ..base
        },
        StandardZstdLimits {
            expansion_ratio: 1,
            expansion_slack_bytes: 0,
            ..base
        },
    ] {
        let result = decode_standard_zstd_jsonl::<SourceIoError>(
            File::open(&path).unwrap(),
            compressed.len() as u64,
            JsonlRecordFraming::ordinary(),
            limits,
            None,
        );
        assert!(result.is_err());
    }
}

#[test]
fn standard_zstd_accepts_a_large_spool_within_the_route_budget() {
    let plaintext = b"{}\n".repeat(1024);
    let frame = standard_stream(&plaintext);
    let frames = (256 * 1024 * 1024) / plaintext.len() + 1;
    let mut file = tempfile::tempfile().unwrap();
    let mut writer = std::io::BufWriter::new(&mut file);
    for _ in 0..frames {
        writer.write_all(&frame).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);

    let compressed = (frames * frame.len()) as u64;
    let decoded = (frames * plaintext.len()) as u64;
    let resources = SourceBackedRouteResources::production(MAX_STANDARD_ZSTD_PARALLEL_STREAMS);
    let stream = decode_standard_zstd_jsonl::<SourceIoError>(
        file,
        compressed,
        JsonlRecordFraming::ordinary(),
        StandardZstdLimits::PRODUCTION,
        Some(&resources),
    )
    .unwrap();
    assert_eq!(stream.logical_length, decoded);
    assert_eq!(stream.total_records, (frames * 1024) as u64);
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::LogicalSourceScratch),
        compressed + decoded
    );
}

#[test]
fn standard_zstd_worker_admission_uses_the_largest_scratch_bound() {
    let small = 64 * 1024;
    assert_eq!(standard_zstd_parallel_limit(std::iter::empty()), None);
    assert_eq!(
        standard_zstd_parallel_limit([small, small].into_iter()),
        Some(MAX_STANDARD_ZSTD_PARALLEL_STREAMS)
    );
    assert_eq!(
        standard_zstd_parallel_limit([small, 256 * 1024 * 1024].into_iter()),
        Some(1)
    );
}

#[test]
fn standard_zstd_snapshot_hash_and_decode_use_only_the_certified_prefix() {
    for overwrite in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bounded-prefix.jsonl.zst");
        let plaintext = b"{\"type\":\"session_meta\"}\n{\"type\":\"response_item\"}\n";
        let compressed = standard_stream(plaintext);
        std::fs::write(&path, &compressed).unwrap();
        let hook_path = path.clone();
        let appended = standard_stream(b"{\"type\":\"appended\"}\n");
        let hook_compressed = compressed.clone();
        let hook_appended = appended.clone();
        set_after_standard_zstd_snapshot_hook(move || {
            if overwrite {
                let mut replacement = hook_compressed;
                replacement[0] ^= 0xff;
                OpenOptions::new()
                    .write(true)
                    .open(&hook_path)
                    .unwrap()
                    .write_all(&replacement)
                    .unwrap();
            } else {
                OpenOptions::new()
                    .append(true)
                    .open(&hook_path)
                    .unwrap()
                    .write_all(&hook_appended)
                    .unwrap();
            }
        });

        let mut stream = JsonlPhysicalStream::open_with_encoding(
            File::open(&path).unwrap(),
            compressed.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::StandardZstdJsonl,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
            || SourceIoError::SourceChangedDuringCapture,
        )
        .unwrap();
        let mut records = Vec::new();
        while let Some(record) = stream.next_record().unwrap() {
            records.push(stream.record_bytes(record).to_vec());
        }
        assert_eq!(
            records,
            [
                b"{\"type\":\"session_meta\"}".to_vec(),
                b"{\"type\":\"response_item\"}".to_vec()
            ]
        );
        assert_eq!(
            stream.digest().complete_hasher().digest(),
            <[u8; 32]>::from(Sha256::digest(&compressed))
        );
        let current = std::fs::read(&path).unwrap();
        if overwrite {
            assert_ne!(&current[..compressed.len()], compressed.as_slice());
        } else {
            assert_eq!(&current[..compressed.len()], compressed.as_slice());
            assert_eq!(&current[compressed.len()..], appended.as_slice());
        }
    }
}

#[test]
fn standard_zstd_parallel_scratch_budget_fails_closed_and_releases() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("parallel-budget.jsonl.zst");
    let plaintext = b"{}\n".repeat(4096);
    let compressed = standard_stream(&plaintext);
    std::fs::write(&path, &compressed).unwrap();
    let one_stream_bytes = compressed.len().saturating_add(plaintext.len());
    let resources = SourceBackedRouteResources::for_test(2, 0, one_stream_bytes as u64);
    let open = || {
        JsonlPhysicalStream::open_with_encoding_and_resources(
            File::open(&path).unwrap(),
            compressed.len() as u64,
            0,
            0,
            JsonlPhysicalEncoding::StandardZstdJsonl,
            JsonlRecordFraming::ordinary(),
            JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
            || SourceIoError::SourceChangedDuringCapture,
            Some(&resources),
        )
    };

    let first = open().unwrap();
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::LogicalSourceScratch),
        one_stream_bytes as u64
    );
    assert!(
        open().is_err(),
        "a concurrent second spool exceeded the shared budget"
    );
    drop(first);
    assert_eq!(
        resources.live_bytes(SourceBackedRouteResourceKind::LogicalSourceScratch),
        0
    );
    assert!(open().is_ok());
}
