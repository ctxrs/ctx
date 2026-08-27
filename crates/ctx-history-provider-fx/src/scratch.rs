use std::io::{self, Read, Seek, Write};

pub trait ScratchFile: Read + Write + Seek + Send {}

impl<T: Read + Write + Seek + Send> ScratchFile for T {}

/// Provider-local replacement spool abstraction.
///
/// Replay enforces both `ReplayLimits::max_replacement_decoded_bytes` and
/// `ReplayLimits::max_scratch_bytes` before each write. A later executor may
/// inject a shared scratch implementation without changing protocol logic.
pub trait ReplacementScratch {
    fn create(&self, maximum_bytes: u64) -> io::Result<Box<dyn ScratchFile>>;
}

/// Anonymous OS-backed temporary storage. No provider path is consulted.
#[derive(Debug, Default, Clone, Copy)]
pub struct TempFileScratch;

impl ReplacementScratch for TempFileScratch {
    fn create(&self, _maximum_bytes: u64) -> io::Result<Box<dyn ScratchFile>> {
        Ok(Box::new(tempfile::tempfile()?))
    }
}
