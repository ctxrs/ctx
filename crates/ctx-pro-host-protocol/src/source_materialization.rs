use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, EventHydrationRequest,
    SessionHydrationRequest, SourceFrontier, SourceKey, SourceRecordLocator, StableEntityId,
    StableEntityKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ErrorClass, ProtocolError};

pub const SOURCE_MATERIALIZATION_CONTRACT_VERSION: u16 = 1;
pub const MAX_SOURCE_MANIFEST_SOURCES: usize = 100_000;
pub const MAX_SOURCE_MANIFEST_REMOVALS: usize = 100_000;
pub const MAX_SOURCE_INVENTORY_SOURCES: usize = 100_000;
pub const MAX_SOURCE_PROGRESS_SOURCES: usize = 100_000;
pub const MAX_SOURCE_RECORDS_PER_PAGE: usize = 1_024;
pub const MAX_SOURCE_FACTS_PER_RECORD: usize = 256;
pub const MAX_SOURCE_TOUCHED_FILES_PER_RECORD: usize = 4_096;
pub const MAX_SOURCE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SOURCE_CONTENT_BYTES_PER_PAGE: usize = MAX_SOURCE_CONTENT_BYTES;
pub const MAX_SOURCE_MANIFEST_WIRE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SOURCE_MANIFEST_PAGE_ITEMS: usize = 64;
pub const MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES: usize = MAX_SOURCE_MANIFEST_WIRE_BYTES;
pub const MAX_SOURCE_PROGRESS_PAGE_ITEMS: usize = 64;
pub const MAX_SOURCE_PROGRESS_PAGE_WIRE_BYTES: usize = MAX_SOURCE_MANIFEST_WIRE_BYTES;
pub const MAX_SOURCE_CONTROL_WIRE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_SOURCE_PAGE_WIRE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_SOURCE_IDENTITY_BYTES: usize = 8 * 1024;
pub const MAX_SOURCE_PATH_BYTES: usize = 64 * 1024;

const MAX_SOURCE_ENCODED_CONTENT_BYTES: usize = MAX_SOURCE_CONTENT_BYTES.div_ceil(3) * 4;

include!("source_materialization/manifest.rs");
include!("source_materialization/records.rs");
include!("source_materialization/lifecycle.rs");
include!("source_materialization/helpers.rs");

#[cfg(test)]
#[path = "source_materialization/tests.rs"]
mod tests;
