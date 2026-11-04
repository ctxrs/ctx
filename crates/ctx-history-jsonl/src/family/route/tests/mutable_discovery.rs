use super::*;
use crate::family::route::capture::classify_incomplete_first_records;
use crate::family::{
    probe_records_until_with_mutable_first_record, JsonlMutableFirstRecord, JsonlReader,
};
use std::{
    fs,
    io::Write,
    time::{Duration, SystemTime},
};

fn overwrite_title(path: &Path, title: &str, change_header: bool) {
    let mut value = serde_json::json!({
        "type": "title", "v": 1, "title": title, "pad": "",
        "updatedAt": "2026-10-17T00:00:00.000Z",
    });
    let padding = 255 - serde_json::to_vec(&value).unwrap().len();
    value["pad"] = serde_json::json!(" ".repeat(padding));
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    assert_eq!(bytes.len(), 256);
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.write_all(&bytes).unwrap();
    if change_header {
        file.write_all(b"{\"type\":\"session\",\"id\":\"modified\"}\n")
            .unwrap();
    }
    file.sync_all().unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
}

#[test]
fn title_changes_between_discovery_classification_and_scan_preserve_exact_header() {
    for (change_header, restore_mtime) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        let mut bytes = vec![b' '; 256];
        bytes.extend_from_slice(
            b"{\"type\":\"session\",\"id\":\"original\"}\n{\"message\":\"original body\"}\n",
        );
        fs::write(&path, bytes).unwrap();
        overwrite_title(&path, "initial title", false);
        let discovered = TestAdapter.discover(temp.path()).unwrap();
        let member = discovered.accepted_leaves().next().unwrap();
        let opened = Arc::new(member.authority.open_file(&member.authority_path).unwrap());
        let crate::family::JsonlProbeOutcome::Matched((), probe) =
            probe_records_until_with_mutable_first_record(
                &path,
                &opened,
                2,
                Some(JsonlMutableFirstRecord::OmpTitleSlotV1),
                |record| {
                    Ok::<_, ctx_history_source_io::SourceIoError>(
                        (record.evidence().physical_ordinal() == 1).then_some(()),
                    )
                },
            )
            .unwrap()
        else {
            panic!("expected a complete session header");
        };
        let leaf = JsonlFamilyLeaf::observe_after_identity_probe(
            member.source.clone(),
            path.clone(),
            Arc::clone(&member.authority),
            member.authority_path.clone(),
            member.binding.clone(),
            probe,
            0,
        )
        .unwrap();
        let mut opening = JsonlFamilyInventory::present(
            CaptureProvider::Pi,
            temp.path(),
            Arc::clone(&member.authority),
            vec![leaf],
        )
        .unwrap();

        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        if restore_mtime {
            std::thread::sleep(Duration::from_millis(2));
        }
        overwrite_title(&path, "before classification", change_header);
        if restore_mtime {
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(original_modified)
                .unwrap();
        }
        classify_incomplete_first_records(&TestAdapter, &mut opening).unwrap();
        overwrite_title(&path, "before scan", false);
        if restore_mtime {
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(original_modified)
                .unwrap();
            let leaf = opening.accepted_leaves().next().unwrap();
            let opened = leaf.authority.open_file(&leaf.authority_path).unwrap();
            let current = crate::family::observe_opened_file_allow_append(&path, &opened).unwrap();
            assert!(leaf.observation.differs_only_by_change_identity(&current));
        }
        let scanned = opening.accepted_leaves().next().unwrap().open_for_scan();
        if change_header {
            assert!(matches!(
                scanned,
                Err(ctx_history_source_io::SourceIoError::SourceChangedDuringCapture)
            ));
        } else {
            let (leaf, source_file) = scanned.unwrap();
            let mut reader = JsonlReader::open(
                physical_identity(&TestAdapter, &leaf),
                source_file,
                None,
                leaf.identity_probe.clone(),
            )
            .unwrap();
            let mut records = Vec::new();
            while reader
                .visit_page(&mut |record| {
                    records.push(record.bytes().to_vec());
                    Ok::<_, ctx_history_source_io::SourceIoError>(())
                })
                .unwrap()
                .is_some()
            {}
            assert_eq!(records, vec![br#"{"message":"original body"}"#.to_vec()]);
        }
    }
}

struct MutablePolicyAdapter {
    policy: Option<JsonlMutableFirstRecord>,
    projection: IdentityRevisionTestAdapter,
}

impl JsonlFamilyAdapter for MutablePolicyAdapter {
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
        self.projection.parser_revision()
    }
    fn event_identity_revision(&self) -> &'static str {
        self.projection.event_identity_revision()
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }
    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let inventory = TestAdapter.discover(root)?;
        let leaves = inventory
            .accepted_leaves()
            .map(|leaf| {
                let opened = Arc::new(leaf.authority().open_file(leaf.authority_path())?);
                let crate::family::JsonlProbeOutcome::Matched((), probe) =
                    probe_records_until_with_mutable_first_record(
                        leaf.source_path(),
                        &opened,
                        2,
                        self.policy,
                        |record| -> Result<Option<()>> {
                            let value: serde_json::Value = serde_json::from_slice(record.bytes())?;
                            Ok((value["type"] == "session").then_some(()))
                        },
                    )?
                else {
                    panic!("expected a complete session header");
                };
                JsonlFamilyLeaf::observe_after_identity_probe(
                    leaf.source().clone(),
                    leaf.source_path().to_owned(),
                    Arc::clone(leaf.authority()),
                    leaf.authority_path().to_owned(),
                    leaf.binding().clone(),
                    probe,
                    0,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        JsonlFamilyInventory::present(
            self.provider(),
            root,
            Arc::clone(&inventory.authorities[0]),
            leaves,
        )
    }
    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        self.projection.projector(leaf, source_file, imported_at)
    }
    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        self.projection.projector_with_provider_checkpoint(
            leaf,
            source_file,
            imported_at,
            checkpoint,
            base_event_lookup,
            mode,
        )
    }
}

