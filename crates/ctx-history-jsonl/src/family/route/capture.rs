use super::*;

pub(super) fn default_base_source_path<R: JsonlFamilyRuntime>(
    _adapter: &(impl JsonlFamilyAdapter<Runtime = R> + ?Sized),
    certificate: &CertifiedSource,
) -> JsonlResult<PathBuf, JsonlRuntimeError<R>> {
    certificate
        .validate_contract()
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    // Parser revisions govern projection semantics, not source ownership. The
    // family still needs the prior source path so an unchanged source can be
    // selected and replaced under the current parser rather than rejected.
    let frontier = certificate.frontier().ok_or_else(|| {
        JsonlRuntimeError::<R>::invalid_payload("JSONL base frontier is absent".to_owned())
    })?;
    // This lookup recovers ownership only; it does not authorize continuation.
    // Decode a structurally current checkpoint even when its outer kind is a
    // retired value so the source can be selected for a conservative full
    // replacement. `decode_checkpoint` separately requires the current kind
    // before granting no-op or suffix authority.
    let checkpoint =
        FamilyCheckpoint::decode_frontier_key::<JsonlRuntimeError<R>>(frontier.checkpoint())?;
    if checkpoint.physical.identity().source_descriptor_digest()
        != &certificate.observation().source().exact_descriptor_digest()
    {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base checkpoint source changed".to_owned(),
        ));
    }
    Ok(checkpoint.physical.identity().source_path().clone())
}

fn disposition_terminal_proof<R: JsonlFamilyRuntime>(
    opening: &JsonlFamilyInventory<JsonlRuntimeError<R>>,
    source_path: &Path,
    authority_path: &Path,
    observation: &JsonlFileObservation,
) -> JsonlResult<JsonlFamilyTerminalProof<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    JsonlFamilyTerminalProof::exact_admitted_path(
        source_path.to_path_buf(),
        Arc::clone(exact_member_authority(
            &opening.authorities,
            source_path,
            authority_path,
        )?),
        authority_path.to_path_buf(),
        observation,
    )
}

pub fn jsonl_family_driver<R: JsonlFamilyRuntime>(
    adapter: Arc<dyn JsonlFamilyAdapter<Runtime = R>>,
    root: PathBuf,
) -> super::super::JsonlRuntimeDriver<R> {
    let resident = Arc::new(Mutex::new(FamilyResident::<JsonlRuntimeError<R>>::default()));
    let scan_adapter = Arc::clone(&adapter);
    let scan_root = root.clone();
    let scan_resident = Arc::clone(&resident);
    let owns_adapter = Arc::clone(&adapter);
    let owns_resident = Arc::clone(&resident);
    let revalidation_resident = Arc::clone(&resident);
    let terminal_adapter = adapter;
    let terminal_root = root;
    let inventory_resident = Arc::clone(&resident);

    super::super::JsonlRuntimeDriver::<R>::new(
        move |sink| capture(&*scan_adapter, &scan_root, &scan_resident, sink),
        move |source| {
            owns_adapter.owns(source)
                && owns_resident.lock().is_ok_and(|resident| {
                    !resident.ownership_initialized
                        || resident
                            .owned_sources
                            .get(&source.exact_descriptor_digest())
                            .is_some_and(|owned| owned.exact_descriptor_eq(source))
                        || resident
                            .quarantined_sources
                            .get(&source.exact_descriptor_digest())
                            .is_some_and(|owned| owned.exact_descriptor_eq(source))
                })
        },
        move |target| revalidate_target(&revalidation_resident, target),
    )
    .with_parallel_leaf_workers()
    .with_fallible_complete_inventory_revalidation(move |expected| {
        match revalidate_complete_inventory(
            terminal_adapter.as_ref(),
            &terminal_root,
            &inventory_resident,
            expected,
        ) {
            Ok(revalidated) => Ok(revalidated),
            Err(error)
                if normalized_jsonl_error_kind(&error)
                    .unwrap_or_else(|| terminal_adapter.scan_error_kind(&error))
                    == SourceBackedRouteErrorKind::SourceChanged =>
            {
                Ok(false)
            }
            Err(error) => Err(route_scan(terminal_adapter.as_ref(), error)),
        }
    })
}

