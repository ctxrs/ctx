use super::*;

pub(super) struct TestAdapter;

struct PendingOnlyAdapter;

pub(super) struct QuarantinedTestAdapter;

pub(super) struct ReplacementWithQuarantineTestAdapter;

pub(super) const TEST_RECORD: &[u8] = b"{\"message\":\"before\"}\n";
pub(super) const PROGRESS_TEST_RECORDS: &[u8] =
    b"{\"message\":\"one\"}\n{\"message\":\"two\"}\n{\"tool_call\":\"three\"}\n";

impl JsonlFamilyAdapter for TestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-witness-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        if !root.exists() {
            return JsonlFamilyInventory::missing(self.provider(), root);
        }
        let authority = Arc::new(ProviderSourceRoot::open(root)?);
        let mut leaves = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let name = entry.file_name();
            let source = SourceKey::derive(
                self.provider().as_str(),
                TEST_SOURCE_FORMAT,
                TEST_SCHEMA,
                1,
                SourceAnchor::provider_native(
                    "terminal-witness-file",
                    TypedKey::bytes(name.as_encoded_bytes().to_vec())
                        .map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?,
            )
            .map_err(test_contract_error)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                PathBuf::from(&name),
                TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(test_contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "terminal witness tests never project",
        ))
    }
}

impl JsonlFamilyAdapter for PendingOnlyAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "pending-only-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        // Deliberately present this as accepted provider input. The shared
        // capture boundary, not this adapter, must recognize its physical
        // first record as pending.
        TestAdapter.discover(root)
    }
}

impl JsonlFamilyAdapter for QuarantinedTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "quarantined-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let discovered = TestAdapter.discover(root)?;
        let leaf = discovered
            .accepted_leaves()
            .next()
            .ok_or(CaptureError::SystemInvariant(
                "quarantine test inventory has no leaf",
            ))?;
        let claimed_source = leaf.source().clone();
        let failure_source = SourceKey::derive_provider_native(
            self.provider().as_str(),
            TEST_SOURCE_FORMAT,
            TEST_SCHEMA,
            1,
            "quarantined-test-file",
            TypedKey::utf8("quarantined-sibling").map_err(test_contract_error)?,
        )
        .map_err(test_contract_error)?;
        JsonlFamilyInventory::present_with_rejected(
            self.provider(),
            root,
            Arc::clone(leaf.authority()),
            Vec::new(),
            vec![JsonlFamilyRejectedLeaf::bind_observed(
                leaf.source_path().to_path_buf(),
                PathBuf::from(leaf.source_path().file_name().ok_or(
                    CaptureError::SystemInvariant("quarantine test leaf has no filename"),
                )?),
                leaf.observation().clone(),
                TypedKey::utf8("quarantined-sibling").map_err(test_contract_error)?,
                1,
            )
            .with_logical_source_failure(failure_source, "quarantined sibling source")
            .with_quarantined_source(claimed_source)],
        )
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "quarantine tests never project",
        ))
    }
}

impl JsonlFamilyAdapter for ReplacementWithQuarantineTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "replacement-with-quarantine-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let discovered = TestAdapter.discover(root)?;
        let authority =
            discovered
                .authorities
                .first()
                .cloned()
                .ok_or(CaptureError::SystemInvariant(
                    "replacement quarantine test inventory has no authority",
                ))?;
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for leaf in discovered.accepted_leaves() {
            if leaf.source_path().ends_with("current.jsonl") {
                let mut replacement = leaf.clone();
                replacement.source = SourceKey::derive_provider_native(
                    self.provider().as_str(),
                    TEST_SOURCE_FORMAT,
                    TEST_SCHEMA,
                    1,
                    "replacement-current-test",
                    TypedKey::utf8("current").map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?;
                accepted.push(replacement);
            } else if leaf.source_path().ends_with("sibling.jsonl") {
                let failure_source = SourceKey::derive_provider_native(
                    self.provider().as_str(),
                    TEST_SOURCE_FORMAT,
                    TEST_SCHEMA,
                    1,
                    "quarantined-test-file",
                    TypedKey::utf8("replacement-sibling").map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?;
                rejected.push(
                    JsonlFamilyRejectedLeaf::bind_observed(
                        leaf.source_path().to_path_buf(),
                        leaf.authority_path.clone(),
                        leaf.observation().clone(),
                        TypedKey::utf8("replacement-sibling").map_err(test_contract_error)?,
                        1,
                    )
                    .with_logical_source_failure(
                        failure_source,
                        "quarantined sibling replacement source",
                    )
                    .with_quarantined_source(leaf.source().clone()),
                );
            } else {
                return Err(CaptureError::SystemInvariant(
                    "replacement quarantine test inventory has an unexpected leaf",
                ));
            }
        }
        JsonlFamilyInventory::present_with_rejected(
            self.provider(),
            root,
            authority,
            accepted,
            rejected,
        )
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Ok(Box::new(ParallelTestProjector))
    }
}

