use std::{
    collections::BTreeSet,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{
    io::{retained_jsonl_file_v1_identity, retained_ordinary_file_v2_identity},
    open_provider_source_file, OpenedProviderSourceFile, Result,
};

pub(crate) const ORDINARY_FILE_V2_TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";
const ORDINARY_FILE_V2_FULL_FINGERPRINT_MAX_BYTES: u64 = 64 * 1024;
const ORDINARY_FILE_V2_SPARSE_SAMPLE_BYTES: u64 = 8 * 1024;

/// A bounded observation of an ordinary provider file.
///
/// Length and mtime retain the inexpensive append/no-op checks used by callers.
/// The token is the root-handle layer's fixed-width fingerprint of the exact
/// opened object and its change stamp. The same opened-handle proof is
/// revalidated before the observation escapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryFileObservation {
    len: u64,
    modified_at: SystemTime,
    token: [u8; 32],
}

impl OrdinaryFileObservation {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub fn token_hex(&self) -> String {
        self.token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Exact stable and change tokens for one retained ordinary-file identity
/// contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedFileIdentity {
    stable: [u8; 32],
    change: [u8; 32],
}

impl RetainedFileIdentity {
    pub fn stable(self) -> [u8; 32] {
        self.stable
    }

    pub fn change(self) -> [u8; 32] {
        self.change
    }
}

/// Returns the released shared-JSONL v1 identity for an already-opened file.
pub fn retained_jsonl_file_identity_v1(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> Result<Option<RetainedFileIdentity>> {
    retained_jsonl_file_v1_identity(path, file, metadata)
        .map(|identity| identity.map(|(stable, change)| RetainedFileIdentity { stable, change }))
}

/// Versioned ordinary-file facts suitable for a serialized inventory token.
///
/// This preserves the ordinary-file-v2 token domains and portable sparse
/// fallback exactly. Provider adapters decide how to serialize these facts and
/// whether a change is retryable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryFileObservationV2 {
    len: u64,
    modified_at: SystemTime,
    stable_token: Option<[u8; 32]>,
    change_token: [u8; 32],
}

impl OrdinaryFileObservationV2 {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    pub fn stable_token(&self) -> Option<[u8; 32]> {
        self.stable_token
    }

    pub fn change_token(&self) -> [u8; 32] {
        self.change_token
    }
}

/// Observes an already-opened ordinary file using the versioned inventory
/// token contract.
///
/// The two metadata observations and optional content fingerprint fence the
/// returned values against concurrent changes. This function does not open the
/// file by path.
pub fn observe_opened_ordinary_file_v2(
    path: &Path,
    file: &File,
) -> Result<OrdinaryFileObservationV2> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(crate::SourceIoError::SourceChangedDuringCapture);
    }
    let platform_before = retained_ordinary_file_v2_identity(path, file, &metadata)?;
    let content_fingerprint = if platform_before.is_some() {
        None
    } else {
        Some(opened_file_content_fingerprint_v2(file, &metadata)?)
    };
    let current = file.metadata()?;
    let platform_after = retained_ordinary_file_v2_identity(path, file, &current)?;
    if current.len() != metadata.len()
        || current.modified().ok() != metadata.modified().ok()
        || platform_after != platform_before
    {
        return Err(crate::SourceIoError::SourceChangedDuringCapture);
    }
    Ok(OrdinaryFileObservationV2 {
        len: metadata.len(),
        modified_at: metadata.modified().unwrap_or(UNIX_EPOCH),
        stable_token: platform_before.map(|tokens| tokens.0),
        change_token: combine_ordinary_file_v2_token(
            platform_before.map(|tokens| tokens.1),
            content_fingerprint,
        ),
    })
}

/// SHA-256 of exactly `len` bytes from the start of an already-opened file.
pub fn opened_file_prefix_sha256(file: &File, len: u64) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut reader = file.try_clone()?;
    hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    Ok(hasher.finalize().into())
}

