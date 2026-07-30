use std::path::Path;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, SourceRecordLocator,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    continue_history_item_text, decode_hex_digest, discover_continue_root,
    validate_continue_locator, ContinueNativePathError, ContinueSourceBackedError,
    ContinueSourceBackedResult,
};
use crate::provider::source_backed::hydration_failure;

use crate::provider::providers::continue_cli::native_path::parse::{
    locate_continue_exact_history_items, ContinueExactHistoryLookup,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

pub(crate) fn hydrate_continue_source_backed_record(
    selected_sessions_root: impl AsRef<Path>,
    locator: &SourceRecordLocator,
) -> ContinueSourceBackedResult<ContinueHydratedSourceRecord> {
    let mut records =
        hydrate_continue_locators(selected_sessions_root.as_ref(), &[locator], || {})?;
    records
        .pop()
        .ok_or(ContinueSourceBackedError::LocatorRecordMissing)
}

pub(super) fn hydrate_continue_group_with_observer(
    root: &Path,
    request: &BatchHydrationRequest,
    observe_parse: impl FnMut(),
) -> Result<BatchHydrationResult, HydrationFailure> {
    if request.is_empty() {
        return BatchHydrationResult::new(Vec::new())
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error));
    }
    let expected_source = request
        .events()
        .first()
        .map(|event| event.locator().source().clone())
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Continue hydration group was unexpectedly empty",
            )
        })?;
    if request.events().iter().any(|event| {
        !event
            .locator()
            .source()
            .exact_descriptor_eq(&expected_source)
    }) {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Continue hydration group spans more than one exact source",
        ));
    }
    let locators = request
        .events()
        .iter()
        .map(|event| event.locator())
        .collect::<Vec<_>>();
    let records = if let [locator] = locators.as_slice() {
        vec![hydrate_continue_source_backed_record(root, locator)
            .map_err(continue_hydration_failure)?]
    } else {
        hydrate_continue_locators(root, &locators, observe_parse)
            .map_err(continue_hydration_failure)?
    };
    let hydrated = request
        .events()
        .iter()
        .zip(records)
        .map(|(event, record)| HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes: record.provider_bytes,
        })
        .collect();
    BatchHydrationResult::new(hydrated)
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
}

fn hydrate_continue_locators(
    root: &Path,
    locators: &[&SourceRecordLocator],
    mut observe_parse: impl FnMut(),
) -> ContinueSourceBackedResult<Vec<ContinueHydratedSourceRecord>> {
    let mut expected_session = None;
    let mut expected_revision = None;
    let mut ordinals = Vec::with_capacity(locators.len());
    for locator in locators {
        locator.validate_contract()?;
        let (session, ordinal, revision) = validate_continue_locator(locator)?;
        if expected_session
            .as_ref()
            .is_some_and(|expected| expected != &session)
            || expected_revision
                .as_ref()
                .is_some_and(|expected| expected != &revision)
        {
            return Err(ContinueSourceBackedError::InvalidLocator);
        }
        expected_session = Some(session);
        expected_revision = Some(revision);
        ordinals.push(ordinal);
    }
    let expected_session = expected_session.ok_or(ContinueSourceBackedError::InvalidLocator)?;
    let expected_revision = expected_revision.ok_or(ContinueSourceBackedError::InvalidLocator)?;
    let discovery = discover_continue_root(root)?;
    let (leaves, authority) = discovery.into_parts();
    let expected_tree = authority.tree_fingerprint();
    let mut resolved = None;
    for leaf in &leaves {
        let snapshot = authority.open_source(leaf)?;
        let observed_revision = decode_hex_digest(snapshot.observation().session_revision())
            .ok_or(ContinueSourceBackedError::InvalidRevisionEvidence)?;
        if observed_revision != expected_revision {
            continue;
        }
        observe_parse();
        let items = match locate_continue_exact_history_items(
            snapshot.bytes(),
            &expected_session,
            &ordinals,
        )
        .map_err(ContinueSourceBackedError::ExactResolver)?
        {
            ContinueExactHistoryLookup::DifferentSession => {
                return Err(ContinueSourceBackedError::InvalidLocator);
            }
            ContinueExactHistoryLookup::MissingItem => {
                return Err(ContinueSourceBackedError::LocatorRecordMissing);
            }
            ContinueExactHistoryLookup::Items(items) => items,
        };
        if resolved.is_some() {
            return Err(ContinueSourceBackedError::AmbiguousLocatorSource);
        }
        let records = locators
            .iter()
            .zip(items)
            .map(|(locator, item)| hydrate_continue_item(locator, item))
            .collect::<ContinueSourceBackedResult<Vec<_>>>()?;
        resolved = Some(records);
    }
    let terminal_tree = authority.revalidate_fingerprint()?.ok_or_else(|| {
        ContinueNativePathError::SourceChanged {
            path: root.to_path_buf(),
        }
    })?;
    if terminal_tree != expected_tree {
        return Err(ContinueNativePathError::SourceChanged {
            path: root.to_path_buf(),
        }
        .into());
    }
    resolved.ok_or(ContinueSourceBackedError::LocatorSourceRevisionNotFound)
}

fn hydrate_continue_item(
    locator: &SourceRecordLocator,
    provider_item: &[u8],
) -> ContinueSourceBackedResult<ContinueHydratedSourceRecord> {
    let actual_digest: [u8; 32] = Sha256::digest(provider_item).into();
    if &actual_digest != locator.record_digest() {
        return Err(ContinueSourceBackedError::LocatorDigestMismatch);
    }
    let value: Value = serde_json::from_slice(provider_item)?;
    let decoded_display_text = continue_history_item_text(&value)
        .ok_or(ContinueSourceBackedError::LocatorRecordMissing)?;
    Ok(ContinueHydratedSourceRecord {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text: Some(decoded_display_text),
    })
}

fn continue_hydration_failure(error: ContinueSourceBackedError) -> HydrationFailure {
    let kind = match &error {
        ContinueSourceBackedError::InvalidLocator
        | ContinueSourceBackedError::Projection(_)
        | ContinueSourceBackedError::Resolver(_) => HydrationFailureKind::InvalidLocator,
        ContinueSourceBackedError::LocatorRecordMissing => HydrationFailureKind::MissingRecord,
        ContinueSourceBackedError::LocatorSourceRevisionNotFound
        | ContinueSourceBackedError::AmbiguousLocatorSource
        | ContinueSourceBackedError::LocatorDigestMismatch
        | ContinueSourceBackedError::InvalidRevisionEvidence => {
            HydrationFailureKind::StaleRecordEvidence
        }
        ContinueSourceBackedError::Native(_)
        | ContinueSourceBackedError::Json(_)
        | ContinueSourceBackedError::ExactResolver(_)
        | ContinueSourceBackedError::SessionChanged
        | ContinueSourceBackedError::MissingSourceAuthority
        | ContinueSourceBackedError::OverlappingSource
        | ContinueSourceBackedError::UnterminatedSource
        | ContinueSourceBackedError::CountMismatch
        | ContinueSourceBackedError::CountOverflow => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(kind, error)
}
