use std::{
    fs::{self, File},
    io::{Error, ErrorKind, Read},
    path::{Path, PathBuf},
};

use ctx_history_core::{CertifiedSource, SourceKey};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::*;

const REPLAY_HINT_FORMAT_VERSION: u32 = 1;
const REPLAY_HINT_BINDING_DOMAIN: &[u8] = b"ctx-provider-sqlite-replay-hint-v1\0";
const REPLAY_HINT_DIRECTORY: &str = "provider-sqlite-replay-v1";
const REPLAY_HINT_MAX_BYTES: u64 = 64 * 1024;

/// Best-effort ctx-owned certificate for one exact physical replay.
///
/// This is metadata only: it contains the committed logical certificate, not
/// provider content. Every field is bound into `binding`, then compared with
/// the live source descriptor, parser, base manifest, and physical revision.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SqlitePhysicalReplayHint {
    format_version: u32,
    source: SourceKey,
    parser_revision: String,
    certificate: CertifiedSource,
    content_digest: [u8; 32],
    physical_revision: [u8; 32],
    binding: [u8; 32],
}

impl SqlitePhysicalReplayHint {
    pub(crate) fn load(data_root: &Path, source: &SourceKey) -> Option<SqlitePhysicalReplayHint> {
        let path = replay_hint_path(data_root, source);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.file_type().is_file() || metadata.len() > REPLAY_HINT_MAX_BYTES {
            return None;
        }
        let file = File::open(path).ok()?;
        let capacity = usize::try_from(metadata.len()).ok()?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(REPLAY_HINT_MAX_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        if u64::try_from(bytes.len()).ok()? > REPLAY_HINT_MAX_BYTES {
            return None;
        }
        let hint: Self = serde_json::from_slice(&bytes).ok()?;
        hint.valid_contract().then_some(hint)
    }

    pub(crate) fn matches(
        &self,
        source: &SourceKey,
        parser_revision: &str,
        physical_revision: &[u8; 32],
        committed: &CertifiedSource,
    ) -> bool {
        self.format_version == REPLAY_HINT_FORMAT_VERSION
            && self.source.exact_descriptor_eq(source)
            && self.parser_revision == parser_revision
            && self.physical_revision == *physical_revision
            && self.certificate == *committed
            && self
                .certificate
                .observation()
                .source()
                .exact_descriptor_eq(source)
            && self.certificate.parser_revision() == parser_revision
            && self.content_digest == *committed.content_digest()
    }

    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }

    pub(crate) fn publish_best_effort(
        data_root: &Path,
        source: &SourceKey,
        parser_revision: &str,
        certificate: &CertifiedSource,
        physical_revision: [u8; 32],
    ) {
        let _ = Self::publish(
            data_root,
            source,
            parser_revision,
            certificate,
            physical_revision,
        );
    }

    fn publish(
        data_root: &Path,
        source: &SourceKey,
        parser_revision: &str,
        certificate: &CertifiedSource,
        physical_revision: [u8; 32],
    ) -> std::io::Result<()> {
        if !certificate
            .observation()
            .source()
            .exact_descriptor_eq(source)
            || certificate.parser_revision() != parser_revision
            || certificate.validate_contract().is_err()
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "SQLite replay hint certificate does not match its source contract",
            ));
        }
        let mut hint = Self {
            format_version: REPLAY_HINT_FORMAT_VERSION,
            source: source.clone(),
            parser_revision: parser_revision.to_owned(),
            certificate: certificate.clone(),
            content_digest: *certificate.content_digest(),
            physical_revision,
            binding: [0; 32],
        };
        hint.binding = hint.compute_binding()?;
        let bytes = serde_json::to_vec(&hint).map_err(invalid_hint_data)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > REPLAY_HINT_MAX_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "SQLite replay hint exceeds its bounded size",
            ));
        }

        let directory = replay_hint_directory(data_root);
        create_private_directory_all(&directory)?;
        let mut temporary = NamedTempFile::new_in(&directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(replay_hint_path(data_root, source))
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn valid_contract(&self) -> bool {
        self.format_version == REPLAY_HINT_FORMAT_VERSION
            && self
                .certificate
                .observation()
                .source()
                .exact_descriptor_eq(&self.source)
            && self.certificate.parser_revision() == self.parser_revision
            && self.content_digest == *self.certificate.content_digest()
            && self.certificate.validate_contract().is_ok()
            && self
                .compute_binding()
                .is_ok_and(|binding| binding == self.binding)
    }

    fn compute_binding(&self) -> std::io::Result<[u8; 32]> {
        let certificate = serde_json::to_vec(&self.certificate).map_err(invalid_hint_data)?;
        let mut digest = Sha256::new();
        digest.update(REPLAY_HINT_BINDING_DOMAIN);
        digest.update(self.format_version.to_le_bytes());
        digest.update(self.source.exact_descriptor_digest());
        hash_hint_bytes(&mut digest, self.parser_revision.as_bytes());
        hash_hint_bytes(&mut digest, &certificate);
        digest.update(self.content_digest);
        digest.update(self.physical_revision);
        Ok(digest.finalize().into())
    }
}

fn replay_hint_directory(data_root: &Path) -> PathBuf {
    data_root.join("cache").join(REPLAY_HINT_DIRECTORY)
}

fn replay_hint_path(data_root: &Path, source: &SourceKey) -> PathBuf {
    let digest = source.exact_descriptor_digest();
    let mut name = String::with_capacity(digest.len() * 2 + 5);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".json");
    replay_hint_directory(data_root).join(name)
}

fn hash_hint_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn invalid_hint_data(error: serde_json::Error) -> Error {
    Error::new(ErrorKind::InvalidData, error)
}
