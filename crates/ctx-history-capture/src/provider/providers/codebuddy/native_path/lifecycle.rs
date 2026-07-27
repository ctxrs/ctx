use super::*;

pub(super) fn retire_missing_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    known: &[KnownRoute],
    live: &[CodeBuddySource],
    work_limit: CaptureWorkLimit,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let live = live
        .iter()
        .map(|source| source.locator_identity.as_str())
        .collect::<BTreeSet<_>>();
    let missing = known
        .iter()
        .filter(|route| !live.contains(route.locator_identity.as_str()))
        .collect::<Vec<_>>();
    let mut summary = ProviderImportSummary::default();
    for (index, route) in missing.iter().enumerate() {
        let route_summary = retire_route(store, bulk_guard, context, route, reason)?;
        let changed = route_summary.work_result() == ProviderImportWorkResult::Changed;
        summary.merge_from(route_summary);
        if work_limit == CaptureWorkLimit::OneSafeGroup && changed {
            summary.work_remaining = index.saturating_add(1) < missing.len();
            break;
        }
    }
    Ok(summary)
}

pub(super) fn retire_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &route.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy route retirement lost its cursor",
        ))?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::CodeBuddy,
        source_format: CODEBUDDY_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor_stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: route.cursor_stream.clone(),
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    if changed {
        summary.skipped = 1;
        summary.skipped_sessions = 1;
        summary.set_work_result(ProviderImportWorkResult::Changed);
    } else {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(CODEBUDDY_RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("codebuddy-retirement:{}", hex(&digest.finalize()))
}