#[test]
fn canonical_inventory_exposes_typed_physical_leaf_dispositions() {
    let temp = tempfile::tempdir().unwrap();
    let accepted_path = temp.path().join("accepted.jsonl");
    let quarantined_path = temp.path().join("quarantined.jsonl");
    let pending_path = temp.path().join("pending.jsonl");
    fs::write(&accepted_path, TEST_RECORD).unwrap();
    fs::write(&quarantined_path, b"not-json\n").unwrap();
    fs::write(&pending_path, b"").unwrap();

    let discovered = TestAdapter.discover(temp.path()).unwrap();
    let accepted = discovered
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == accepted_path)
        .unwrap()
        .clone();
    let authority = Arc::clone(&discovered.authorities[0]);
    let rejected_opened = authority.open_file(Path::new("quarantined.jsonl")).unwrap();
    let rejected_observation = observe_opened_file(&quarantined_path, &rejected_opened).unwrap();
    let pending_opened = authority.open_file(Path::new("pending.jsonl")).unwrap();
    let pending_observation = observe_opened_file(&pending_path, &pending_opened).unwrap();
    let inventory = JsonlFamilyInventory::present_multi_with_dispositions(
        CaptureProvider::Pi,
        temp.path(),
        vec![authority],
        vec![accepted],
        vec![JsonlFamilyRejectedLeaf::bind_observed(
            quarantined_path,
            PathBuf::from("quarantined.jsonl"),
            rejected_observation,
            TypedKey::utf8("bad-owner").unwrap(),
            1,
        )],
        vec![JsonlFamilyPendingLeaf::bind_observed(
            pending_path,
            PathBuf::from("pending.jsonl"),
            pending_observation,
            TypedKey::utf8("incomplete").unwrap(),
            None,
        )],
    )
    .unwrap();

    assert_eq!(
        inventory
            .members()
            .iter()
            .map(JsonlFamilyInventoryMember::disposition)
            .collect::<Vec<_>>(),
        vec![
            JsonlFamilyLeafDisposition::Accepted,
            JsonlFamilyLeafDisposition::Pending,
            JsonlFamilyLeafDisposition::Quarantined,
        ]
    );
    assert!(inventory
        .members()
        .windows(2)
        .all(|pair| pair[0].identity() != pair[1].identity()));
}

#[test]
fn canonical_inventory_rejects_two_dispositions_for_one_physical_leaf() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("same.jsonl");
    fs::write(&path, TEST_RECORD).unwrap();
    let discovered = TestAdapter.discover(temp.path()).unwrap();
    let accepted = discovered.accepted_leaves().next().unwrap().clone();
    let pending_observation = accepted.observation().clone();
    let authority = Arc::clone(&discovered.authorities[0]);

    let error = JsonlFamilyInventory::present_multi_with_dispositions(
        CaptureProvider::Pi,
        temp.path(),
        vec![authority],
        vec![accepted],
        Vec::new(),
        vec![JsonlFamilyPendingLeaf::bind_observed(
            path,
            PathBuf::from("same.jsonl"),
            pending_observation,
            TypedKey::utf8("incomplete").unwrap(),
            None,
        )],
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("physical inventory contains duplicate member"));
}

