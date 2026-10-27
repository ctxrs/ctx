//! Inspect decoded replacements before exposing their encoded audit payloads.
use crate::{privacy, spool::Spool, Result};
use ctx_history_provider_fx::{ReplacementScratch, ScratchFile};
use serde_json::Value;
use std::{fs::File, io::BufReader, path::Path};

pub(super) struct CaptureScratch(tempfile::TempDir);
impl CaptureScratch {
    pub fn new(parent: &Path) -> Result<Self> {
        Ok(Self(tempfile::tempdir_in(parent)?))
    }
    pub fn scan(&self, spool: &Spool) -> Result<()> {
        // Replay has validated each complete replacement and bounded its decoded
        // size. Visit one at a time after canonical state has been spooled. Two
        // passes allow a question and its answer to occur in different snapshots.
        for questions in [true, false] {
            for entry in std::fs::read_dir(self.0.path())? {
                let value: Value =
                    serde_json::from_reader(BufReader::new(File::open(entry?.path())?))?;
                if questions {
                    privacy::questions(spool, &value)?;
                } else {
                    privacy::answers(spool, &value)?;
                }
            }
        }
        Ok(())
    }
}
impl ReplacementScratch for CaptureScratch {
    fn create(&self, _maximum_bytes: u64) -> std::io::Result<Box<dyn ScratchFile>> {
        // The native replay reader enforces its byte limit before every write.
        // Keep the decoded file until the privacy scan; TempDir owns cleanup.
        let (file, _) = tempfile::NamedTempFile::new_in(self.0.path())?
            .keep()
            .map_err(|e| e.error)?;
        Ok(Box::new(file))
    }
}
