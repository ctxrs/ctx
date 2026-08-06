use std::{io::Write, time::Duration};

use anyhow::Result;

#[derive(Clone, Copy, Debug)]
pub struct ArtifactFetchRequest<'a> {
    endpoint: &'a str,
    max_bytes: u64,
    timeout: Duration,
}

impl<'a> ArtifactFetchRequest<'a> {
    #[cfg_attr(
        not(any(target_os = "macos", test, feature = "test-support")),
        allow(dead_code)
    )]
    pub(crate) const fn new(endpoint: &'a str, max_bytes: u64, timeout: Duration) -> Self {
        Self {
            endpoint,
            max_bytes,
            timeout,
        }
    }

    pub fn endpoint(self) -> &'a str {
        self.endpoint
    }

    pub fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    pub fn timeout(self) -> Duration {
        self.timeout
    }
}

pub trait ArtifactFetcher {
    fn fetch_to_writer(
        &self,
        request: ArtifactFetchRequest<'_>,
        writer: &mut dyn Write,
    ) -> Result<u64>;
}