fn combine_ordinary_file_v2_token(
    platform_token: Option<[u8; 32]>,
    content_fingerprint: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_V2_TOKEN_DOMAIN);
    if let Some(platform_token) = platform_token {
        hasher.update(b"platform\0");
        hasher.update(platform_token);
    } else {
        hasher.update(b"portable\0");
        match content_fingerprint {
            Some(content_fingerprint) => hasher.update(content_fingerprint),
            None => hasher.update(b"missing-content-fingerprint\0"),
        }
    }
    hasher.finalize().into()
}

fn opened_file_content_fingerprint_v2(file: &File, metadata: &Metadata) -> Result<[u8; 32]> {
    let len = metadata.len();
    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_V2_TOKEN_DOMAIN);
    hasher.update(len.to_le_bytes());
    let mut reader = file.try_clone()?;
    let original_position = reader.stream_position()?;
    if len <= ORDINARY_FILE_V2_FULL_FINGERPRINT_MAX_BYTES {
        hasher.update(b"full\0");
        hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    } else {
        hasher.update(b"sparse\0");
        for offset in opened_file_sparse_sample_offsets_v2(len) {
            let sample_len = ORDINARY_FILE_V2_SPARSE_SAMPLE_BYTES.min(len.saturating_sub(offset));
            hasher.update(offset.to_le_bytes());
            hasher.update(sample_len.to_le_bytes());
            hash_opened_file_range(&mut reader, offset, sample_len, &mut hasher)?;
        }
    }
    reader.seek(SeekFrom::Start(original_position))?;
    Ok(hasher.finalize().into())
}

fn opened_file_sparse_sample_offsets_v2(len: u64) -> BTreeSet<u64> {
    let last = len.saturating_sub(ORDINARY_FILE_V2_SPARSE_SAMPLE_BYTES);
    [0, len / 4, len / 2, len.saturating_mul(3) / 4, last]
        .into_iter()
        .map(|offset| offset.min(last))
        .collect()
}

fn hash_opened_file_range(
    file: &mut File,
    offset: u64,
    len: u64,
    hasher: &mut Sha256,
) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    let mut buffer = [0_u8; 8 * 1024];
    while remaining > 0 {
        let take = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(crate::SourceIoError::SourceChangedDuringCapture);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok(())
}

pub fn observe_ordinary_file(path: impl AsRef<Path>) -> Result<OrdinaryFileObservation> {
    observe_ordinary_file_inner(path.as_ref(), || {})
}

fn observe_ordinary_file_inner(
    path: &Path,
    before_open: impl FnOnce(),
) -> Result<OrdinaryFileObservation> {
    before_open();
    let opened = open_provider_source_file(path)?;
    observe_opened_ordinary_file(path, &opened)
}

