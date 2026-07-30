use std::{fs, path::Path, sync::Arc};

use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    SourceRecordLocator,
};
use sha2::{Digest, Sha256};

use super::*;

const TEST_SOURCE_FORMAT: &str = "terminal_witness_jsonl";
const TEST_SCHEMA: &str = "terminal-witness-v1";

struct TestAdapter;

const TEST_RECORD: &[u8] = b"{\"message\":\"before\"}\n";

struct TestHydrator {
    source_file: Arc<OpenedProviderSourceFile>,
}

impl JsonlFamilyHydrator for TestHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let NativeRecordCoordinate::Jsonl {
            byte_offset,
            byte_length,
            ..
        } = request.locator().coordinate()
        else {
            return Err(hydration_error(
                HydrationFailureKind::InvalidLocator,
                "terminal witness locator is not JSONL",
            ));
        };
        let bytes = self
            .source_file
            .read_exact_range_allow_append(
                *byte_offset,
                usize::try_from(*byte_length).unwrap(),
                TEST_RECORD.len(),
            )
            .map_err(|error| hydration_error(HydrationFailureKind::StaleRecordEvidence, error))?;
        if &<[u8; 32]>::from(Sha256::digest(&bytes)) != request.locator().record_digest() {
            return Err(hydration_error(
                HydrationFailureKind::StaleRecordEvidence,
                "terminal witness record digest changed",
            ));
        }
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: b"frozen exact body".to_vec(),
        })
    }
}

impl JsonlFamilyAdapter for TestAdapter {
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
                    TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
                )
                .map_err(contract_error)?,
            )
            .map_err(contract_error)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                PathBuf::from(&name),
                TypedKey::bytes(name.as_encoded_bytes().to_vec()).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Err(CaptureError::SystemInvariant(
            "terminal witness tests never project",
        ))
    }

    fn hydrator(
        &self,
        _leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        Ok(Box::new(TestHydrator { source_file }))
    }
}

fn hydration_request(source: SourceKey) -> EventHydrationRequest {
    let native_session_key =
        NativeSessionKey::native_id("terminal-witness.session", TypedKey::U64(1)).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "terminal-witness-session",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key =
        NativeItemKey::native_id("terminal-witness.event", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "terminal-witness-event",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let locator = SourceRecordLocator::new(
        source,
        NativeRecordCoordinate::Jsonl {
            byte_offset: 0,
            byte_length: TEST_RECORD.len() as u64,
            physical_ordinal: 0,
            native_session_key: Some(TypedKey::U64(1)),
            native_event_key: Some(TypedKey::U64(1)),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        Sha256::digest(TEST_RECORD).into(),
    )
    .unwrap();
    EventHydrationRequest::new(event_id, locator).unwrap()
}

fn expected_state(
    adapter: &TestAdapter,
    root: &Path,
) -> (FamilyResident, CertifiedSourceInventory) {
    let observed = adapter.discover(root).unwrap();
    let inventory = observed.certify_against(&observed).unwrap();
    let terminal_sources = observed
        .leaves()
        .iter()
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
            let observation =
                source_observation(leaf.source(), checkpoint.source_observation()).unwrap();
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                "terminal-witness-parser-v1",
                *checkpoint.complete_prefix_sha256(),
                ScannedSourceCounts::default(),
            )
            .unwrap();
            (
                leaf.source().exact_descriptor_digest(),
                TerminalSourceEvidence {
                    certificate,
                    checkpoint: Some(checkpoint),
                },
            )
        })
        .collect();
    let owned_sources = observed
        .leaves()
        .iter()
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
            terminal_sources,
            certified_inventory: Some(inventory.clone()),
            hydration_inventory: None,
        },
        inventory,
    )
}

fn expected_source(resident: &FamilyResident) -> CertifiedSource {
    resident
        .terminal_sources
        .values()
        .next()
        .unwrap()
        .certificate
        .clone()
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rediscovers_live_tree() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, b"{\"message\":\"before\"}\n").unwrap();
    let adapter = TestAdapter;

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(&first, b"{\"message\":\"changed between callbacks\"}\n").unwrap();
    assert!(
        !revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap_or(false)
    );

    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));
    fs::write(root.join("new.jsonl"), b"{\"message\":\"late leaf\"}\n").unwrap();
    assert!(!revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap());
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_accepts_proven_append() {
    use std::{fs::OpenOptions, io::Write};

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let first = root.join("first.jsonl");
    fs::write(&first, TEST_RECORD).unwrap();
    let adapter = TestAdapter;
    let (resident, inventory) = expected_state(&adapter, &root);
    let source = expected_source(&resident);
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Source(&source),
    ));

    OpenOptions::new()
        .append(true)
        .open(&first)
        .unwrap()
        .write_all(b"{\"message\":\"next refresh\"}\n")
        .unwrap();
    assert!(revalidate_complete_inventory(&adapter, &root, &resident, &inventory,).unwrap());

    let hydration_source = adapter
        .discover(&root)
        .unwrap()
        .leaves()
        .first()
        .unwrap()
        .source()
        .clone();
    let request = hydration_request(hydration_source);
    let append_path = first.clone();
    set_after_jsonl_group_open_hook(move || {
        OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap()
            .write_all(b"{\"message\":\"during hydration\"}\n")
            .unwrap();
    });
    let hydration_resident = Mutex::new(FamilyResident::default());
    let hydrated = hydrate_single(&adapter, &root, &hydration_resident, &request)
        .expect("append-safe hydrate");
    assert_eq!(hydrated.provider_bytes, b"frozen exact body");
}

#[test]
fn active_source_family_contract_jsonl_hydration_rejects_same_length_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("first.jsonl");
    fs::write(&source_path, b"{\"message\":\"before\"}\n").unwrap();
    let adapter = TestAdapter;
    let source = adapter
        .discover(&root)
        .unwrap()
        .leaves()
        .first()
        .unwrap()
        .source()
        .clone();
    let request = hydration_request(source);
    let rewrite_path = source_path.clone();
    set_after_jsonl_group_open_hook(move || {
        fs::write(rewrite_path, b"{\"message\":\"after!\"}\n").unwrap();
    });
    let resident = Mutex::new(FamilyResident::default());
    let error = hydrate_single(&adapter, &root, &resident, &request).unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn active_source_family_contract_jsonl_terminal_inventory_rejects_reappearance() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("retained.jsonl"), b"{\"message\":\"kept\"}\n").unwrap();
    let deleted_path = root.join("deleted.jsonl");
    fs::write(&deleted_path, b"{\"message\":\"old\"}\n").unwrap();
    let adapter = TestAdapter;
    let before = adapter.discover(&root).unwrap();
    let deleted_source = before
        .leaves()
        .iter()
        .find(|leaf| leaf.source_path() == deleted_path)
        .unwrap()
        .source()
        .clone();

    fs::remove_file(&deleted_path).unwrap();
    let (resident, inventory) = expected_state(&adapter, &root);
    let deletion = CertifiedSourceDeletion::from_inventory(deleted_source, &inventory).unwrap();
    let resident = Mutex::new(resident);
    assert!(revalidate_target(
        &resident,
        SourceBackedRevalidationTarget::Deletion(&deletion),
    ));

    fs::write(&deleted_path, b"{\"message\":\"reappeared\"}\n").unwrap();
    assert!(!revalidate_complete_inventory(&adapter, &root, &resident, &inventory).unwrap());
}
