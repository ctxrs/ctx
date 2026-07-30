use std::{
    fs::Metadata,
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::Result;

use super::CODEBUDDY_CAPTURE_REVISION;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodeBuddyFrozenFile {
    pub(super) length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl CodeBuddyFrozenFile {
    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    pub(super) fn update_revision(&self, revision: &mut CodeBuddyRevisionHasher) {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (b'+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (b'-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        revision.update(&self.length.to_be_bytes());
        revision.update(&[side]);
        revision.update(&seconds.to_be_bytes());
        revision.update(&nanos.to_be_bytes());
        revision.update(&[u8::from(self.readonly)]);
        revision.update(&self.device.unwrap_or(u64::MAX).to_be_bytes());
        revision.update(&self.inode.unwrap_or(u64::MAX).to_be_bytes());
    }

    pub(super) fn source_revision_with_policy(&self, shape: &str, policy_revision: u32) -> String {
        let mut revision = CodeBuddyRevisionHasher::new();
        revision.update(shape.as_bytes());
        revision.update(&CODEBUDDY_CAPTURE_REVISION.to_be_bytes());
        revision.update(&policy_revision.to_be_bytes());
        self.update_revision(&mut revision);
        format!("codebuddy-{shape}-v1:fnv1a64:{:016x}", revision.finish())
    }

    pub(super) fn identity_token(&self) -> String {
        match (self.device, self.inode) {
            (Some(device), Some(inode)) => format!("unix:{device}:{inode}"),
            _ => "metadata-only".to_owned(),
        }
    }

    pub(super) fn modified(&self) -> SystemTime {
        self.modified
    }
}

pub(super) struct CodeBuddyRevisionHasher(u64);

impl CodeBuddyRevisionHasher {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    pub(super) fn new() -> Self {
        Self(Self::OFFSET)
    }

    pub(super) fn update(&mut self, bytes: &[u8]) {
        self.0 ^= bytes.len() as u64;
        self.0 = self.0.wrapping_mul(Self::PRIME);
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    pub(super) fn finish(&self) -> u64 {
        self.0
    }
}

impl Write for CodeBuddyRevisionHasher {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