/// Restores an authenticatable ctime/ChangeTime ambiguity to the retained
/// receipt observation. A certificate-bound resident observation may advance
/// the task-local terminal proof after one successful content authentication;
/// otherwise the terminal witness must authenticate the exact admitted EOF.
fn normalize_authenticated_change_time_hints<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &mut JsonlFamilyInventory<JsonlRuntimeError<R>>,
    bases: &HashMap<[u8; 32], &CertifiedSource>,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
) -> JsonlResult<HashMap<[u8; 32], JsonlFileObservation>, JsonlRuntimeError<R>> {
    let authenticated = resident
        .lock()
        .map_err(|_| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL resident catalog lock was poisoned".to_owned(),
            )
        })?
        .authenticated_source_observations
        .clone();
    let mut reusable = HashMap::new();
    let mut normalized = false;
    for member in &mut opening.members {
        let JsonlFamilyInventoryMember::Accepted { leaf, .. } = member else {
            continue;
        };
        let Some(base) = base_for_leaf(bases, leaf) else {
            continue;
        };
        let Ok(checkpoint) = decode_checkpoint(adapter, leaf, base) else {
            continue;
        };
        let retained = checkpoint.physical.source_observation();
        if retained.differs_only_by_change_identity(&leaf.observation)
            && checkpoint.authenticates_admitted_eof()
        {
            let digest = leaf.source().exact_descriptor_digest();
            if authenticated.get(&digest).is_some_and(|authenticated| {
                authenticated.certificate == *base && authenticated.observation == leaf.observation
            }) {
                reusable.insert(digest, leaf.observation.clone());
            }
            leaf.observation = retained.clone();
            normalized = true;
        }
    }
    if normalized {
        opening.rebuild_observation()?;
    }
    Ok(reusable)
}

