//! Bounded accounting primitives for provider-owned source-backed Core pages.

use std::fmt;

use thiserror::Error;

pub(crate) const NATIVE_INGESTION_PAGE_MAX_UNITS: usize = 64;
pub(crate) const NATIVE_INGESTION_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const NATIVE_INGESTION_FRONTIER_MAX_BYTES: usize = 256 * 1024;

/// A provider-certified, opaque native cursor at a safe page boundary.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeSafeFrontier {
    pub(crate) version: u32,
    pub(crate) bytes: Vec<u8>,
}

impl NativeSafeFrontier {
    pub(crate) fn new(version: u32, bytes: Vec<u8>) -> Result<Self, NativeIngestionPageError> {
        if bytes.len() > NATIVE_INGESTION_FRONTIER_MAX_BYTES {
            return Err(NativeIngestionPageError::FrontierTooLarge { bytes: bytes.len() });
        }
        Ok(Self { version, bytes })
    }
}

impl fmt::Debug for NativeSafeFrontier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSafeFrontier")
            .field("version", &self.version)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Provider-certified conservative accounting for the full owned page.
///
/// `conservative_serialized_bytes` includes the provider-specific Core
/// encoding, the routing source key, and both safe frontier/checkpoint
/// encodings. The coordinator revalidates each page before Core sees it. The
/// claim does not change Core identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePageAccounting {
    pub(crate) logical_units: usize,
    pub(crate) conservative_serialized_bytes: usize,
}

/// One owned bounded page.  `C` remains provider-specific Core data.
#[derive(Debug)]
pub(crate) struct NativeIngestionPage<C> {
    pub(crate) expected_frontier: NativeSafeFrontier,
    pub(crate) next_safe_frontier: NativeSafeFrontier,
    pub(crate) terminal: bool,
    pub(crate) accounting: NativePageAccounting,
    pub(crate) core: C,
}

impl<C> NativeIngestionPage<C> {
    pub(crate) fn new(
        expected_frontier: NativeSafeFrontier,
        next_safe_frontier: NativeSafeFrontier,
        terminal: bool,
        accounting: NativePageAccounting,
        core: C,
    ) -> Result<Self, NativeIngestionPageError> {
        validate_page_accounting(accounting)?;
        validate_known_owned_payload_bytes(
            accounting,
            known_ingestion_page_owned_payload_bytes(&expected_frontier, &next_safe_frontier),
        )?;
        Ok(Self {
            expected_frontier,
            next_safe_frontier,
            terminal,
            accounting,
            core,
        })
    }
}

fn validate_page_accounting(
    accounting: NativePageAccounting,
) -> Result<(), NativeIngestionPageError> {
    if accounting.logical_units > NATIVE_INGESTION_PAGE_MAX_UNITS {
        return Err(NativeIngestionPageError::TooManyLogicalUnits {
            units: accounting.logical_units,
        });
    }
    if accounting.conservative_serialized_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Err(NativeIngestionPageError::TooManySerializedBytes {
            bytes: accounting.conservative_serialized_bytes,
        });
    }
    Ok(())
}

fn validate_known_owned_payload_bytes(
    accounting: NativePageAccounting,
    minimum: usize,
) -> Result<(), NativeIngestionPageError> {
    if accounting.conservative_serialized_bytes < minimum {
        return Err(NativeIngestionPageError::OwnedEncodedBytesUnderreported {
            claimed: accounting.conservative_serialized_bytes,
            minimum,
        });
    }
    Ok(())
}

#[derive(Default)]
struct NativeOwnedEncodedByteCounter {
    bytes: usize,
}

impl NativeOwnedEncodedByteCounter {
    fn add_fixed(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn add_bytes(&mut self, bytes: &[u8]) {
        self.add_fixed(size_of::<u64>());
        self.add_fixed(bytes.len());
    }

    fn add_frontier(&mut self, frontier: &NativeSafeFrontier) {
        self.add_fixed(size_of::<u32>());
        self.add_bytes(&frontier.bytes);
    }

    fn finish(self) -> usize {
        self.bytes
    }
}

fn known_ingestion_page_owned_payload_bytes(
    expected_frontier: &NativeSafeFrontier,
    next_safe_frontier: &NativeSafeFrontier,
) -> usize {
    let mut counter = NativeOwnedEncodedByteCounter::default();
    counter.add_frontier(expected_frontier);
    counter.add_frontier(next_safe_frontier);
    counter.finish()
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum NativeIngestionPageError {
    #[error(
        "NativePath page has {units} logical units; maximum is {NATIVE_INGESTION_PAGE_MAX_UNITS}"
    )]
    TooManyLogicalUnits { units: usize },
    #[error(
        "NativePath page conservatively serializes to {bytes} bytes; maximum is {NATIVE_INGESTION_PAGE_MAX_BYTES}"
    )]
    TooManySerializedBytes { bytes: usize },
    #[error(
        "NativePath safe frontier has {bytes} bytes; maximum is {NATIVE_INGESTION_FRONTIER_MAX_BYTES}"
    )]
    FrontierTooLarge { bytes: usize },
    #[error(
        "NativePath page claims {claimed} owned encoded payload bytes but its known frontier/output payload requires at least {minimum}"
    )]
    OwnedEncodedBytesUnderreported { claimed: usize, minimum: usize },
}
