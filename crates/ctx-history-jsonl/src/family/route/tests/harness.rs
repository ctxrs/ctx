use super::*;

pub(super) fn revalidate_test_sources(
    root: &Path,
    resident: &Mutex<FamilyResident>,
) -> Result<bool> {
    let sources = resident
        .lock()
        .map_err(|_| CaptureError::SystemInvariant("JSONL test resident lock was poisoned"))?
        .terminal_sources
        .values()
        .map(|evidence| evidence.certificate.clone())
        .collect::<Vec<_>>();
    for source in &sources {
        if !revalidate_target_fallible(
            resident,
            SourceBackedRevalidationTarget::Source(source),
            Some(root),
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn capture_parallel_test_generation(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> (IndexCaptureCommitReceipt, JsonlFamilyScannerActivity) {
    let (writer, _resident, ()) =
        capture_test_generation!(adapter, root, index_root, workers, |resident, sink| {
            capture(adapter, root, resident, sink).unwrap()
        });
    let activity = jsonl_family_scanner_activity();
    let commit = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true).unwrap());
    (commit, activity)
}

pub(super) fn capture_parallel_test_generation_exhaustive_with_terminal_revalidation(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> Result<(IndexCaptureCommitReceipt, JsonlFamilyScannerActivity)> {
    let (writer, resident, ()) = capture_test_generation!(
        adapter,
        root,
        index_root,
        workers,
        SourceBackedReconciliationDemand::Exhaustive,
        |resident, sink| { capture(adapter, root, resident, sink).unwrap() }
    );
    let inventory = resident
        .lock()
        .map_err(|_| CaptureError::SystemInvariant("JSONL test resident lock was poisoned"))?
        .certified_inventory
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "JSONL test capture did not certify an inventory",
        ))?;
    if !revalidate_test_sources(root, &resident)?
        || !revalidate_complete_inventory(adapter, root, &resident, &inventory)?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let activity = jsonl_family_scanner_activity();
    let commit = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true)?);
    Ok((commit, activity))
}

pub(super) fn capture_parallel_test_generation_with_terminal_revalidation(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> Result<(IndexCaptureCommitReceipt, JsonlFamilyScannerActivity)> {
    let (writer, resident, ()) =
        capture_test_generation!(adapter, root, index_root, workers, |resident, sink| {
            capture(adapter, root, resident, sink).unwrap()
        });
    let inventory = resident
        .lock()
        .map_err(|_| CaptureError::SystemInvariant("JSONL test resident lock was poisoned"))?
        .certified_inventory
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "JSONL test capture did not certify an inventory",
        ))?;
    let valid = match revalidate_test_sources(root, &resident).and_then(|valid| {
        if valid {
            revalidate_complete_inventory(adapter, root, &resident, &inventory)
        } else {
            Ok(false)
        }
    }) {
        Ok(valid) => valid,
        Err(error) if error.is_not_found() || error.is_source_changed() => false,
        Err(error) => return Err(error),
    };
    if !valid {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let activity = jsonl_family_scanner_activity();
    let commit = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true)?);
    Ok((commit, activity))
}

pub(super) fn capture_parallel_test_generation_with_resident_and_terminal_revalidation(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
    resident: &Mutex<FamilyResident>,
) -> Result<(IndexCaptureCommitReceipt, JsonlFamilyScannerActivity)> {
    let (writer, ()) = capture_test_generation_with_resident!(
        resident,
        adapter,
        root,
        index_root,
        workers,
        |resident, sink| { capture(adapter, root, resident, sink).unwrap() }
    );
    let inventory = resident
        .lock()
        .map_err(|_| CaptureError::SystemInvariant("JSONL test resident lock was poisoned"))?
        .certified_inventory
        .clone()
        .ok_or(CaptureError::SystemInvariant(
            "JSONL test capture did not certify an inventory",
        ))?;
    if !revalidate_test_sources(root, resident)?
        || !revalidate_complete_inventory(adapter, root, resident, &inventory)?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let activity = jsonl_family_scanner_activity();
    let commit = IndexCaptureCommitReceipt::new(writer.commit(|_| true, |_| true)?);
    Ok((commit, activity))
}

pub(super) fn capture_checkpoint_test_generation(
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> IndexCaptureCommitReceipt {
    capture_parallel_test_generation(&CheckpointTestAdapter::default(), root, index_root, workers).0
}

pub(super) fn run_scheduler_test_capture(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index_root: &Path,
    workers: usize,
) -> SourceBackedRouteResult<JsonlFamilyScannerActivity> {
    let (_writer, _resident, result) = capture_test_generation!(
        adapter,
        root,
        index_root,
        workers,
        |resident, sink| capture(adapter, root, resident, sink)
    );
    result.map(|()| jsonl_family_scanner_activity())
}

pub(super) fn scheduler_test_repository(parent: &Path) -> PathBuf {
    let repository = parent.join("attributed-repository");
    fs::create_dir(&repository).unwrap();
    repository
}

pub(super) fn write_scheduler_test_leaf(root: &Path, partition: u64, phase: usize, ordinal: usize) {
    fs::write(
        root.join(format!(
            "partition-{partition:02}-phase-{phase}-leaf-{ordinal}.jsonl"
        )),
        b"{\"message\":\"scheduler\"}\n",
    )
    .unwrap();
}

pub(super) fn provider_checkpoints(receipt: &IndexCaptureCommitReceipt) -> Vec<Option<TypedKey>> {
    receipt
        .manifest()
        .sources
        .iter()
        .map(|source| {
            let frontier = source.frontier().unwrap();
            FamilyCheckpoint::decode_frontier_key::<CaptureError>(frontier.checkpoint())
                .unwrap()
                .provider_checkpoint
        })
        .collect()
}

pub(super) fn prepare_semantic_lifecycle_test(
    adapter: &SemanticLifecycleTestAdapter,
    root: &Path,
    index_root: &Path,
    base: Option<&CertifiedSource>,
    publications: &mut Vec<(bool, u64, usize)>,
) -> Result<leaf::PreparedLeaf<CaptureError>> {
    let inventory = adapter.discover(root)?;
    let leaf = inventory
        .accepted_leaves()
        .next()
        .ok_or(CaptureError::SystemInvariant(
            "semantic lifecycle test has no leaf",
        ))?;
    let writer = match TestLifecycle::open(index_root, ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |event| {
        if let JsonlLeafOutputEvent::Page {
            append,
            completed_bytes,
            records,
        } = event
        {
            publications.push((append, completed_bytes, records.len()));
        }
        Ok(())
    };
    prepare_leaf(
        adapter,
        leaf,
        base,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut JsonlLeafOutput::new(&mut emit),
        true,
    )
}
