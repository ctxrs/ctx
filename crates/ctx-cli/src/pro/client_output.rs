use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{anyhow, bail, Result};
use ctx_history_core::{CertifiedSource, SourceFrontier, SourceKey};
use ctx_pro_host_protocol::{Capability, HelperMessage, HostMessage};

use super::{protocol_error, ProClient, BATCH_TIMEOUT};

#[path = "source_backed_pro_provider.rs"]
mod source_backed_pro_provider;

/// Synchronizes Pro from one already-published, generation-pinned Core source manifest.
///
/// Exact canonical content is hydrated through the supplied source resolver.
/// Any helper or hydration failure leaves Core intact and Pro independently retryable.
pub(crate) fn sync_source_manifest_materialization(
    data_root: &Path,
    manifest: ctx_pro_host_protocol::SourceManifest,
    index: &ctx_history_index::VerifiedIndex,
    resolver: &ctx_history_capture::SourceBackedResolverRegistry,
) -> Result<ctx_pro_host_protocol::SourceManifestReceipt> {
    source_backed_pro_provider::sync_generation_pinned_source_manifest(
        data_root, manifest, index, resolver,
    )
    .map(|report| report.receipt)
}

/// Deferred Pro catch-up over the authoritative source-backed wire contract.
///
/// Core is already published before this coordinator runs. Any helper or
/// hydration failure therefore leaves Core intact and Pro retryable from its
/// independently committed per-source progress.
#[path = "client_output/source_backed_feed.rs"]
mod source_backed_feed;

#[cfg(test)]
#[path = "client_output/tests.rs"]
mod tests;