pub(super) fn capture<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    root: &Path,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
) -> SourceBackedRouteResult<()> {
    if let Some(members) = sink.member_workset().cloned() {
        let partial_captured = capture_partial_members(adapter, root, resident, sink, &members)?;
        if partial_captured {
            return Ok(());
        }
        adapter
            .prepare_partial_member_fallback()
            .map_err(|error| route_discovery(adapter, error))?;
    }
    reset_terminal(resident)?;
    let mut opening = adapter
        .discover(root)
        .map_err(|error| route_discovery(adapter, error))?;
    classify_incomplete_first_records(adapter, &mut opening)
        .map_err(|error| route_discovery(adapter, error))?;
    if opening.root_missing()
        && adapter.root_missing_mode() == JsonlFamilyRootMissingMode::Unavailable
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "provider JSONL root is unavailable",
        ));
    }
    let bases = base_sources_for_route(adapter, sink)?;
    bind_prior_disposition_sources(adapter, &mut opening, &bases)?;
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let authenticated_change_time_hints = normalize_authenticated_change_time_hints(
        adapter,
        &mut opening,
        &bases_by_descriptor,
        resident,
    )
    .map_err(|error| route_scan(adapter, error))?;
    let opening_membership = adapter
        .observe_terminal_membership(root, &opening)
        .map_err(|error| route_discovery(adapter, error))?;
    let (route_local_quarantined_count, route_local_pending_count) =
        route_local_disposition_counts::<R>(&opening, sink);
    // A rejected member with exact quarantine ownership belongs to a
    // source-local failure even when a peer member owns the one diagnostic.
    let route_fatal_rejected = opening
        .quarantined_leaves()
        .filter(|leaf| leaf.logical_source_failure.is_none() && leaf.quarantined_source.is_none())
        .collect::<Vec<_>>();
    if opening.accepted_len() == 0 && !route_fatal_rejected.is_empty() && bases.is_empty() {
        let rejected_records = route_fatal_rejected.iter().try_fold(0_u64, |total, leaf| {
            total.checked_add(leaf.rejected_records).ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "provider JSONL rejected-record count overflow",
                )
            })
        })?;
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "direct JSONL route rejected {rejected_records} records across {} sources; \
                 all provider-native session identity leaves were rejected",
                route_fatal_rejected.len(),
            ),
        ));
    }
    for rejected in opening.quarantined_leaves() {
        if let Some((source, detail)) = &rejected.logical_source_failure {
            if !quarantined_member_is_route_local::<R>(rejected, sink) {
                continue;
            }
            let failure =
                SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, detail);
            let carried_forward = bases
                .iter()
                .any(|base| base.observation().source().exact_descriptor_eq(source));
            if adapter.provider() == CaptureProvider::Codex {
                sink.record_logical_source_quarantine(source.clone(), failure)
            } else {
                sink.record_logical_source_failure(source.clone(), failure, carried_forward)
            }
            .map_err(route_internal)?;
        }
    }
    let mut rejected_quarantine_sources = HashMap::new();
    for rejected in opening.quarantined_leaves() {
        if let Some(source) = rejected.source() {
            if sink.source_owned_by_other_route(source) {
                continue;
            }
            if rejected_quarantine_sources
                .insert(source.exact_descriptor_digest(), source.clone())
                .is_some_and(|previous: SourceKey| !previous.exact_descriptor_eq(source))
            {
                return Err(route_invalid(
                    "rejected JSONL quarantine source descriptor digest collision",
                ));
            }
        }
    }
    if opening.accepted_len() == 0 && route_local_pending_count != 0 && bases.is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::SourceChanged,
            format!(
                "provider JSONL inventory contains {} incomplete sources and no complete source; retry after a pending file changes",
                route_local_pending_count,
            ),
        ));
    }
    let mut selected_leaves = opening
        .accepted_leaves()
        .filter(|leaf| !sink.source_owned_by_other_route(leaf.source()))
        .cloned()
        .collect::<Vec<_>>();
    adapter
        .order_leaf_scans(&mut selected_leaves)
        .map_err(|error| route_scan(adapter, error))?;
    let exact_scan_total_bytes = selected_leaves.iter().try_fold(0_u64, |total, leaf| {
        total.checked_add(leaf.frozen_scan_observation()?.length())
    });
    // Only leaves whose existing capture contract already freezes the opening
    // observation opt in. Overflow or any other family shape simply abstains.
    if let Some(total) = exact_scan_total_bytes {
        sink.enable_exact_scan_accounting(total);
        if selected_leaves.is_empty() {
            sink.report_completed_bytes_with_exact(0, Some(0))
                .map_err(route_internal)?;
        }
    }
    let base_event_lookup = sink.base_event_lookup();
    let mut scan_selected_leaves = Vec::with_capacity(selected_leaves.len());
    let mut retained_terminal_sources = HashMap::new();
    #[cfg(test)]
    tests::begin_admission(selected_leaves.len(), bases.len());
    let append_only_trust_allowed = sink.reconciliation_demand()
        == ctx_history_capture_runtime::SourceBackedReconciliationDemand::Incremental;
    for leaf in &selected_leaves {
        let Some(base) = base_for_leaf(&bases_by_descriptor, leaf) else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        let Ok(observation) =
            source_observation::<JsonlRuntimeError<R>>(leaf.source(), leaf.observation())
        else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        if observation != *base.observation() {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        #[cfg(test)]
        let decoded = decode_checkpoint(adapter, leaf, base)
            .inspect_err(|_| tests::record_checkpoint_rejection());
        #[cfg(not(test))]
        let decoded = decode_checkpoint(adapter, leaf, base);
        let Ok(checkpoint) = decoded else {
            scan_selected_leaves.push(leaf.clone());
            continue;
        };
        if !checkpoint.physical.terminal() && !checkpoint.authenticates_admitted_eof() {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        if !append_only_trust_allowed
            && adapter.append_trust_contract()
                == JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
        {
            scan_selected_leaves.push(leaf.clone());
            continue;
        }
        let digest = leaf.source().exact_descriptor_digest();
        let terminal_proof =
            if let Some(authenticated) = authenticated_change_time_hints.get(&digest) {
                let mut proof_leaf = leaf.clone();
                proof_leaf.observation = authenticated.clone();
                JsonlFamilyTerminalProof::unchanged(adapter, &proof_leaf, base, &checkpoint, true)
            } else {
                JsonlFamilyTerminalProof::unchanged(adapter, leaf, base, &checkpoint, false)
            }
            .map_err(|error| route_scan(adapter, error))?;
        sink.retain_source(base.clone()).map_err(route_internal)?;
        sink.report_completed_bytes_with_exact(
            base.counts().certified_bytes,
            leaf.frozen_scan_observation()
                .map(|observation| observation.length()),
        )
        .map_err(route_internal)?;
        retained_terminal_sources.insert(
            digest,
            TerminalSourceEvidence {
                certificate: base.clone(),
                terminal_certificate: None,
                terminal_proof,
                emitted_bytes: 0,
                exact_scan_bytes: leaf
                    .frozen_scan_observation()
                    .map(|observation| observation.length()),
                record_rejections: SourceBackedRecordRejectionDrafts::default(),
            },
        );
    }
    #[cfg(test)]
    tests::record_retained_sources(retained_terminal_sources.len());
    let terminal_sources = scan_leaves(
        adapter,
        &scan_selected_leaves,
        &bases_by_descriptor,
        base_event_lookup,
        sink,
        append_only_trust_allowed,
    );
    let finish_leaf_scans = adapter
        .finish_leaf_scans()
        .map_err(|error| route_scan(adapter, error));
    let mut scan_result = terminal_sources?;
    finish_leaf_scans?;
    let mut disposition_dependencies = opening
        .members()
        .iter()
        .filter_map(|member| match member {
            JsonlFamilyInventoryMember::Quarantined { leaf, .. } => {
                leaf.observation.as_ref().map(|observation| {
                    (
                        leaf.source_path.as_path(),
                        leaf.authority_path.as_path(),
                        observation,
                    )
                })
            }
            JsonlFamilyInventoryMember::Pending { leaf, .. } => Some((
                leaf.source_path.as_path(),
                leaf.authority_path.as_path(),
                &leaf.observation,
            )),
            JsonlFamilyInventoryMember::Accepted { .. } => None,
        })
        .map(|(source_path, authority_path, observation)| {
            disposition_terminal_proof::<R>(&opening, source_path, authority_path, observation)
        })
        .collect::<JsonlResult<Vec<_>, _>>()
        .map_err(|error| route_scan(adapter, error))?;
    for quarantined in &scan_result.quarantined {
        if !opening.accepted_leaves().any(|leaf| {
            leaf.source_path() == quarantined.source_path
                && leaf
                    .source()
                    .exact_descriptor_eq(&quarantined.claimed_source)
        }) {
            return Err(route_invalid(
                "scanned JSONL quarantine is not bound to an accepted physical member",
            ));
        }
        quarantined
            .terminal_proof
            .revalidate_dependency()
            .map_err(|error| route_scan(adapter, error))?;
        disposition_dependencies.push(quarantined.terminal_proof.clone());
        let failure = SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            &quarantined.detail,
        );
        let carried_forward = bases.iter().any(|base| {
            base.observation()
                .source()
                .exact_descriptor_eq(&quarantined.failure_source)
        });
        if adapter.provider() == CaptureProvider::Codex {
            sink.record_logical_source_quarantine(quarantined.failure_source.clone(), failure)
        } else {
            sink.record_logical_source_failure(
                quarantined.failure_source.clone(),
                failure,
                carried_forward,
            )
        }
        .map_err(route_internal)?;
    }
    let mut quarantined_source_ownership = rejected_quarantine_sources;
    for leaf in &scan_result.quarantined {
        if quarantined_source_ownership
            .insert(
                leaf.claimed_source.exact_descriptor_digest(),
                leaf.claimed_source.clone(),
            )
            .is_some_and(|previous| !previous.exact_descriptor_eq(&leaf.claimed_source))
        {
            return Err(route_invalid(
                "scanned JSONL quarantine source descriptor digest collision",
            ));
        }
    }
    let quarantined_sources = quarantined_source_ownership
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut owned_sources = HashMap::with_capacity(bases.len() + selected_leaves.len());
    for source in bases
        .iter()
        .map(|base| base.observation().source())
        .chain(selected_leaves.iter().map(JsonlFamilyLeaf::source))
    {
        if quarantined_sources.contains(&source.exact_descriptor_digest()) {
            continue;
        }
        let digest = source.exact_descriptor_digest();
        if owned_sources
            .insert(digest, source.clone())
            .is_some_and(|previous| !previous.exact_descriptor_eq(source))
        {
            return Err(route_invalid(
                "JSONL route source descriptor digest collision",
            ));
        }
    }
    let mut terminal_sources = std::mem::take(&mut scan_result.terminal_sources);
    for (digest, evidence) in retained_terminal_sources {
        if terminal_sources.insert(digest, evidence).is_some() {
            return Err(route_invalid("duplicate JSONL terminal source evidence"));
        }
    }

    let partial_inventory = route_local_quarantined_count != 0
        || route_local_pending_count != 0
        || !scan_result.quarantined.is_empty();
    if partial_inventory && !bases.is_empty() {
        for dependency in &disposition_dependencies {
            dependency
                .revalidate_dependency()
                .map_err(|error| route_scan(adapter, error))?;
        }
        // A non-accepted physical member makes absence unprovable. Publish
        // trustworthy accepted peers as a partial route update and carry every
        // unstaged base member until a later clean inventory can certify
        // deletion. This is the same retained-generation safety contract used
        // by bounded exact-member refreshes.
        sink.retain_unstaged_base_route_sources()
            .map_err(route_internal)?;
        let mut resident = resident
            .lock()
            .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
        resident.ownership_initialized = false;
        resident.owned_sources = owned_sources;
        resident.quarantined_sources = quarantined_source_ownership;
        resident.replace_terminal_sources(terminal_sources);
        resident.absent_sources.clear();
        resident.opening_membership = None;
        resident.certified_inventory = None;
        resident.opening_inventory = None;
        return Ok(());
    }

    let selected_sources = selected_leaves
        .iter()
        .filter(|leaf| !quarantined_sources.contains(&leaf.source().exact_descriptor_digest()))
        .map(|leaf| leaf.source().clone())
        .collect::<Vec<_>>();
    let opening = opening.with_appended_exact_dependencies(disposition_dependencies);
    let inventory = opening
        .certify_selected_against(&opening, selected_sources)
        .map_err(route_invalid)?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_internal)?;
    let mut absent_sources = Vec::new();
    for base in &bases {
        if !inventory.contains(base.observation().source()) {
            let base_path = adapter
                .base_source_path(base)
                .map_err(|error| route_scan(adapter, error))?;
            // An identity/parser migration may retire one descriptor while a
            // newly certified descriptor owns the same admitted path. Its
            // terminal source proof and the complete inventory are the fence;
            // requiring pathname absence would incorrectly reject the atomic
            // replacement. Truly unrepresented base paths still need an
            // explicit absence witness.
            if let Some(absent) = retirement_absence_dependency(
                adapter,
                &opening,
                &inventory,
                &terminal_sources,
                base.observation().source(),
                &base_path,
            ) {
                absent_sources.push(absent);
            }
            let deletion = CertifiedSourceDeletion::from_inventory(
                base.observation().source().clone(),
                &inventory,
            )
            .map_err(route_invalid)?;
            sink.delete_source(deletion, inventory.clone())
                .map_err(route_internal)?;
        }
    }
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    resident.ownership_initialized = true;
    resident.owned_sources = owned_sources;
    resident.quarantined_sources = quarantined_source_ownership;
    resident.replace_terminal_sources(terminal_sources);
    resident.absent_sources = absent_sources;
    resident.opening_membership = Some(opening_membership);
    resident.certified_inventory = Some(inventory);
    resident.opening_inventory = Some(opening);
    Ok(())
}