#[test]
fn diagnosed_quarantine_owner_is_not_reclassified_as_pending() {
    let temp = tempfile::tempdir().unwrap();
    let owner_path = temp.path().join("owner.jsonl");
    let peer_path = temp.path().join("peer.jsonl");
    fs::write(&owner_path, b"{\"message\":\"incomplete\"").unwrap();
    fs::write(&peer_path, TEST_RECORD).unwrap();

    let discovered = TestAdapter.discover(temp.path()).unwrap();
    let source = discovered
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == owner_path)
        .unwrap()
        .source()
        .clone();
    let owner_observation = discovered
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == owner_path)
        .unwrap()
        .observation()
        .clone();
    let peer_observation = discovered
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == peer_path)
        .unwrap()
        .observation()
        .clone();
    let authority = Arc::clone(&discovered.authorities[0]);
    let mut inventory = JsonlFamilyInventory::present_with_rejected(
        CaptureProvider::Pi,
        temp.path(),
        authority,
        Vec::new(),
        vec![
            JsonlFamilyRejectedLeaf::bind_observed(
                owner_path,
                PathBuf::from("owner.jsonl"),
                owner_observation,
                TypedKey::utf8("owner").unwrap(),
                0,
            )
            .with_logical_source_failure(source.clone(), "duplicate source")
            .with_quarantined_source(source.clone()),
            JsonlFamilyRejectedLeaf::bind_observed(
                peer_path,
                PathBuf::from("peer.jsonl"),
                peer_observation,
                TypedKey::utf8("peer").unwrap(),
                0,
            )
            .with_quarantined_source(source),
        ],
    )
    .unwrap();

    super::super::capture::classify_incomplete_first_records(&PendingOnlyAdapter, &mut inventory)
        .unwrap();

    assert_eq!(inventory.quarantined_len(), 2);
    assert_eq!(inventory.pending_len(), 0);
    assert_eq!(
        inventory
            .quarantined_leaves()
            .filter(|leaf| leaf.logical_source_failure.is_some())
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn unobserved_membership_uses_parent_enumeration_without_opening_the_leaf() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("denied.jsonl");
    fs::write(&path, TEST_RECORD).unwrap();
    let discovered = TestAdapter.discover(&root).unwrap();
    let source = discovered
        .accepted_leaves()
        .next()
        .unwrap()
        .source()
        .clone();
    let authority = Arc::clone(&discovered.authorities[0]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let inventory = JsonlFamilyInventory::present_with_rejected(
        CaptureProvider::Pi,
        &root,
        authority,
        Vec::new(),
        vec![JsonlFamilyRejectedLeaf::bind_unobserved(
            path.clone(),
            PathBuf::from("denied.jsonl"),
            TypedKey::utf8("denied").unwrap(),
            0,
        )
        .with_logical_source_failure(source.clone(), "permission denied")
        .with_quarantined_source(source)],
    )
    .unwrap();

    let opening = JsonlFamilyMembershipObservation::observe(&root, &inventory).unwrap();
    let unchanged = JsonlFamilyMembershipObservation::observe(&root, &inventory).unwrap();
    assert!(opening.admits(
        &unchanged,
        JsonlFamilyInventoryMode::Exact,
        &HashMap::new(),
        &HashMap::new(),
    ));

    fs::remove_file(&path).unwrap();
    let removed = JsonlFamilyMembershipObservation::observe(&root, &inventory).unwrap();
    assert!(!opening.admits(
        &removed,
        JsonlFamilyInventoryMode::Exact,
        &HashMap::new(),
        &HashMap::new(),
    ));
}

#[test]
fn canonical_inventory_rejects_duplicate_logical_sources_before_staging() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first.jsonl");
    let second_path = temp.path().join("second.jsonl");
    fs::write(&first_path, TEST_RECORD).unwrap();
    fs::write(&second_path, TEST_RECORD).unwrap();
    let discovered = TestAdapter.discover(temp.path()).unwrap();
    let first = discovered
        .accepted_leaves()
        .find(|leaf| leaf.source_path() == first_path)
        .unwrap()
        .clone();
    let second = JsonlFamilyLeaf::observe(
        first.source().clone(),
        second_path,
        Arc::clone(first.authority()),
        PathBuf::from("second.jsonl"),
        TypedKey::utf8("second-binding").unwrap(),
    )
    .unwrap();

    let error = JsonlFamilyInventory::present(
        CaptureProvider::Pi,
        temp.path(),
        Arc::clone(first.authority()),
        vec![first, second],
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("duplicate logical source identity"));
    assert!(!error
        .to_string()
        .contains("source replacement has already started"));
}

