use super::*;
use crate::family::{observe_opened_file, probe_first_record};

const HEADER: &[u8] = b"{\"id\":\"alpha\"}\n";
const BODY: &[u8] = b"{\"message\":\"body\"}\n";

#[derive(Clone, Copy, Debug)]
enum Mutation {
    Rewrite,
    Touch,
    Append,
}

fn mutate(path: &Path, mutation: Mutation) {
    let opened =
        ctx_history_source_io::open_provider_source_file_mapped::<CaptureError>(path).unwrap();
    let before = observe_opened_file(path, &opened).unwrap();
    let modified = fs::metadata(path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    match mutation {
        Mutation::Rewrite => {
            let bytes = fs::read_to_string(path).unwrap();
            fs::write(path, bytes.replace("alpha", "bravo")).unwrap();
        }
        Mutation::Touch => {}
        Mutation::Append => {
            OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(BODY)
                .unwrap();
        }
    }
    // set_modified updates ctime even when restoring the original mtime.
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let reopened =
        ctx_history_source_io::open_provider_source_file_mapped::<CaptureError>(path).unwrap();
    let after = observe_opened_file(path, &reopened).unwrap();
    if !matches!(mutation, Mutation::Append) {
        assert!(before.differs_only_by_change_identity(&after));
    }
}

struct ReplayAdapter {
    before_scan: Option<Mutation>,
    after_reader_open: Option<Mutation>,
    admitted_eof: bool,
}

impl JsonlFamilyAdapter for ReplayAdapter {
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
        "replayed-probe-test-v1"
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }
    fn bind_admitted_eof(&self) -> bool {
        self.admitted_eof
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let inventory = TestAdapter.discover(root)?;
        let leaves = inventory
            .accepted_leaves()
            .map(|leaf| {
                let opened = Arc::new(leaf.authority().open_file(leaf.authority_path())?);
                let (_, probe) =
                    probe_first_record(leaf.source_path(), &opened, |_| Ok::<_, CaptureError>(()))?;
                JsonlFamilyLeaf::observe_replaying_identity_probe(
                    leaf.source().clone(),
                    leaf.source_path().to_owned(),
                    Arc::clone(leaf.authority()),
                    leaf.authority_path().to_owned(),
                    leaf.binding().clone(),
                    probe,
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

    fn order_leaf_scans(&self, leaves: &mut [JsonlFamilyLeaf]) -> Result<()> {
        if let Some(mutation) = self.before_scan {
            mutate(leaves[0].source_path(), mutation);
        }
        Ok(())
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _opened: Arc<OpenedProviderSourceFile>,
        _at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        if let Some(mutation) = self.after_reader_open {
            mutate(leaf.source_path(), mutation);
        }
        Ok(Box::new(ReplayProjector {
            source: leaf.source().clone(),
        }))
    }
}

// Deliberately has no provider-specific header comparison. The shared probe
// alone must authorize the binding, including for metadata-only headers.
struct ReplayProjector {
    source: SourceKey,
}
impl JsonlFamilyProjector for ReplayProjector {
    type Runtime = TestJsonlRuntime;
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if record.evidence().physical_ordinal() != 0 {
            emit(emission_test_record(
                &self.source,
                record.evidence().physical_ordinal(),
            )?)?;
        }
        Ok(())
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let path = root.join("session.jsonl");
    fs::write(&path, [HEADER, BODY].concat()).unwrap();
    (temp, root, path)
}

#[test]
fn replayed_probe_rejects_rewrite_at_scan_and_reader_boundaries() {
    for admitted_eof in [false, true] {
        for after_reader_open in [false, true] {
            let (temp, root, _) = fixture();
            let adapter = ReplayAdapter {
                before_scan: (!after_reader_open).then_some(Mutation::Rewrite),
                after_reader_open: after_reader_open.then_some(Mutation::Rewrite),
                admitted_eof,
            };
            let (_writer, _resident, result) = capture_test_generation!(
                &adapter,
                &root,
                &temp.path().join("index"),
                1,
                |resident, sink| capture(&adapter, &root, resident, sink)
            );
            assert_eq!(
                result.unwrap_err().kind,
                SourceBackedRouteErrorKind::SourceChanged
            );
        }
    }
}

#[test]
fn replayed_probe_accepts_unchanged_content_and_append_without_skipping_header() {
    for admitted_eof in [false, true] {
        for mutation in [Mutation::Touch, Mutation::Append] {
            let (temp, root, _) = fixture();
            let adapter = ReplayAdapter {
                before_scan: Some(mutation),
                after_reader_open: None,
                admitted_eof,
            };
            let (receipt, _) = capture_parallel_test_generation_with_terminal_revalidation(
                &adapter,
                &root,
                &temp.path().join("index"),
                1,
            )
            .unwrap();
            let expected = if matches!(mutation, Mutation::Append) {
                2
            } else {
                1
            };
            assert_eq!(receipt.manifest().records.len(), expected);
            let counts = receipt.manifest().sources[0].counts();
            assert_eq!(counts.complete_records, expected as u64 + 1);
            assert_eq!(counts.retained_records, expected as u64);
            assert_eq!(counts.ignored_records, 1);
            assert_eq!(counts.rejected_records, 0);
        }
    }
}

#[test]
fn replayed_probe_authenticates_between_leaf_and_reader_opening() {
    let (_temp, root, path) = fixture();
    let adapter = ReplayAdapter {
        before_scan: None,
        after_reader_open: None,
        admitted_eof: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let (leaf, opened) = inventory
        .accepted_leaves()
        .next()
        .unwrap()
        .open_for_scan()
        .unwrap();
    mutate(&path, Mutation::Rewrite);
    let mut reader =
        JsonlReader::open(physical_identity(&adapter, &leaf), opened, None, None).unwrap();
    let error = reader
        .bind_replayed_identity_probe(*leaf.replayed_identity_probe.unwrap())
        .unwrap_err();
    assert!(error.is_source_changed());
}

#[test]
fn replayed_probe_terminal_proof_rejects_rewrite_after_scan() {
    let (temp, root, path) = fixture();
    let adapter = ReplayAdapter {
        before_scan: None,
        after_reader_open: None,
        admitted_eof: false,
    };
    let (_writer, resident, result) = capture_test_generation!(
        &adapter,
        &root,
        &temp.path().join("index"),
        1,
        |resident, sink| capture(&adapter, &root, resident, sink)
    );
    result.unwrap();
    mutate(&path, Mutation::Rewrite);
    let result = revalidate_test_sources(&root, &resident);
    assert!(matches!(result, Ok(false)) || result.is_err_and(|error| error.is_source_changed()));
}

#[test]
fn replayed_probe_binds_semantic_records_and_reader_completion() {
    for after_header in [false, true] {
        let (_temp, root, path) = fixture();
        let adapter = ReplayAdapter {
            before_scan: None,
            after_reader_open: None,
            admitted_eof: false,
        };
        let inventory = adapter.discover(&root).unwrap();
        let (leaf, opened) = inventory
            .accepted_leaves()
            .next()
            .unwrap()
            .open_for_scan()
            .unwrap();
        let mut reader =
            JsonlReader::open(physical_identity(&adapter, &leaf), opened, None, None).unwrap();
        reader
            .bind_replayed_identity_probe(*leaf.replayed_identity_probe.unwrap())
            .unwrap();
        if after_header {
            assert_eq!(
                reader
                    .next_execution_record()
                    .unwrap()
                    .unwrap()
                    .physical_ordinal,
                0
            );
        }
        mutate(&path, Mutation::Rewrite);
        let error = loop {
            match reader.next_execution_record() {
                Err(error) => break error,
                Ok(Some(_)) => {}
                Ok(None) => panic!("rewritten identity was accepted"),
            }
        };
        assert!(error.is_source_changed());
        assert!(reader.outcome().is_none());
    }
}
