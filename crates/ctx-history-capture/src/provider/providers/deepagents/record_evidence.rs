use sha2::{Digest, Sha256};

use crate::record_evidence::RecordDigest;

use super::source::DeepAgentsWriteKey;

pub(super) fn deepagents_write_record_digest(
    key: &DeepAgentsWriteKey,
    value_type: Option<&str>,
    value: &[u8],
) -> RecordDigest {
    const DOMAIN: &[u8] = b"ctx-deepagents-write-record-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    update_digest_string(&mut digest, &key.thread_id);
    update_digest_string(&mut digest, &key.checkpoint_id);
    update_digest_string(&mut digest, &key.task_id);
    digest.update(key.idx.to_be_bytes());
    match value_type {
        Some(value_type) => {
            digest.update([1]);
            update_digest_string(&mut digest, value_type);
        }
        None => digest.update([0]),
    }
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
    RecordDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

fn update_digest_string(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}