#[test]
fn nonzero_incomplete_first_record_is_pending_before_provider_projection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("pending.jsonl"), br#"{"message":"unfinished""#).unwrap();
    let (_writer, _resident, result) = capture_test_generation!(
        &PendingOnlyAdapter,
        &root,
        &temp.path().join("index"),
        1,
        |resident, sink| capture(&PendingOnlyAdapter, &root, resident, sink)
    );

    let error = result.unwrap_err();
    assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    assert!(error.detail.contains("incomplete sources"));
}

macro_rules! impl_standard_jsonl_test_adapter {
    (
        $adapter:ty,
        $parser_revision:literal,
        $append_mode:expr,
        |$this:ident, $leaf:ident, $source_file:ident, $imported_at:ident| $projector:block
        $(, |$framing_adapter:ident| $record_framing:expr)?
    ) => {
        impl JsonlFamilyAdapter for $adapter {
            type Runtime = TestJsonlRuntime;

            fn provider(&self) -> CaptureProvider {
                CaptureProvider::Pi
            }

            fn source_format(&self) -> &'static str {
                TEST_SOURCE_FORMAT
            }

            fn schema_variant(&self) -> &'static str {
                TEST_SCHEMA
            }

            fn parser_revision(&self) -> &'static str {
                $parser_revision
            }

            fn append_mode(&self) -> JsonlFamilyAppendMode {
                $append_mode
            }

            $(
                fn record_framing(&self) -> JsonlRecordFraming {
                    let $framing_adapter = self;
                    $record_framing
                }
            )?

            fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
                TestAdapter.discover(root)
            }

            fn projector(
                &self,
                leaf: &JsonlFamilyLeaf,
                source_file: Arc<OpenedProviderSourceFile>,
                imported_at: DateTime<Utc>,
            ) -> Result<Box<JsonlFamilyProjectorObject>> {
                let $this = self;
                let $leaf = leaf;
                let $source_file = source_file;
                let $imported_at = imported_at;
                $projector
            }
        }
    };
}

#[cfg(unix)]
pub(super) struct TerminalLeafSwapTestAdapter {
    pub(super) selected: PathBuf,
    pub(super) outside: PathBuf,
    pub(super) enabled: AtomicBool,
    pub(super) swapped: AtomicBool,
}

#[cfg(unix)]
impl JsonlFamilyAdapter for TerminalLeafSwapTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        TestAdapter.provider()
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-leaf-swap-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation> {
        if self.enabled.load(Ordering::SeqCst) && !self.swapped.swap(true, Ordering::SeqCst) {
            fs::remove_file(&self.selected)?;
            std::os::unix::fs::symlink(&self.outside, &self.selected)?;
        }
        JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "terminal leaf swap tests never project",
        ))
    }
}