/// Applies the shared physical JSONL contract before any provider semantic
/// classification. A nonempty first record without its framing terminator is
/// pending/incomplete; zero-byte files and complete malformed records retain
/// their existing provider semantics.
pub(super) fn classify_incomplete_first_records<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &mut JsonlFamilyInventory<JsonlRuntimeError<R>>,
) -> JsonlResult<(), JsonlRuntimeError<R>> {
    if opening.root_missing {
        return Ok(());
    }
    let mut changed = false;
    let mut classified = Vec::with_capacity(opening.members.len());
    for member in std::mem::take(&mut opening.members) {
        match member {
            JsonlFamilyInventoryMember::Accepted { identity, leaf }
                if !leaf.whole_record
                    && first_record_is_incomplete(
                        &leaf.source_path,
                        &leaf.authority,
                        &leaf.authority_path,
                        &leaf.observation,
                        adapter.physical_encoding(&leaf),
                        adapter.record_framing(),
                        leaf.frozen_scan_observation().is_some(),
                    )? =>
            {
                changed = true;
                classified.push(JsonlFamilyInventoryMember::Pending {
                    identity,
                    leaf: JsonlFamilyPendingLeaf::bind_observed(
                        leaf.source_path,
                        leaf.authority_path,
                        leaf.observation,
                        leaf.binding,
                        Some(leaf.source),
                    ),
                });
            }
            JsonlFamilyInventoryMember::Quarantined { identity, leaf } => {
                // Provider classification wins once a member owns a diagnosed
                // source failure; a framing probe must not erase that outcome.
                let incomplete = if leaf.logical_source_failure.is_some() {
                    false
                } else if let Some(observation) = &leaf.observation {
                    let authority = exact_member_authority(
                        &opening.authorities,
                        &leaf.source_path,
                        &leaf.authority_path,
                    )?;
                    first_record_is_incomplete(
                        &leaf.source_path,
                        authority,
                        &leaf.authority_path,
                        observation,
                        leaf.physical_encoding,
                        adapter.record_framing(),
                        false,
                    )?
                } else {
                    false
                };
                if incomplete {
                    changed = true;
                    classified.push(JsonlFamilyInventoryMember::Pending {
                        identity,
                        leaf: JsonlFamilyPendingLeaf::bind_observed(
                            leaf.source_path,
                            leaf.authority_path,
                            leaf.observation
                                .expect("incomplete quarantined leaf has an admitted observation"),
                            leaf.proof,
                            leaf.quarantined_source,
                        ),
                    });
                } else {
                    classified.push(JsonlFamilyInventoryMember::Quarantined { identity, leaf });
                }
            }
            member => classified.push(member),
        }
    }
    opening.members = classified;
    if changed {
        opening.rebuild_observation()?;
    }
    Ok(())
}