#[test]
fn mutable_policy_replaces_only_omp_checkpoints_without_a_parser_revision_change() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir(&root).unwrap();
    let session = b"{\"type\":\"session\",\"id\":\"original\"}\n{\"message\":\"body\"}\n";
    let omp = root.join("omp.jsonl");
    fs::write(&omp, [vec![b' '; 256], session.to_vec()].concat()).unwrap();
    overwrite_title(&omp, "initial title", false);
    fs::write(root.join("pi.jsonl"), session).unwrap();
    let mut adapter = MutablePolicyAdapter {
        policy: None,
        projection: IdentityRevisionTestAdapter {
            parser_revision: "mutable-policy-test-v1",
            revision: "content-occurrence-v1",
            expected_mode: JsonlFamilyProjectionMode::Cold,
        },
    };
    let (cold, activity) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(activity.sources_started, 2);
    adapter.policy = Some(JsonlMutableFirstRecord::OmpTitleSlotV1);
    adapter.projection.expected_mode = JsonlFamilyProjectionMode::Replacement;
    let (upgraded, activity) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(activity.sources_started, 1, "only OMP needs reimport");
    assert_eq!(
        upgraded
            .manifest()
            .sources
            .iter()
            .filter(|source| cold.manifest().sources.contains(source))
            .count(),
        1,
        "ordinary Pi keeps its exact certificate",
    );
    assert!(upgraded
        .manifest()
        .sources
        .iter()
        .all(|source| source.parser_revision() == "mutable-policy-test-v1"));
    let (repeated, activity) = capture_parallel_test_generation(&adapter, &root, &index, 1);
    assert_eq!(activity, JsonlFamilyScannerActivity::default());
    assert_eq!(repeated.manifest().sources, upgraded.manifest().sources);
}