pub(super) fn expected_state(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
) -> (FamilyResident, CertifiedSourceInventory) {
    let observed = adapter.discover(root).unwrap();
    let opening_membership = adapter
        .observe_terminal_membership(root, &observed)
        .unwrap();
    let inventory = observed.certify_against(&observed).unwrap();
    let terminal_sources = observed
        .accepted_leaves()
        .map(|leaf| {
            let opened = leaf.open_verified().unwrap();
            let mut reader =
                JsonlReader::open(physical_identity(adapter, leaf), opened, None, None).unwrap();
            while reader
                .visit_page(&mut |_record| -> Result<()> { Ok(()) })
                .unwrap()
                .is_some()
            {}
            let checkpoint = reader.outcome().unwrap().checkpoint().clone();
            let observation = scanner::source_observation::<CaptureError>(
                leaf.source(),
                checkpoint.source_observation(),
            )
            .unwrap();
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                adapter.parser_revision(),
                *checkpoint.complete_prefix_sha256(),
                ScannedSourceCounts::default(),
            )
            .unwrap();
            let terminal_proof = JsonlFamilyTerminalProof::frozen_shared_prefix(
                adapter,
                leaf,
                &certificate,
                checkpoint.complete_prefix_end(),
                *checkpoint.complete_prefix_sha256(),
            )
            .unwrap();
            (
                leaf.source().exact_descriptor_digest(),
                TerminalSourceEvidence {
                    certificate,
                    terminal_certificate: None,
                    terminal_proof,
                    emitted_bytes: 0,
                    exact_scan_bytes: None,
                    record_rejections: SourceBackedRecordRejectionDrafts::default(),
                },
            )
        })
        .collect();
    let owned_sources = observed
        .accepted_leaves()
        .map(|leaf| {
            (
                leaf.source().exact_descriptor_digest(),
                leaf.source().clone(),
            )
        })
        .collect();
    (
        FamilyResident {
            ownership_initialized: true,
            owned_sources,
            quarantined_sources: HashMap::new(),
            terminal_sources,
            absent_sources: Vec::new(),
            opening_membership: Some(opening_membership),
            certified_inventory: Some(inventory.clone()),
            opening_inventory: Some(observed),
            authenticated_source_observations: HashMap::new(),
        },
        inventory,
    )
}

pub(super) struct FrozenMultiRootTestAdapter {
    pub(super) roots: Vec<PathBuf>,
}

impl JsonlFamilyAdapter for FrozenMultiRootTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "frozen-multi-root-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let mut authorities = Vec::new();
        let mut leaves = Vec::new();
        for source_root in &self.roots {
            let authority = Arc::new(ProviderSourceRoot::open(source_root)?);
            for entry in fs::read_dir(source_root)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    continue;
                }
                let name = entry.file_name();
                let source = SourceKey::derive(
                    self.provider().as_str(),
                    TEST_SOURCE_FORMAT,
                    TEST_SCHEMA,
                    1,
                    SourceAnchor::provider_native(
                        "frozen-multi-root-file",
                        TypedKey::bytes(path.as_os_str().as_encoded_bytes().to_vec())
                            .map_err(test_contract_error)?,
                    )
                    .map_err(test_contract_error)?,
                )
                .map_err(test_contract_error)?;
                leaves.push(JsonlFamilyLeaf::observe(
                    source,
                    path,
                    Arc::clone(&authority),
                    PathBuf::from(&name),
                    TypedKey::bytes(name.as_encoded_bytes().to_vec())
                        .map_err(test_contract_error)?,
                )?);
            }
            authorities.push(authority);
        }
        authorities.reverse();
        JsonlFamilyInventory::present_multi(self.provider(), root, authorities, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "frozen inventory tests never project",
        ))
    }
}

pub(super) struct TerminalRootSwapTestAdapter {
    pub(super) root: PathBuf,
    pub(super) discoveries: AtomicUsize,
}

impl JsonlFamilyAdapter for TerminalRootSwapTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "terminal-root-swap-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, selection_root: &Path) -> Result<JsonlFamilyInventory> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        FrozenMultiRootTestAdapter {
            roots: vec![self.root.clone()],
        }
        .discover(selection_root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "terminal root swap tests never project",
        ))
    }
}

pub(super) fn expected_source(resident: &FamilyResident) -> CertifiedSource {
    resident
        .terminal_sources
        .values()
        .next()
        .unwrap()
        .certificate
        .clone()
}

pub(super) use impl_standard_jsonl_test_adapter;