fn first_record_is_incomplete<E: JsonlFamilyError>(
    source_path: &Path,
    authority: &Arc<ProviderSourceRoot<E>>,
    authority_path: &Path,
    expected: &JsonlFileObservation,
    encoding: JsonlPhysicalEncoding,
    framing: JsonlRecordFraming,
    freeze_observation_at_scan: bool,
) -> JsonlResult<bool, E> {
    let opened = authority.open_file(authority_path)?;
    let current = if freeze_observation_at_scan {
        observe_opened_file_allow_append(source_path, &opened)?
    } else {
        observe_opened_file(source_path, &opened)?
    };
    // Frozen-scan leaves already bind publication to the discovery-time
    // observation. Classify their first record inside that bound while later
    // scan and terminal proofs authenticate the prefix; newly appended bytes
    // belong to the next refresh. Exact leaves retain exact preflight behavior.
    if (freeze_observation_at_scan && !expected.admits_frozen_prefix_in(&current))
        || (!freeze_observation_at_scan && expected != &current)
    {
        return Err(E::source_changed());
    }
    let observation = expected;
    if observation.length() == 0 {
        opened.revalidate_same_object()?;
        return Ok(false);
    }
    let mut file = opened.reopen_same_object()?;
    if encoding == JsonlPhysicalEncoding::RawJsonl && observation.length() >= 4 {
        let mut magic = [0_u8; 4];
        file.read_exact(&mut magic)?;
        file.seek(SeekFrom::Start(0))?;
        if magic == [0x28, 0xb5, 0x2f, 0xfd] {
            // A quarantined member does not necessarily have a provider leaf
            // from which to ask for encoding. Never reinterpret an ordinary
            // Zstandard stream as an unfinished raw JSONL record.
            opened.revalidate_same_object()?;
            return Ok(false);
        }
    }
    let mut stream = JsonlPhysicalStream::open_with_encoding(
        file,
        observation.length(),
        0,
        0,
        encoding,
        framing,
        JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
        E::source_changed,
    )?;
    let incomplete = stream.next_record()?.is_none_or(|record| !record.complete);
    opened.revalidate_same_object()?;
    Ok(incomplete)
}