pub fn observe_opened_ordinary_file(
    _path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<OrdinaryFileObservation> {
    let token = opened.ordinary_file_token();
    opened.revalidate_leaf()?;

    Ok(OrdinaryFileObservation {
        len: opened.len(),
        modified_at: opened.modified().unwrap_or(UNIX_EPOCH),
        token,
    })
}

pub fn open_ordinary_file_without_following(path: &Path) -> Result<File> {
    open_provider_source_file(path)?
        .file()
        .try_clone()
        .map_err(Into::into)
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use std::{
        io::{Seek, SeekFrom, Write},
        time::Duration,
    };

    use super::*;

    fn hex_digest(digest: [u8; 32]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn prefix_sha256_hashes_exactly_the_requested_bytes() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"prefix\nsuffix\n").unwrap();
        let file = File::open(&path).unwrap();

        let digest = opened_file_prefix_sha256(&file, 7).unwrap();

        assert_eq!(
            hex_digest(digest),
            "5a958fd0cb0435992ec0b7afb3255dbe976078447b0fe2830119c083b9eae082"
        );
    }

    #[test]
    fn full_and_sparse_fingerprints_preserve_v2_hash_oracles_and_cursor() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        for (len, expected) in [
            (
                64 * 1024,
                "38874611e67db54c2f465711881da92f79279fddb8d8ee1c077b24e30833a45f",
            ),
            (
                64 * 1024 + 1,
                "ee71a52819ad21e4f257577e99b394a43b5b047fb6e068ac96ddafb7e0ff6d6c",
            ),
        ] {
            let path = temp.path().join(format!("source-{len}.jsonl"));
            let bytes: Vec<_> = (0..len).map(|index| (index % 251) as u8).collect();
            std::fs::write(&path, bytes).unwrap();
            let mut file = File::open(&path).unwrap();
            file.seek(SeekFrom::Start(31)).unwrap();

            let digest =
                opened_file_content_fingerprint_v2(&file, &file.metadata().unwrap()).unwrap();

            assert_eq!(hex_digest(digest), expected);
            assert_eq!(file.stream_position().unwrap(), 31);
        }
    }

    #[test]
    fn v2_combined_token_domains_preserve_platform_and_portable_oracles() {
        assert_eq!(
            hex_digest(combine_ordinary_file_v2_token(Some([7; 32]), None)),
            "47557d3a3234d411619816b639cc70b35a8a615f8eafd5f15102a59e597569b9"
        );
        assert_eq!(
            hex_digest(combine_ordinary_file_v2_token(None, Some([7; 32]))),
            "32e310c6c69c525e2d74e00ef9f3ebf83d839748d3acdfe428ff071042bc7561"
        );
    }

    #[test]
    fn v2_is_empty_tracks_zero_and_nonzero_observations() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let cases: [(&str, &[u8], u64, bool); 2] = [
            ("empty.jsonl", b"", 0, true),
            ("nonempty.jsonl", b"content\n", 8, false),
        ];

        for (name, contents, expected_len, expected_empty) in cases {
            let path = temp.path().join(name);
            std::fs::write(&path, contents).unwrap();
            let file = File::open(&path).unwrap();
            let observation = observe_opened_ordinary_file_v2(&path, &file).unwrap();

            assert_eq!(observation.len(), expected_len);
            assert_eq!(observation.is_empty(), expected_empty);
        }
    }

    #[cfg(unix)]
    #[test]
    fn v2_observation_preserves_unix_stable_and_change_token_bytes() {
        use std::os::unix::fs::MetadataExt;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"content\n").unwrap();
        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let observation = observe_opened_ordinary_file_v2(&path, &file).unwrap();

        let mut stable = Sha256::new();
        stable.update(b"ctx-ordinary-file-observation-v2\0unix-stable\0");
        stable.update(metadata.dev().to_le_bytes());
        stable.update(metadata.ino().to_le_bytes());
        stable.update(metadata.mode().to_le_bytes());
        let mut platform_change = Sha256::new();
        platform_change.update(b"ctx-ordinary-file-observation-v2\0unix-change\0");
        platform_change.update(metadata.dev().to_le_bytes());
        platform_change.update(metadata.ino().to_le_bytes());
        platform_change.update(metadata.ctime().to_le_bytes());
        platform_change.update(metadata.ctime_nsec().to_le_bytes());

        assert_eq!(observation.len(), metadata.len());
        assert_eq!(observation.modified_at(), metadata.modified().unwrap());
        assert_eq!(
            observation.stable_token(),
            Some(<[u8; 32]>::from(stable.finalize()))
        );
        assert_eq!(
            observation.change_token(),
            combine_ordinary_file_v2_token(Some(platform_change.finalize().into()), None)
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_jsonl_v1_identity_preserves_unix_token_bytes() {
        use std::os::unix::fs::MetadataExt;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"content\n").unwrap();
        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let identity = retained_jsonl_file_identity_v1(&path, &file, &metadata)
            .unwrap()
            .unwrap();

        let mut stable = Sha256::new();
        stable.update(b"ctx-jsonl-retained-file-identity-v1\0unix-stable\0");
        stable.update(metadata.dev().to_le_bytes());
        stable.update(metadata.ino().to_le_bytes());
        let mut change = Sha256::new();
        change.update(b"ctx-jsonl-retained-file-identity-v1\0unix-change\0");
        change.update(metadata.ctime().to_le_bytes());
        change.update(metadata.ctime_nsec().to_le_bytes());

        assert_eq!(identity.stable(), <[u8; 32]>::from(stable.finalize()));
        assert_eq!(identity.change(), <[u8; 32]>::from(change.finalize()));
    }

    #[cfg(unix)]
    #[test]
    fn shared_jsonl_v1_identity_treats_hardlink_churn_as_change_only() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let link = temp.path().join("temporary-link.jsonl");
        std::fs::write(&path, b"content\n").unwrap();
        let file = File::open(&path).unwrap();
        let before_metadata = file.metadata().unwrap();
        let before = retained_jsonl_file_identity_v1(&path, &file, &before_metadata)
            .unwrap()
            .unwrap();

        std::thread::sleep(Duration::from_millis(2));
        std::fs::hard_link(&path, &link).unwrap();
        std::fs::remove_file(&link).unwrap();

        let after_metadata = file.metadata().unwrap();
        let after = retained_jsonl_file_identity_v1(&path, &file, &after_metadata)
            .unwrap()
            .unwrap();
        assert_eq!(before_metadata.len(), after_metadata.len());
        assert_eq!(
            before_metadata.modified().unwrap(),
            after_metadata.modified().unwrap()
        );
        assert_eq!(before.stable(), after.stable());
        assert_ne!(before.change(), after.change());
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn shared_jsonl_v1_identity_is_repeatable_for_the_same_opened_object() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"content\n").unwrap();
        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();

        let first = retained_jsonl_file_identity_v1(&path, &file, &metadata)
            .unwrap()
            .unwrap();
        let second = retained_jsonl_file_identity_v1(&path, &file, &file.metadata().unwrap())
            .unwrap()
            .unwrap();

        assert_eq!(first.stable(), second.stable());
        assert_eq!(first.change(), second.change());
    }

    #[test]
    fn observation_token_is_derived_from_the_opened_object_stamp() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"content\n").unwrap();
        let opened = open_provider_source_file(&path).unwrap();

        let observation = observe_opened_ordinary_file(&path, &opened).unwrap();

        assert_eq!(observation.token(), &opened.ordinary_file_token());
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn opened_authority_fingerprint_detects_same_size_rewrite_with_restored_mtime() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let source = vec![b'a'; 128 * 1024];
        std::fs::write(&path, source).unwrap();
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let first = observe_ordinary_file(&path).unwrap();

        std::thread::sleep(Duration::from_millis(2));
        let mut file = File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(16 * 1024)).unwrap();
        file.write_all(b"b").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        drop(file);
        let second = observe_ordinary_file(&path).unwrap();

        assert_eq!(first.len(), second.len());
        assert_eq!(first.modified_at(), second.modified_at());
        assert_ne!(first.token(), second.token());
    }

    #[test]
    fn opened_observation_rejects_named_replacement() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        std::fs::write(&path, b"original\n").unwrap();
        let opened = open_provider_source_file(&path).unwrap();

        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement\n").unwrap();

        assert!(observe_opened_ordinary_file(&path, &opened).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_final_component() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target = temp.path().join("target.jsonl");
        let link = temp.path().join("link.jsonl");
        std::fs::write(&target, b"content\n").unwrap();
        symlink(&target, &link).unwrap();

        assert!(observe_ordinary_file(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_parent_component() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target_parent = temp.path().join("target-parent");
        let link_parent = temp.path().join("link-parent");
        std::fs::create_dir(&target_parent).unwrap();
        std::fs::write(target_parent.join("source.jsonl"), b"content\n").unwrap();
        symlink(&target_parent, &link_parent).unwrap();

        assert!(observe_ordinary_file(link_parent.join("source.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_final_component_symlink_swapped_before_open() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        let target = temp.path().join("target.jsonl");
        std::fs::write(&path, b"original\n").unwrap();
        std::fs::write(&target, b"replacement\n").unwrap();

        let result = observe_ordinary_file_inner(&path, || {
            std::fs::rename(&path, &moved).unwrap();
            symlink(&target, &path).unwrap();
        });

        assert!(result.is_err());
    }
}
