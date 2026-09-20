use super::*;
use crate::conversation::Artifact;
use ctx_history_provider_runtime::source_io::{OpenedProviderSourcePath, ProviderSourceDirectory};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Ephemeral observations only: checkpoints contain one digest, never a growing
/// list of references. Any inventory mutation deliberately replays the session.
pub(super) struct Artifacts {
    authority: Arc<ProviderSourceRoot>,
    path: PathBuf,
    directory: Option<ProviderSourceDirectory>,
    files: BTreeMap<std::ffi::OsString, [u8; 32]>,
    digest: [u8; 32],
}

impl Artifacts {
    pub(super) fn observe(
        authority: Arc<ProviderSourceRoot>,
        path: PathBuf,
    ) -> Result<Self, CaptureError> {
        let directory = match authority.open_directory(&path) {
            Ok(directory) => Some(directory),
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error),
        };
        let mut files = BTreeMap::new();
        let mut digest = Sha256::new();
        digest.update(b"fx-tool-results-inventory-v1\0");
        digest.update(authority.authority_fingerprint());
        digest.update(path.as_os_str().as_encoded_bytes());
        if let Some(directory) = &directory {
            digest.update([1]);
            digest.update(directory.authority_fingerprint());
            let mut names = directory.entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)?;
            names.sort();
            for name in names {
                let bytes = name.as_encoded_bytes();
                digest.update((bytes.len() as u64).to_be_bytes());
                digest.update(bytes);
                match directory.open_child(&name) {
                    Ok(OpenedProviderSourcePath::File(file)) => {
                        let fingerprint = file.authority_fingerprint();
                        digest.update([1]);
                        digest.update(fingerprint);
                        files.insert(name, fingerprint);
                        file.revalidate()?;
                    }
                    Ok(OpenedProviderSourcePath::Directory(child)) => {
                        digest.update([2]);
                        digest.update(child.authority_fingerprint());
                    }
                    Err(error) if error.is_ignorable_membership_entry() => {
                        digest.update([3]);
                    }
                    Err(error) => return Err(error),
                }
            }
            directory.revalidate()?;
        } else {
            digest.update([0]);
        }
        Ok(Self {
            authority,
            path,
            directory,
            files,
            digest: digest.finalize().into(),
        })
    }

    fn fresh_digest(&self) -> Result<[u8; 32], CaptureError> {
        if let Some(directory) = &self.directory {
            directory.revalidate()?;
        }
        Ok(Self::observe(Arc::clone(&self.authority), self.path.clone())?.digest)
    }

    pub(super) fn read(&self, name: &str) -> Result<Artifact, CaptureError> {
        if !matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [std::path::Component::Normal(_)]
        ) {
            return Ok(Artifact::Unavailable);
        }
        let Some(expected) = self.files.get(std::ffi::OsStr::new(name)) else {
            return Ok(Artifact::Unavailable);
        };
        let opened = self.authority.open_file(&self.path.join(name))?;
        if opened.authority_fingerprint() != *expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
        if opened.len() > MAX_ARTIFACT_BYTES as u64 {
            opened.revalidate()?;
            return Ok(Artifact::Omitted {
                reason: "fx artifact exceeds bounded content capture".into(),
                observed_bytes: opened.len(),
            });
        }
        Ok(Artifact::Present(
            opened.read_all_bounded(MAX_ARTIFACT_BYTES)?,
        ))
    }
}

pub(super) struct SessionArtifacts {
    tool: Artifacts,
    command: Artifacts,
}
impl SessionArtifacts {
    pub(super) fn observe(
        authority: Arc<ProviderSourceRoot>,
        session: &Path,
    ) -> Result<Self, CaptureError> {
        Ok(Self {
            tool: Artifacts::observe(Arc::clone(&authority), session.join("tool-results"))?,
            command: Artifacts::observe(authority, session.join("logs/commands"))?,
        })
    }
    pub(super) fn bind(
        self: &Arc<Self>,
        leaf: ProviderJsonlLeaf,
    ) -> Result<ProviderJsonlLeaf, CaptureError> {
        let snapshot = Arc::clone(self);
        leaf.with_compound_input(combined(self.tool.digest, self.command.digest), move || {
            Ok(combined(
                snapshot.tool.fresh_digest()?,
                snapshot.command.fresh_digest()?,
            ))
        })
    }
    pub(super) fn read(
        &self,
        kind: crate::conversation::ArtifactKind,
        name: &str,
    ) -> Result<Artifact, CaptureError> {
        match kind {
            crate::conversation::ArtifactKind::Tool => self.tool.read(name),
            crate::conversation::ArtifactKind::Command => self.command.read(name),
        }
    }
}
fn combined(tool: [u8; 32], command: [u8; 32]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"fx-session-artifacts-v1\0");
    digest.update(tool);
    digest.update(command);
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_observes_missing_directory_and_file_repairs_and_rejects_changed_reads() {
        let temp = tempfile::tempdir().unwrap();
        let root = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let missing = Artifacts::observe(Arc::clone(&root), "tool-results".into()).unwrap();
        assert!(matches!(
            missing.read("result.txt").unwrap(),
            Artifact::Unavailable
        ));
        std::fs::create_dir(temp.path().join("tool-results")).unwrap();
        std::fs::write(temp.path().join("tool-results/result.txt"), "original").unwrap();
        let root = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let present = Artifacts::observe(Arc::clone(&root), "tool-results".into()).unwrap();
        assert_ne!(missing.digest, present.digest);
        assert!(
            matches!(present.read("result.txt").unwrap(), Artifact::Present(bytes) if bytes == b"original")
        );
        std::fs::write(temp.path().join("tool-results/replacement.txt"), "modified").unwrap();
        std::fs::rename(
            temp.path().join("tool-results/replacement.txt"),
            temp.path().join("tool-results/result.txt"),
        )
        .unwrap();
        assert!(present.read("result.txt").is_err());
        let root = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let changed = Artifacts::observe(root, "tool-results".into()).unwrap();
        assert_ne!(present.digest, changed.digest);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_artifact_members_never_follow_outside_the_retained_directory() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("tool-results")).unwrap();
        std::fs::write(temp.path().join("outside"), "private outside bytes").unwrap();
        std::os::unix::fs::symlink("../outside", temp.path().join("tool-results/link")).unwrap();
        let root = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let artifacts = Artifacts::observe(root, "tool-results".into()).unwrap();
        for name in ["../outside", "link", "/outside"] {
            assert!(matches!(
                artifacts.read(name).unwrap(),
                Artifact::Unavailable
            ));
        }
        std::fs::rename(temp.path().join("tool-results"), temp.path().join("old")).unwrap();
        std::fs::create_dir(temp.path().join("tool-results")).unwrap();
        std::fs::write(temp.path().join("tool-results/link"), "replacement").unwrap();
        assert!(!artifacts
            .fresh_digest()
            .is_ok_and(|digest| digest == artifacts.digest));
    }
}
