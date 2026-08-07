use std::path::Path;

use ctx_history_index::{GenerationWriter, SourceRouteIdentity, VerifiedIndex, WriterOptions};
use ctx_history_refresh::{
    SourceBackedRefreshCurrent, SourceBackedRefreshReceipt, SourceBackedRefreshRouteResult,
    SourceBackedZeroSourceAuthority, SourceBackedZeroSourceAuthorityKind,
};
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub(super) enum EmptyPublicationAuthority {
    AuthoritativeV2,
    LegacyV1,
    Missing,
    Malformed,
    UnknownVersion,
}

pub(super) fn publish_empty_generation(
    data_root: &Path,
    authority: EmptyPublicationAuthority,
) -> String {
    let index_root = data_root.join("search/lexical");
    let generation_id = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap()
        .generation_id;
    if matches!(authority, EmptyPublicationAuthority::Missing) {
        return generation_id;
    }
    let metadata = match authority {
        EmptyPublicationAuthority::AuthoritativeV2 => {
            publication_metadata(&generation_id, 2, 0, true)
        }
        EmptyPublicationAuthority::LegacyV1 => publication_metadata(&generation_id, 1, 0, false),
        EmptyPublicationAuthority::Malformed => b"{".to_vec(),
        EmptyPublicationAuthority::UnknownVersion => {
            publication_metadata(&generation_id, 99, 0, true)
        }
        EmptyPublicationAuthority::Missing => unreachable!("missing metadata returned above"),
    };
    republish_metadata(&index_root, &generation_id, metadata);
    generation_id
}

pub(super) fn republish_active_as_legacy_v1(data_root: &Path) -> String {
    let index_root = data_root.join("search/lexical");
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let generation_id = index.generation_id().to_owned();
    let source_count = index.manifest().sources.len();
    assert!(
        source_count > 0,
        "legacy compatibility fixture must be nonempty"
    );
    drop(index);
    republish_metadata(
        &index_root,
        &generation_id,
        publication_metadata(&generation_id, 1, source_count, false),
    );
    generation_id
}

fn publication_metadata(
    generation_id: &str,
    version: u64,
    source_count: usize,
    authoritative_empty: bool,
) -> Vec<u8> {
    let route_identity = "ab".repeat(32);
    let route = SourceRouteIdentity::from_sha256(route_identity.clone()).unwrap();
    let receipt = SourceBackedRefreshReceipt {
        previous_generation: None,
        published_generation: generation_id.to_owned(),
        generation_changed: true,
        published_explicit_source_catalog: None,
        current: SourceBackedRefreshCurrent {
            source_count,
            ..SourceBackedRefreshCurrent::default()
        },
        route_results: vec![SourceBackedRefreshRouteResult::succeeded(
            route_identity,
            true,
        )],
        zero_source_authority: authoritative_empty
            .then(|| SourceBackedZeroSourceAuthority {
                generation_id: generation_id.to_owned(),
                route_identity: route,
                kind: SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
            })
            .into_iter()
            .collect(),
        catalog_route_bindings: Vec::new(),
    };
    let mut receipt = receipt.to_json();
    if version == 1 {
        receipt
            .as_object_mut()
            .unwrap()
            .remove("zero_source_authority");
    }
    serde_json::to_vec(&json!({
        "version": version,
        "request_id": "query-authority-test",
        "operation": "refresh",
        "refresh_scope": {"kind": "all"},
        "receipt": receipt,
        "route_observations": [Value::Null],
    }))
    .unwrap()
}

fn republish_metadata(index_root: &Path, generation_id: &str, metadata: Vec<u8>) {
    GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .republish_current_publication_metadata(generation_id, metadata)
        .unwrap();
}
