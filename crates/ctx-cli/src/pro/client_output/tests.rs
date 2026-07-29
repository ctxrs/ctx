use super::source_backed_feed::*;
use super::*;
use std::fs;

use ctx_history_capture::{ingest_codex_source_backed_v0, CodexLocatorResolverV0};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, ScannedSourceCounts,
    SourceInventoryObservation, SourceObservation, TypedKey,
};
use ctx_history_index::VerifiedIndex;
use ctx_pro_host_protocol::{
    certified_source_revision_sha256, DeleteSourceRequest, FinishAdmittedSourceManifestRequest,
    FinishSourceManifestRequest, MaterializeSourcePageRequest, PrepareSourceRequest, SourceDeleted,
    SourceManifestAdmissionBegan, SourceManifestAdmitted, SourceManifestBegan,
    SourceManifestFinished, SourceManifestHeader, SourceManifestPage, SourceManifestPageAdmitted,
    SourceMessageFact, SourcePageMaterialized, SourcePrepared, SourceRecordMetadata,
    SourceSessionRelationships, TransientSourceContent, TransientSourceFact,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const MATERIALIZER_REVISION: &str = "pro-source-materializer-v1";

#[derive(Debug, Clone)]
struct FixtureSourceFeed {
    generation_id: String,
    generation_manifest: ctx_history_index::GenerationManifest,
    source: CertifiedSource,
    records: Vec<SourceBackedProRecord>,
    intermediate_frontier: SourceFrontier,
}

impl FixtureSourceFeed {
    fn provider(&self) -> FixtureProvider {
        let terminal_frontier = self
            .source
            .frontier()
            .expect("Codex fixture has a certified frontier")
            .clone();
        FixtureProvider {
            pages: vec![
                SourceBackedProviderPage {
                    source: self.source.observation().source().clone(),
                    expected_prior_frontier: None,
                    next_frontier: Some(self.intermediate_frontier.clone()),
                    terminal: false,
                    records: vec![self.records[0].clone()],
                },
                SourceBackedProviderPage {
                    source: self.source.observation().source().clone(),
                    expected_prior_frontier: Some(self.intermediate_frontier.clone()),
                    next_frontier: Some(terminal_frontier),
                    terminal: true,
                    records: vec![self.records[1].clone()],
                },
            ],
            requests: Vec::new(),
        }
    }

    fn manifest(&self) -> SourceBackedProManifest {
        SourceBackedProManifest::new(
            self.generation_id.clone(),
            vec![self.source.clone()],
            Vec::new(),
        )
        .expect("fixture manifest")
    }
}

#[derive(Default)]
struct FixtureProvider {
    pages: Vec<SourceBackedProviderPage>,
    requests: Vec<Option<SourceFrontier>>,
}

impl SourceBackedProProvider for FixtureProvider {
    fn reread_source_page(
        &mut self,
        source: &CertifiedSource,
        expected_prior_frontier: Option<&SourceFrontier>,
    ) -> Result<SourceBackedProviderPage> {
        self.requests.push(expected_prior_frontier.cloned());
        self.pages
            .iter()
            .find(|page| {
                page.source
                    .exact_descriptor_eq(source.observation().source())
                    && page.expected_prior_frontier.as_ref() == expected_prior_frontier
            })
            .cloned()
            .ok_or_else(|| anyhow!("fixture provider has no matching source page"))
    }
}

struct FixtureConsumer {
    materializer_revision: String,
    progress: BTreeMap<[u8; 32], SourceBackedProProgress>,
    durable_event_ids: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>,
    transient_record_digests: Vec<[u8; 32]>,
    dispositions: Vec<SourceBackedProDisposition>,
    deleted_epochs: Vec<u64>,
    finish_called: bool,
    corrupt_page_ack: bool,
    admission_header: Option<SourceManifestHeader>,
    admission_cursor: Option<ctx_pro_host_protocol::SourceManifestAdmissionCursor>,
    admission_replayed: Vec<bool>,
}

impl FixtureConsumer {
    fn new(progress: Vec<SourceBackedProProgress>) -> Self {
        Self {
            materializer_revision: MATERIALIZER_REVISION.to_owned(),
            progress: progress
                .into_iter()
                .map(|progress| (progress.source.identity().digest(), progress))
                .collect(),
            durable_event_ids: BTreeMap::new(),
            transient_record_digests: Vec::new(),
            dispositions: Vec::new(),
            deleted_epochs: Vec::new(),
            finish_called: false,
            corrupt_page_ack: false,
            admission_header: None,
            admission_cursor: None,
            admission_replayed: Vec::new(),
        }
    }

    fn durable_ids_for(&self, source: &SourceKey) -> BTreeSet<[u8; 32]> {
        self.durable_event_ids
            .get(&source.identity().digest())
            .cloned()
            .unwrap_or_default()
    }
}

impl SourceBackedProPageConsumer for FixtureConsumer {
    fn prepare_source(&mut self, request: &PrepareSourceRequest) -> Result<SourcePrepared> {
        assert_eq!(request.materializer_revision, self.materializer_revision);
        self.dispositions.push(request.disposition);
        let (source_epoch, frontier, terminal) = match request.disposition {
            SourceBackedProDisposition::NewSource => (1, None, false),
            SourceBackedProDisposition::Resume => {
                let prior = request.expected_prior.as_ref().expect("resume progress");
                (prior.source_epoch, prior.frontier.clone(), prior.terminal)
            }
            SourceBackedProDisposition::Rewrite => {
                let prior = request.expected_prior.as_ref().expect("rewrite progress");
                self.durable_event_ids
                    .remove(&request.source.identity().digest());
                (prior.source_epoch.saturating_add(1), None, false)
            }
        };
        Ok(SourcePrepared {
            core_generation_id: request.core_generation_id.clone(),
            progress: SourceBackedProProgress {
                source: request.source.clone(),
                source_epoch,
                certified_revision_sha256: request.certified_revision_sha256.clone(),
                frontier,
                materializer_revision: request.materializer_revision.clone(),
                terminal,
            },
            replayed: false,
        })
    }

    fn materialize_source_page(
        &mut self,
        request: &MaterializeSourcePageRequest,
    ) -> Result<SourcePageMaterialized> {
        let source_id = request.expected_prior.source.identity().digest();
        let durable = self.durable_event_ids.entry(source_id).or_default();
        for record in &request.records {
            self.transient_record_digests
                .push(Sha256::digest(serde_json::to_vec(&record.facts).unwrap()).into());
            durable.insert(record.event_id.digest());
        }
        let progress = request.next_progress();
        self.progress.insert(source_id, progress.clone());
        let accepted_records = u32::try_from(request.records.len())
            .expect("fixture page record count fits u32")
            .saturating_add(u32::from(self.corrupt_page_ack));
        let materialized_facts = request.records.iter().fold(0_u32, |total, record| {
            total.saturating_add(
                u32::try_from(record.facts.len()).expect("fixture fact count fits u32"),
            )
        });
        Ok(SourcePageMaterialized {
            core_generation_id: request.core_generation_id.clone(),
            progress,
            accepted_records,
            materialized_facts,
            replayed: false,
        })
    }

    fn delete_source(&mut self, request: &DeleteSourceRequest) -> Result<SourceDeleted> {
        assert!(request
            .removal
            .deletion
            .source()
            .exact_descriptor_eq(&request.expected_prior.source));
        let source_id = request.expected_prior.source.identity().digest();
        self.progress.remove(&source_id);
        self.durable_event_ids.remove(&source_id);
        self.deleted_epochs
            .push(request.expected_prior.source_epoch);
        Ok(SourceDeleted {
            core_generation_id: request.core_generation_id.clone(),
            source: request.expected_prior.source.clone(),
            removed_source_epoch: request.expected_prior.source_epoch,
            replayed: false,
        })
    }
}

impl SourceBackedProConsumer for FixtureConsumer {
    fn begin_source_manifest(
        &mut self,
        manifest: &SourceBackedProManifest,
    ) -> Result<SourceManifestBegan> {
        Ok(SourceManifestBegan {
            core_generation_id: manifest.core_generation_id.clone(),
            materializer_revision: self.materializer_revision.clone(),
            progress: self.progress.values().cloned().collect(),
            replayed: false,
        })
    }

    fn finish_source_manifest(
        &mut self,
        request: &FinishSourceManifestRequest,
    ) -> Result<SourceManifestFinished> {
        self.finish_called = true;
        self.progress = request
            .expected_progress
            .iter()
            .cloned()
            .map(|progress| (progress.source.identity().digest(), progress))
            .collect();
        Ok(SourceManifestFinished {
            receipt: SourceBackedProReceipt {
                core_generation_id: request.manifest.core_generation_id.clone(),
                manifest_aggregate_sha256: "b".repeat(64),
                materializer_revision: self.materializer_revision.clone(),
                progress: request.expected_progress.clone(),
            },
            replayed: false,
        })
    }
}

impl SourceBackedProAdmissionConsumer for FixtureConsumer {
    fn begin_source_manifest_admission(
        &mut self,
        header: &SourceManifestHeader,
    ) -> Result<SourceManifestAdmissionBegan> {
        let replayed = self.admission_header.as_ref() == Some(header);
        if !replayed {
            self.admission_header = Some(header.clone());
            self.admission_cursor =
                Some(ctx_pro_host_protocol::SourceManifestAdmissionCursor::initial(header));
        }
        self.admission_replayed.push(replayed);
        Ok(SourceManifestAdmissionBegan {
            cursor: self
                .admission_cursor
                .clone()
                .expect("fixture admission cursor"),
            replayed,
        })
    }

    fn admit_source_manifest_page(
        &mut self,
        page: &SourceManifestPage,
    ) -> Result<SourceManifestPageAdmitted> {
        let header = self
            .admission_header
            .as_ref()
            .expect("fixture admission header");
        let cursor = self
            .admission_cursor
            .as_ref()
            .expect("fixture admission cursor");
        let next = cursor_after_page(header, cursor, page)?;
        self.admission_cursor = Some(next.clone());
        Ok(SourceManifestPageAdmitted {
            cursor: next,
            replayed: false,
        })
    }

    fn finish_source_manifest_admission(
        &mut self,
        header: &SourceManifestHeader,
    ) -> Result<SourceManifestAdmitted> {
        assert_eq!(self.admission_header.as_ref(), Some(header));
        let cursor = self
            .admission_cursor
            .as_ref()
            .expect("fixture admission cursor");
        assert!(cursor.is_complete_for(header));
        Ok(SourceManifestAdmitted {
            receipt: ctx_pro_host_protocol::SourceManifestAdmissionReceipt {
                header: header.clone(),
                page_count: cursor.next_page_index,
            },
            materializer_revision: self.materializer_revision.clone(),
            progress: self.progress.values().cloned().collect(),
            replayed: false,
        })
    }

    fn finish_admitted_source_manifest(
        &mut self,
        request: &FinishAdmittedSourceManifestRequest,
    ) -> Result<SourceManifestFinished> {
        self.finish_called = true;
        self.progress = request
            .expected_progress
            .iter()
            .cloned()
            .map(|progress| (progress.source.identity().digest(), progress))
            .collect();
        Ok(SourceManifestFinished {
            receipt: SourceBackedProReceipt {
                core_generation_id: request.admission.header.core_generation_id.clone(),
                manifest_aggregate_sha256: request.admission.header.aggregate_sha256.clone(),
                materializer_revision: self.materializer_revision.clone(),
                progress: request.expected_progress.clone(),
            },
            replayed: false,
        })
    }
}

#[test]
fn source_backed_helper_negotiates_source_materialization_capability() {
    assert_eq!(
        source_backed_feed::source_materialization_capabilities(),
        BTreeSet::from([Capability::SourceMaterialization])
    );
}

#[test]
fn public_transport_exposes_only_the_paged_source_manifest_path() {
    let source = [
        include_str!("../client_output.rs"),
        include_str!("source_backed_feed.rs"),
    ]
    .join("\n");
    for forbidden in [
        ["Pro", "OutputImport"].concat(),
        ["ClientPro", "OutputSink"].concat(),
        ["Import", "Profile"].concat(),
        ["BeginOutput", "Inventory"].concat(),
        ["MaterializeOutput", "Page"].concat(),
        ["ProReplay", "Only"].concat(),
        ["sync_source_backed_pro_feed_", "deferred("].concat(),
        ["HostMessage::BeginSource", "Manifest("].concat(),
        ["HostMessage::FinishSource", "Manifest("].concat(),
    ] {
        assert!(
            !source.contains(&forbidden),
            "public Pro transport contains retired path {forbidden}"
        );
    }
    for required in [
        "sync_source_manifest_materialization",
        "BeginSourceManifestAdmission",
        "AdmitSourceManifestPage",
        "FinishSourceManifestAdmission",
        "FinishAdmittedSourceManifest",
    ] {
        assert!(
            source.contains(required),
            "public Pro transport is missing {required}"
        );
    }
}

#[test]
fn source_backed_pro_v026_uses_generation_pinned_paged_manifest_admission() {
    let fixture = public_codex_fixture();
    let mut provider = fixture.provider();
    let mut consumer = FixtureConsumer::new(Vec::new());
    let report = sync_source_backed_pro_feed_paged(
        fixture.manifest(),
        &fixture.generation_manifest,
        &mut provider,
        &mut consumer,
    )
    .expect("paged source admission and catch-up");

    assert!(consumer.finish_called);
    assert_eq!(report.receipt.core_generation_id, fixture.generation_id);
    assert_eq!(
        report.receipt.manifest_aggregate_sha256,
        consumer
            .admission_header
            .as_ref()
            .expect("admission header")
            .aggregate_sha256
    );
    assert_eq!(
        consumer
            .admission_cursor
            .as_ref()
            .expect("admission cursor")
            .next_source_index,
        1
    );
}

#[test]
fn paged_manifest_restart_replays_admission_and_reuses_terminal_source_progress() {
    let fixture = public_codex_fixture();
    let mut consumer = FixtureConsumer::new(Vec::new());
    let mut first_provider = fixture.provider();
    let first = sync_source_backed_pro_feed_paged(
        fixture.manifest(),
        &fixture.generation_manifest,
        &mut first_provider,
        &mut consumer,
    )
    .expect("initial paged source sync");
    let first_receipt = first.receipt;

    let mut restarted_provider = fixture.provider();
    let restarted = sync_source_backed_pro_feed_paged(
        fixture.manifest(),
        &fixture.generation_manifest,
        &mut restarted_provider,
        &mut consumer,
    )
    .expect("restarted paged source sync");

    assert_eq!(consumer.admission_replayed, [false, true]);
    assert_eq!(restarted.reread_pages, 0);
    assert_eq!(restarted.reread_records, 0);
    assert!(restarted_provider.requests.is_empty());
    assert_eq!(restarted.receipt, first_receipt);
}

#[test]
fn source_backed_pro_new_and_lagging_reread_from_independent_frontiers() {
    let fixture = public_codex_fixture();

    let mut new_provider = fixture.provider();
    let mut new_consumer = FixtureConsumer::new(Vec::new());
    let new_report =
        sync_source_backed_pro_feed(fixture.manifest(), &mut new_provider, &mut new_consumer)
            .expect("new Pro source catch-up");

    assert_eq!(
        new_consumer.dispositions,
        [SourceBackedProDisposition::NewSource]
    );
    assert_eq!(new_report.reread_pages, 2);
    assert_eq!(new_report.reread_records, 2);
    assert_eq!(new_report.prepared_sources, 1);
    assert_eq!(new_report.receipt.core_generation_id, fixture.generation_id);
    assert_eq!(
        new_provider.requests,
        [None, Some(fixture.intermediate_frontier.clone())]
    );
    assert_eq!(new_consumer.transient_record_digests.len(), 2);
    assert_eq!(
        new_consumer.durable_ids_for(fixture.source.observation().source()),
        fixture
            .records
            .iter()
            .map(|record| record.event_id.digest())
            .collect()
    );

    let revision = certified_source_revision_sha256(&fixture.source).expect("source revision");
    let lagging_progress = SourceBackedProProgress {
        source: fixture.source.observation().source().clone(),
        source_epoch: 7,
        certified_revision_sha256: revision,
        frontier: Some(fixture.intermediate_frontier.clone()),
        materializer_revision: MATERIALIZER_REVISION.to_owned(),
        terminal: false,
    };
    let mut lagging_provider = fixture.provider();
    let mut lagging_consumer = FixtureConsumer::new(vec![lagging_progress]);
    lagging_consumer
        .durable_event_ids
        .entry(fixture.source.observation().source().identity().digest())
        .or_default()
        .insert(fixture.records[0].event_id.digest());
    let lagging_report = sync_source_backed_pro_feed(
        fixture.manifest(),
        &mut lagging_provider,
        &mut lagging_consumer,
    )
    .expect("lagging Pro source catch-up");

    assert_eq!(
        lagging_consumer.dispositions,
        [SourceBackedProDisposition::Resume]
    );
    assert_eq!(
        lagging_provider.requests,
        [Some(fixture.intermediate_frontier)]
    );
    assert_eq!(lagging_report.reread_pages, 1);
    assert_eq!(lagging_report.reread_records, 1);
    assert_eq!(lagging_report.rewritten_sources, 0);
    assert_eq!(
        lagging_consumer.durable_ids_for(fixture.source.observation().source()),
        fixture
            .records
            .iter()
            .map(|record| record.event_id.digest())
            .collect()
    );
}

#[test]
fn source_backed_pro_rewrite_invalidates_old_epoch_before_reread() {
    let fixture = public_codex_fixture();
    let rewritten = rewritten_certificate(&fixture.source);
    let old_revision = certified_source_revision_sha256(&fixture.source).expect("old revision");
    let prior = SourceBackedProProgress {
        source: fixture.source.observation().source().clone(),
        source_epoch: 11,
        certified_revision_sha256: old_revision,
        frontier: fixture.source.frontier().cloned(),
        materializer_revision: "pro-source-materializer-v0".to_owned(),
        terminal: true,
    };
    let source_id = prior.source.identity().digest();
    let retained_record = fixture.records[1].clone();
    let mut consumer = FixtureConsumer::new(vec![prior]);
    consumer.durable_event_ids.insert(
        source_id,
        fixture
            .records
            .iter()
            .map(|record| record.event_id.digest())
            .collect(),
    );
    let mut provider = FixtureProvider {
        pages: vec![SourceBackedProviderPage {
            source: rewritten.observation().source().clone(),
            expected_prior_frontier: None,
            next_frontier: rewritten.frontier().cloned(),
            terminal: true,
            records: vec![retained_record.clone()],
        }],
        requests: Vec::new(),
    };
    let manifest =
        SourceBackedProManifest::new("c".repeat(64), vec![rewritten], Vec::new()).unwrap();

    let report = sync_source_backed_pro_feed(manifest, &mut provider, &mut consumer)
        .expect("rewrite catch-up");

    assert_eq!(consumer.dispositions, [SourceBackedProDisposition::Rewrite]);
    assert_eq!(report.rewritten_sources, 1);
    assert_eq!(provider.requests, [None]);
    assert_eq!(
        consumer.durable_ids_for(fixture.source.observation().source()),
        BTreeSet::from([retained_record.event_id.digest()])
    );
    assert!(!consumer
        .durable_ids_for(fixture.source.observation().source())
        .contains(&fixture.records[0].event_id.digest()));
    assert_eq!(report.receipt.progress[0].source_epoch, 12);
}

#[test]
fn source_backed_pro_deletion_requires_certified_complete_inventory() {
    let fixture = public_codex_fixture();
    let source = fixture.source.observation().source().clone();
    let prior = SourceBackedProProgress {
        source: source.clone(),
        source_epoch: 17,
        certified_revision_sha256: certified_source_revision_sha256(&fixture.source).unwrap(),
        frontier: fixture.source.frontier().cloned(),
        materializer_revision: MATERIALIZER_REVISION.to_owned(),
        terminal: true,
    };
    let inventory_observation = SourceInventoryObservation::new(
        source.provider(),
        "fixture-codex-root",
        TypedKey::utf8("public-codex-fixture").unwrap(),
        "fixture-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        inventory_observation.clone(),
        inventory_observation,
        "fixture-discovery-v1",
        Vec::new(),
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    let removal = SourceBackedProRemoval::new(deletion, inventory).unwrap();
    let manifest = SourceBackedProManifest::new("d".repeat(64), Vec::new(), vec![removal]).unwrap();
    let mut provider = FixtureProvider::default();
    let mut consumer = FixtureConsumer::new(vec![prior.clone()]);
    consumer.durable_event_ids.insert(
        source.identity().digest(),
        BTreeSet::from([fixture.records[0].event_id.digest()]),
    );

    let report = sync_source_backed_pro_feed(manifest, &mut provider, &mut consumer)
        .expect("certified deletion");

    assert_eq!(report.deleted_sources, 1);
    assert_eq!(consumer.deleted_epochs, [17]);
    assert!(consumer.durable_ids_for(&source).is_empty());
    assert!(consumer.finish_called);
    assert!(report.receipt.progress.is_empty());

    let manifest_without_proof =
        SourceBackedProManifest::new("e".repeat(64), Vec::new(), Vec::new()).unwrap();
    let mut provider = FixtureProvider::default();
    let mut consumer = FixtureConsumer::new(vec![prior]);
    let error = sync_source_backed_pro_feed(manifest_without_proof, &mut provider, &mut consumer)
        .expect_err("missing deletion proof must fail");

    assert!(error.to_string().contains("without a certified deletion"));
    assert!(!consumer.finish_called);
}

#[test]
fn source_backed_pro_mismatched_page_ack_never_publishes_receipt() {
    let fixture = public_codex_fixture();
    let mut provider = fixture.provider();
    let mut consumer = FixtureConsumer::new(Vec::new());
    consumer.corrupt_page_ack = true;

    let error = sync_source_backed_pro_feed(fixture.manifest(), &mut provider, &mut consumer)
        .expect_err("wrong CAS acknowledgement");

    assert!(error.to_string().contains("wrong source page CAS"));
    assert!(!consumer.finish_called);
}

fn public_codex_fixture() -> FixtureSourceFeed {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_dir = if manifest_dir.is_absolute() {
        manifest_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .expect("Bazel test current directory")
            .join(manifest_dir)
    };
    let public_fixture_root =
        fs::canonicalize(manifest_dir.join("../../tests/fixtures/provider-history/codex-sessions"))
            .expect("canonical public Codex fixture root");
    let temp = tempdir().expect("temporary source-backed index");
    let fixture_root = temp.path().join("sessions");
    let fixture_day = fixture_root.join("2026/06/23");
    fs::create_dir_all(&fixture_day).expect("fixture destination");
    for filename in ["root.jsonl", "subagent.jsonl"] {
        fs::copy(
            public_fixture_root.join("2026/06/23").join(filename),
            fixture_day.join(filename),
        )
        .expect("copy public Codex fixture into ordinary test files");
    }
    let index_root = temp.path().join("index");
    ingest_codex_source_backed_v0(&fixture_root, &index_root).expect("ingest public Codex fixture");
    let index = VerifiedIndex::open(&index_root).expect("open fixture source manifest");
    let candidate = index
        .search_event_candidates("Follow repo instructions", 1)
        .expect("search fixture")
        .into_iter()
        .next()
        .expect("fixture event");
    let events = index
        .events_for_session(candidate.event.session_id.as_uuid())
        .expect("fixture session events");
    assert!(events.len() >= 2);
    let source_key = events[0].locator.source().clone();
    let source = index
        .manifest()
        .sources
        .iter()
        .find(|source| {
            source
                .observation()
                .source()
                .exact_descriptor_eq(&source_key)
        })
        .expect("fixture source certificate")
        .clone();
    let resolver =
        CodexLocatorResolverV0::discover([&fixture_root]).expect("fixture locator resolver");
    let records = events
        .into_iter()
        .take(2)
        .map(|event| {
            let hydrated = resolver
                .hydrate(&event.locator)
                .expect("hydrate fixture record");
            let provider_record: serde_json::Value =
                serde_json::from_slice(&hydrated.provider_bytes)
                    .expect("parse hydrated fixture record");
            let detector_message = provider_record
                .pointer("/payload/content")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !detector_message.is_empty(),
                "fixture provider adapter must normalize detector message content"
            );
            SourceBackedProRecord::new(
                event.event_id,
                event.session_id,
                event.locator,
                SourceSessionRelationships {
                    direct_session_id: event.session_id,
                    root_session_id: event.session_id,
                    parent_session_id: None,
                    provider_session_id: event.provider_session_id,
                    agent_id: None,
                },
                None,
                SourceRecordMetadata {
                    event_sequence: event.event_sequence,
                    occurred_at_unix_ms: event.occurred_at_unix_ms,
                    event_type: event.event_type,
                    role: event.role,
                    workspace: event.workspace,
                    cwd: event.cwd,
                    touched_files: event.touched_files,
                },
                vec![TransientSourceFact::Message(SourceMessageFact {
                    content: TransientSourceContent::from_bytes(detector_message.as_bytes())
                        .expect("fixture detector content bound"),
                })],
            )
            .expect("source-backed Pro record")
        })
        .collect::<Vec<_>>();
    let intermediate_frontier = SourceFrontier::new("fixture-event", TypedKey::U64(1), 1, [1; 32])
        .expect("intermediate source frontier");
    FixtureSourceFeed {
        generation_id: index.generation_id().to_owned(),
        generation_manifest: index.manifest().clone(),
        source,
        records,
        intermediate_frontier,
    }
}

fn rewritten_certificate(base: &CertifiedSource) -> CertifiedSource {
    let source = base.observation().source().clone();
    let observation = SourceObservation::new(source, "fixture-rewrite-v1", vec![9]).unwrap();
    let counts = ScannedSourceCounts {
        certified_bytes: base.counts().certified_bytes,
        ..base.counts()
    };
    let digest = [9; 32];
    let frontier = SourceFrontier::new(
        "fixture-rewrite-record",
        TypedKey::U64(counts.complete_records),
        counts.certified_bytes,
        digest,
    )
    .unwrap();

    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        base.parser_revision(),
        digest,
        counts,
        Some(frontier),
    )
    .unwrap()
}