fn bind_prior_disposition_sources<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    opening: &mut JsonlFamilyInventory<JsonlRuntimeError<R>>,
    bases: &[CertifiedSource],
) -> SourceBackedRouteResult<()> {
    let mut bases_by_path = BTreeMap::<PathBuf, SourceKey>::new();
    for base in bases {
        let path = adapter
            .base_source_path(base)
            .map_err(|error| route_scan(adapter, error))?;
        let source = base.observation().source();
        if bases_by_path
            .insert(path, source.clone())
            .is_some_and(|previous| !previous.exact_descriptor_eq(source))
        {
            return Err(route_invalid(
                "multiple JSONL base sources claim one physical member path",
            ));
        }
    }
    let mut changed = false;
    for member in &mut opening.members {
        let (path, bound) = match member {
            JsonlFamilyInventoryMember::Quarantined { leaf, .. } => {
                (&leaf.source_path, &mut leaf.quarantined_source)
            }
            JsonlFamilyInventoryMember::Pending { leaf, .. } => {
                (&leaf.source_path, &mut leaf.source)
            }
            JsonlFamilyInventoryMember::Accepted { .. } => continue,
        };
        let Some(base_source) = bases_by_path.get(path) else {
            continue;
        };
        if bound
            .as_ref()
            .is_some_and(|source| !source.exact_descriptor_eq(base_source))
        {
            return Err(route_invalid(
                "JSONL disposition source conflicts with the exact base member path",
            ));
        }
        if bound.is_none() {
            *bound = Some(base_source.clone());
            changed = true;
        }
    }
    if changed {
        opening.rebuild_observation().map_err(route_invalid)?;
    }
    Ok(())
}

/// Attempts one bounded existing-member refresh without enumerating route
/// membership. `Ok(false)` deliberately escalates to exhaustive discovery.
fn capture_partial_members<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    root: &Path,
    resident: &Mutex<FamilyResident<JsonlRuntimeError<R>>>,
    sink: &mut SourceBackedGenerationSink<'_, R::Lifecycle>,
    members: &BTreeSet<PathBuf>,
) -> SourceBackedRouteResult<bool> {
    if members.is_empty()
        || sink.reconciliation_demand()
            != ctx_history_capture_runtime::SourceBackedReconciliationDemand::Incremental
    {
        return Ok(false);
    }
    let opened =
        open_partial_members(adapter, root, members).map_err(|error| route_scan(adapter, error))?;
    let Some(mut leaves) = opened else {
        return Ok(false);
    };
    if leaves.len() != members.len() {
        return Ok(false);
    }
    adapter
        .order_leaf_scans(&mut leaves)
        .map_err(|error| route_scan(adapter, error))?;
    let owned_elsewhere = leaves
        .iter()
        .any(|leaf| sink.source_owned_by_other_route(leaf.source()));
    if owned_elsewhere {
        return Ok(false);
    }

    let mut bases = Vec::with_capacity(leaves.len());
    let mut owned_sources = HashMap::with_capacity(leaves.len());
    for leaf in &leaves {
        let digest = leaf.source().exact_descriptor_digest();
        if owned_sources
            .insert(digest, leaf.source().clone())
            .is_some()
        {
            return Ok(false);
        }
        let Some(base) = sink.base_route_source(leaf.source()).cloned() else {
            return Ok(false);
        };
        match adapter.base_source_path(&base) {
            Ok(path) if path == leaf.source_path() => {}
            Ok(_) => {
                return Ok(false);
            }
            Err(_) => {
                return Ok(false);
            }
        }
        let Ok(checkpoint) = decode_checkpoint(adapter, leaf, &base) else {
            return Ok(false);
        };
        let retained = checkpoint.physical.source_observation();
        let current = leaf.observation();
        let unchanged = retained == current;
        let append_candidate =
            current.length() > retained.length() && retained.same_stable_file(current);
        if !unchanged && !append_candidate {
            return Ok(false);
        }
        bases.push(base);
    }
    reset_terminal(resident)?;
    let bases_by_descriptor = bases_by_descriptor(&bases)?;
    let base_event_lookup = sink.base_event_lookup();
    let terminal_sources = scan_leaves(
        adapter,
        &leaves,
        &bases_by_descriptor,
        base_event_lookup,
        sink,
        true,
    );
    let finish_leaf_scans = adapter
        .finish_leaf_scans()
        .map_err(|error| route_scan(adapter, error));
    let scan_result = terminal_sources?;
    finish_leaf_scans?;
    // A partial refresh has no complete inventory with which to delete a
    // formerly valid source. Do not carry that base after ownership becomes
    // ambiguous; let this attempt fall through to exhaustive discovery before
    // it stages retention or a receipt-local failure.
    if !scan_result.quarantined.is_empty() {
        return Ok(false);
    }
    let terminal_sources = scan_result.terminal_sources;
    if terminal_sources.len() != leaves.len() {
        return Err(route_internal(
            "partial JSONL scan did not produce one terminal proof per selected member",
        ));
    }
    sink.retain_unstaged_base_route_sources()
        .map_err(route_internal)?;
    let mut resident = resident
        .lock()
        .map_err(|_| route_internal("JSONL resident catalog lock was poisoned"))?;
    // Unselected route members remain owned by the immutable carried base even
    // though this attempt intentionally has no complete-inventory proof.
    resident.ownership_initialized = false;
    resident.owned_sources = owned_sources;
    resident.quarantined_sources.clear();
    resident.replace_terminal_sources(terminal_sources);
    resident.absent_sources.clear();
    resident.opening_membership = None;
    resident.certified_inventory = None;
    resident.opening_inventory = None;
    Ok(true)
}

fn open_partial_members<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    _root: &Path,
    members: &BTreeSet<PathBuf>,
) -> JsonlPartialLeavesResult<R> {
    let Some(root_paths) = adapter.partial_member_roots(_root) else {
        return Ok(None);
    };
    if root_paths.is_empty() {
        return Ok(None);
    }
    let mut authorities = Vec::with_capacity(root_paths.len());
    for root_path in root_paths {
        authorities.push(Arc::new(ProviderSourceRoot::open(&lexical_absolute::<
            JsonlRuntimeError<R>,
        >(&root_path)?)?));
    }

    let mut normalized_members = BTreeSet::new();
    let mut leaves = Vec::with_capacity(members.len());
    for requested in members {
        let source_path = lexical_absolute::<JsonlRuntimeError<R>>(requested)?;
        if !normalized_members.insert(source_path.clone())
            || source_path.as_os_str().as_encoded_bytes().len()
                > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES
        {
            return Ok(None);
        }
        let matches = authorities
            .iter()
            .filter_map(|authority| {
                source_path
                    .strip_prefix(authority.named_path())
                    .ok()
                    .filter(|relative| !relative.as_os_str().is_empty())
                    .map(|relative| (Arc::clone(authority), relative.to_path_buf()))
            })
            .collect::<Vec<_>>();
        let [(authority, authority_path)] = matches.as_slice() else {
            return Ok(None);
        };
        if authority_path.components().count()
            > PROVIDER_JSONL_INVENTORY_MAX_DEPTH.saturating_add(1)
        {
            return Ok(None);
        }
        let opened = match authority.open_file(authority_path) {
            Ok(opened) => opened,
            Err(_) => return Ok(None),
        };
        let observation = observe_opened_file_allow_append(&source_path, &opened)?;
        let member = JsonlFamilyOpenedMember {
            source_path,
            authority_path: authority_path.clone(),
            authority: Arc::clone(authority),
            opened: &opened,
            observation,
        };
        let Some(leaf) = adapter.bind_partial_member(&member)? else {
            return Ok(None);
        };
        if leaf.source_path() != member.source_path()
            || leaf.authority_path != member.authority_path
            || leaf.authority.named_path() != member.authority.named_path()
            || leaf.observation() != member.observation()
        {
            return Ok(None);
        }
        leaves.push(leaf);
    }
    for authority in authorities {
        authority.revalidate_same_object()?;
    }
    Ok(Some(leaves))
}

fn lexical_absolute<E: JsonlFamilyError>(path: &Path) -> JsonlResult<PathBuf, E> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(E::invalid_payload(
                        "partial JSONL member escapes its filesystem root".to_owned(),
                    ));
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn bases_by_descriptor(
    bases: &[CertifiedSource],
) -> SourceBackedRouteResult<HashMap<[u8; 32], &CertifiedSource>> {
    let mut by_descriptor = HashMap::with_capacity(bases.len());
    for base in bases {
        let source = base.observation().source();
        let digest = source.exact_descriptor_digest();
        if let Some(previous) = by_descriptor.insert(digest, base) {
            if !previous.observation().source().exact_descriptor_eq(source) {
                return Err(route_invalid(
                    "JSONL base source descriptor digest collision",
                ));
            }
            return Err(route_invalid("duplicate JSONL base source descriptor"));
        }
    }
    Ok(by_descriptor)
}
