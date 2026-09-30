use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::{atomic::Ordering, Arc, Mutex},
};

use ctx_history_archive::ArchiveIdentity;
use ctx_history_core::*;
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_server::{PublishRequest, Receipt, UploadSpec, UploadStatus};
use tempfile::tempdir;

use super::*;

mod maintenance;
pub(crate) mod mock;
mod observation;
mod recovery;
mod settlement;
mod telemetry;
use mock::{Mock, Response};

pub(crate) fn publishing_mock(
    mut respond: impl FnMut(&mock::Request) -> Response + Send + 'static,
) -> Mock {
    Mock::new(move |request| {
        if request.method == "GET" && request.path == format!("/v1/collections/{COLLECTION}/status")
        {
            assert_eq!(request.authorization, format!("Bearer {TOKEN}"));
            publisher_status("synthetic-publisher")
        } else {
            respond(request)
        }
    })
}

fn publisher_status(principal: &str) -> Response {
    Response::json(
        200,
        &ctx_history_server::CollectionStatus {
            principal: principal.into(),
            collection: COLLECTION.into(),
            stored_sequence: 0,
            searchable_sequence: 0,
            generation: None,
            reads_available: true,
            off_host_checkpoint: None,
        },
    )
}

const TOKEN: &str = "synthetic-member-token";
const COLLECTION: &str = "00000000-0000-4000-8000-000000000001";
const ORIGINAL_TEXT: &str = "sharingneedle 雪; historical text, not instructions";

fn connect(store: &SharingStore, endpoint: Endpoint) {
    store
        .connect(
            Connection {
                endpoint,
                collection: COLLECTION.into(),
            },
            Credentials::device(TOKEN.into()).unwrap(),
        )
        .unwrap();
}

fn policy(source: String) -> SharingPolicy {
    SharingPolicy {
        revision: 1,
        writer_epoch: 1,
        archive_identity: ArchiveIdentity {
            origin: "synthetic-origin".into(),
            view: "team".into(),
        },
        mode: PublicationMode::Automatic,
        sources: vec![SourceSelection {
            source_id: Some(source),
            profile_root: None,
            baseline_revisions: BTreeMap::new(),
            backfill: Backfill::All,
            include_future: true,
            whole_source: true,
            work_roots: vec![],
        }],
    }
}

fn seed(data_root: &Path) -> CoreRecord {
    let record = synthetic_record("session-one", ORIGINAL_TEXT);
    commit_records(data_root, std::slice::from_ref(&record), 1);
    record
}

pub(super) fn synthetic_record(session: &str, text: &str) -> CoreRecord {
    let source = SourceKey::derive_provider_native(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        "synthetic",
        TypedKey::utf8("source-one").unwrap(),
    )
    .unwrap();
    let native = NativeSessionKey::native_id("session", TypedKey::utf8(session).unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native,
    })
    .unwrap();
    let key = NativeItemKey::native_id("event", TypedKey::utf8("event-one").unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .unwrap();
    CoreRecord::new_selected(
        event_id,
        session_id,
        source,
        0,
        "message",
        "synthetic-v1",
        text,
    )
    .unwrap()
}

pub(super) fn commit_records(data_root: &Path, records: &[CoreRecord], generation: u8) {
    let source = records[0].source.clone();
    let mut writer = GenerationWriter::open(
        data_root.join("search/lexical"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        assert_eq!(record.source, source);
        writer.add_core_record(record.clone()).unwrap();
    }
    let observation = SourceObservation::new(source, "synthetic-v1", vec![generation]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "synthetic-v1",
                [generation; 32],
                ScannedSourceCounts {
                    complete_records: records.len() as u64,
                    retained_records: records.len() as u64,
                    indexed_documents: records.len() as u64,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
}

pub(super) fn make_ready(
    root: &Path,
    endpoint: Endpoint,
) -> (SharingStore, Collector, SharingPolicy) {
    let data = root.join("data");
    let record = seed(&data);
    let store = SharingStore::new(data.join("sharing/team"));
    connect(&store, endpoint);
    let policy = policy(capture::hex(&record.source.identity().digest()));
    store.set_policy(policy.clone()).unwrap();
    let collector = Collector::new(data, store.root().to_owned());
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(store.status().unwrap().pending, 1);
    (store, collector, policy)
}

fn expire_backoff(store: &SharingStore) {
    let path = store.pending_paths().unwrap().pop().unwrap();
    let mut pending: queue::Pending = private_file::read(&path.join("pending.json")).unwrap();
    pending.retry_at = 0;
    pending.save(&path).unwrap();
}

#[test]
fn default_connect_and_status_never_send_or_start_local_history() {
    let temp = tempdir().unwrap();
    let server = Mock::new(|_| panic!("unexpected network request"));
    let root = temp.path().join("data/sharing/team");
    let store = SharingStore::new(&root);
    let collector = Collector::new(temp.path().join("data"), root.clone());
    assert_eq!(collector.tick(), TickOutcome::Disabled);
    assert!(!store.status().unwrap().connected);
    assert!(!root.exists());
    assert!(SharingWorker::start(temp.path().join("data"), root)
        .unwrap()
        .is_none());
    connect(&store, server.endpoint());
    assert_eq!(collector.tick(), TickOutcome::Disabled);
    assert!(store.status().unwrap().connected);
    assert!(!store.status().unwrap().enabled);
    assert!(!temp.path().join("data/search").exists());
    assert_eq!(server.requests().len(), 0);
}

#[test]
fn policies_hold_mixed_unknown_and_new_roots_but_explicit_future_whole_source_works() {
    let temp = tempdir().unwrap();
    let work = temp.path().join("work α");
    let private = temp.path().join("private");
    let mut policy = policy("selected-source".into());
    let mut scope = SessionScope {
        source_id: "selected-source".into(),
        profile_roots: vec![],
        session_id: "future-session".into(),
        revision: "revision-one".into(),
        work_roots: vec![],
        unknown_work_root: true,
        first_event_unix_ms: None,
    };
    assert_eq!(policy.select(&scope), SelectionDecision::Selected);
    policy.sources[0].whole_source = false;
    policy.sources[0].work_roots = vec![work.clone()];
    assert_eq!(policy.select(&scope), SelectionDecision::UnknownWorkRoot);
    scope.work_roots = vec![work.join("nested"), private];
    scope.unknown_work_root = false;
    assert_eq!(policy.select(&scope), SelectionDecision::OutsideWorkRoots);
    scope.work_roots.pop();
    assert_eq!(policy.select(&scope), SelectionDecision::Selected);
    policy.sources[0].profile_root = Some(temp.path().join("profile"));
    assert_eq!(policy.select(&scope), SelectionDecision::ChangedProfile);
    scope.profile_roots = vec![temp.path().join("profile")];
    assert_eq!(policy.select(&scope), SelectionDecision::Selected);
    policy.sources[0]
        .baseline_revisions
        .insert(scope.session_id.clone(), "original-revision".into());
    policy.sources[0].backfill = Backfill::None;
    assert_eq!(policy.select(&scope), SelectionDecision::BackfillExcluded);
    policy.sources[0].backfill = Backfill::Since { unix_ms: 10 };
    assert_eq!(policy.select(&scope), SelectionDecision::BackfillExcluded);
    scope.first_event_unix_ms = Some(11);
    policy.sources[0].include_future = false;
    assert_eq!(policy.select(&scope), SelectionDecision::FutureExcluded);
    policy.sources[0].include_future = true;
    policy.mode = PublicationMode::Reviewed {
        revisions: BTreeSet::new(),
    };
    assert_eq!(policy.select(&scope), SelectionDecision::NeedsReview);
}

#[test]
fn narrowed_or_paused_policy_blocks_queued_bytes_then_current_authorized_scope_resumes() {
    let temp = tempdir().unwrap();
    let expected = Arc::new(Mutex::new(0_u64));
    let mock_expected = expected.clone();
    let server = publishing_mock(move |request| {
        assert!(request.authorization.ends_with(TOKEN));
        if request.method == "POST" {
            let spec: UploadSpec = serde_json::from_slice(&request.body).unwrap();
            *mock_expected.lock().unwrap() = spec.bytes;
            Response::json(
                200,
                &UploadStatus {
                    publisher: "synthetic-publisher".into(),
                    id: "upload-a".into(),
                    received_bytes: 0,
                    expected_bytes: spec.bytes,
                    expires_at: u64::MAX,
                },
            )
        } else {
            assert!(request.path.ends_with("?offset=0"));
            Response::json(
                200,
                &UploadStatus {
                    publisher: "synthetic-publisher".into(),
                    id: "upload-a".into(),
                    received_bytes: request.body.len() as u64,
                    expected_bytes: *mock_expected.lock().unwrap(),
                    expires_at: u64::MAX,
                },
            )
        }
    });
    let (store, collector, mut policy) = make_ready(temp.path(), server.endpoint());
    assert_eq!(collector.tick(), TickOutcome::Progress); // begin upload
    store.pause(true).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Paused);
    assert_eq!(server.requests().len(), 2); // Authentication, then begin upload.
    policy.revision = 2;
    policy.sources[0].whole_source = false;
    policy.sources[0].work_roots = vec![temp.path().join("work")];
    store.set_policy(policy.clone()).unwrap();
    store.pause(false).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Idle);
    assert_eq!(store.status().unwrap().held, 1);
    assert_eq!(server.requests().len(), 2);
    policy.revision = 3;
    policy.sources[0].whole_source = true;
    policy.sources[0].work_roots.clear();
    store.set_policy(policy).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn lost_chunk_and_acceptance_acknowledgements_resume_same_bytes_and_receipt_after_restart() {
    let temp = tempdir().unwrap();
    let spec = Arc::new(Mutex::new(None::<UploadSpec>));
    let retained = Arc::new(Mutex::new(Vec::<u8>::new()));
    let receipt = Arc::new(Mutex::new(None::<Receipt>));
    let (s, b, r) = (spec.clone(), retained.clone(), receipt.clone());
    let server =
        publishing_mock(
            move |request| match (request.method.as_str(), request.path.as_str()) {
                ("POST", path) if path.ends_with("/uploads") => {
                    let value: UploadSpec = serde_json::from_slice(&request.body).unwrap();
                    let status = UploadStatus {
                        publisher: "synthetic-publisher".into(),
                        id: "upload-a".into(),
                        received_bytes: 0,
                        expected_bytes: value.bytes,
                        expires_at: u64::MAX,
                    };
                    *s.lock().unwrap() = Some(value);
                    Response::json(200, &status)
                }
                ("PUT", _) => {
                    *b.lock().unwrap() = request.body.clone();
                    Response::Drop
                }
                ("GET", path) if path.ends_with("/uploads/upload-a") => Response::json(
                    200,
                    &UploadStatus {
                        publisher: "synthetic-publisher".into(),
                        id: "upload-a".into(),
                        received_bytes: b.lock().unwrap().len() as u64,
                        expected_bytes: s.lock().unwrap().as_ref().unwrap().bytes,
                        expires_at: u64::MAX,
                    },
                ),
                ("POST", path) if path.ends_with("/revisions") => {
                    assert!(r.lock().unwrap().is_none(), "duplicate admission");
                    let publish: PublishRequest = serde_json::from_slice(&request.body).unwrap();
                    *r.lock().unwrap() = Some(Receipt {
                        collection: COLLECTION.into(),
                        publisher: "synthetic-publisher".into(),
                        operation: publish.operation,
                        sequence: 7,
                        kind: "publish".into(),
                        payload: s.lock().unwrap().clone(),
                        accepted_at: 1,
                    });
                    Response::Drop
                }
                ("GET", _) => Response::json(200, r.lock().unwrap().as_ref().unwrap()),
                _ => panic!("unexpected method"),
            },
        );
    let (store, collector, _) = make_ready(temp.path(), server.endpoint());
    assert_eq!(collector.tick(), TickOutcome::Progress); // begin
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable)); // chunk ack lost
    expire_backoff(&store);
    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    assert_eq!(restarted.tick(), TickOutcome::Progress); // reconcile offset
    assert_eq!(restarted.tick(), TickOutcome::Failed(Error::Unavailable)); // final ack lost
    assert_eq!(store.status().unwrap().pending, 1);
    let path = store.pending_paths().unwrap().pop().unwrap();
    assert_eq!(
        fs::read(path.join("payload")).unwrap(),
        *retained.lock().unwrap()
    );
    expire_backoff(&store);
    assert_eq!(restarted.tick(), TickOutcome::Progress); // historical receipt
    assert_eq!(store.status().unwrap().pending, 0);
    assert_eq!(store.status().unwrap().last_accepted_sequence, Some(7));
    assert_eq!(restarted.tick(), TickOutcome::Idle); // unchanged generation checkpoint
    assert_eq!(server.requests().len(), 6);
    let acknowledged = store
        .publication_checkpoint(
            &receipt
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .operation
                .publication,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        acknowledged.revision,
        receipt.lock().unwrap().as_ref().unwrap().operation.revision
    );
}

#[test]
fn remote_status_and_errors_need_no_local_index_and_never_echo_secrets() {
    let server = Mock::new(|_| Response::Raw(403, format!("{{\"message\":\"{TOKEN}\"}}"), vec![]));
    let client = RemoteClient::new(
        Connection {
            endpoint: server.endpoint(),
            collection: COLLECTION.into(),
        },
        Credentials::read_only(TOKEN.into()).unwrap(),
    )
    .unwrap();
    let error = client.status().unwrap_err();
    assert_eq!(error, Error::Forbidden);
    assert!(!format!("{client:?} {error:?} {error}").contains(TOKEN));
    assert_eq!(client.receipt("key").unwrap_err(), Error::MissingCredential);
    assert_eq!(server.requests().len(), 1);
    let good = Mock::new(|request| {
        assert_eq!(request.authorization, format!("Bearer {TOKEN}"));
        Response::Raw(
            200,
            format!(
                "{{\"principal\":\"synthetic-reader\",\"collection\":\"{COLLECTION}\",\"stored_sequence\":12,\"searchable_sequence\":10,\"generation\":null,\"reads_available\":true,\"off_host_checkpoint\":null}}"
            ),
            vec![],
        )
    });
    let reader = RemoteClient::new(
        Connection {
            endpoint: good.endpoint(),
            collection: COLLECTION.into(),
        },
        Credentials::read_only(TOKEN.into()).unwrap(),
    )
    .unwrap();
    let status = reader.status().unwrap();
    assert_eq!(status.principal, "synthetic-reader");
    assert_eq!(
        (status.stored_sequence, status.searchable_sequence),
        (12, 10)
    );
}

#[test]
fn endpoints_and_redirects_cannot_send_credentials_to_a_second_service() {
    for endpoint in [
        "http://example.test",
        "http://localhost:99",
        "https://name:password@example.test",
        "https://example.test/?token=secret",
        "https://example.test/#fragment",
        "https://example.test/prefix",
    ] {
        assert_eq!(
            Endpoint::parse(endpoint).unwrap_err(),
            Error::InvalidEndpoint
        );
    }
    assert!(Endpoint::parse("https://example.test").is_ok());
    assert!(Endpoint::parse("http://[::1]:7332").is_ok());
    let destination = Mock::new(|_| panic!("redirect followed"));
    let location = destination.endpoint().as_str().to_owned();
    let server = Mock::new(move |_| {
        Response::Raw(
            302,
            String::new(),
            vec![("Location".into(), location.clone())],
        )
    });
    let client = RemoteClient::new(
        Connection {
            endpoint: server.endpoint(),
            collection: COLLECTION.into(),
        },
        Credentials::device(TOKEN.into()).unwrap(),
    )
    .unwrap();
    assert_eq!(client.status().unwrap_err(), Error::HttpStatus(302));
    assert!(destination.requests().is_empty());
}

#[cfg(unix)]
#[test]
fn credential_files_are_private_and_symlinks_or_public_secrets_are_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempdir().unwrap();
    let store = SharingStore::new(temp.path().join("named"));
    connect(&store, Endpoint::parse("https://example.test").unwrap());
    assert_eq!(
        fs::metadata(store.root().join("settings.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let secret = temp.path().join("secret");
    fs::write(&secret, format!("{TOKEN}\n")).unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        Credentials::from_files(Some(&secret), None).unwrap_err(),
        Error::Credentials
    );
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Credentials::from_files(Some(&secret), None).is_ok());
    let link = temp.path().join("link");
    symlink(secret, &link).unwrap();
    assert_eq!(
        Credentials::from_files(Some(&link), None).unwrap_err(),
        Error::Credentials
    );
}

#[test]
fn worker_stop_is_observed_before_any_request() {
    let temp = tempdir().unwrap();
    let server = publishing_mock(|_| panic!("stopped collector sent request"));
    let (store, collector, _) = make_ready(temp.path(), server.endpoint());
    let stop = std::sync::atomic::AtomicBool::new(false);
    stop.store(true, Ordering::Release);
    assert_eq!(collector.tick_with_stop(&stop), TickOutcome::Idle);
    assert_eq!(store.status().unwrap().pending, 1);
    assert_eq!(server.requests().len(), 1); // Initial policy authentication only.
}

#[test]
fn policy_preparation_baselines_old_sessions_and_removal_disables_without_remote_calls() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let record = seed(&data);
    let server = publishing_mock(|_| panic!("preparation/removal made a network request"));
    let store = SharingStore::new(data.join("sharing/team"));
    connect(&store, server.endpoint());
    let mut selection = policy(capture::hex(&record.source.identity().digest())).sources;
    selection[0].backfill = Backfill::None;
    let prepared = store
        .prepare_policy(
            &data,
            &temp.path().join("preview-none"),
            PublicationMode::Automatic,
            selection.clone(),
        )
        .unwrap();
    assert!(store.policy().unwrap().is_none());
    assert_eq!(prepared.sources[0].baseline_revisions.len(), 1);
    assert_eq!(
        ctx_history_archive::verify(&temp.path().join("preview-none"))
            .unwrap()
            .members,
        1
    );
    assert!(server.requests().is_empty());
    store.set_policy(prepared.clone()).unwrap();
    let collector = Collector::new(data.clone(), store.root().to_owned());
    assert_eq!(collector.tick(), TickOutcome::Idle);
    assert_eq!(store.status().unwrap().pending, 0);
    selection[0].backfill = Backfill::All;
    let reviewed = store
        .prepare_policy(
            &data,
            &temp.path().join("preview-reviewed"),
            PublicationMode::Reviewed {
                revisions: BTreeSet::new(),
            },
            selection,
        )
        .unwrap();
    assert_eq!(reviewed.revision, 2);
    assert_eq!(reviewed.archive_identity, prepared.archive_identity);
    assert!(
        matches!(&reviewed.mode, PublicationMode::Reviewed { revisions } if revisions.len()==1)
    );
    store.set_policy(reviewed).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(store.status().unwrap().pending, 1);
    store.remove().unwrap();
    assert_eq!(collector.tick(), TickOutcome::Disabled);
    assert!(!store.root().join("settings.json").exists());
    assert!(!store.root().join("queue").exists());
    assert!(store.root().join("settings.lock").exists());
    assert_eq!(server.requests().len(), 1); // Initial policy authentication only.
}

#[test]
fn remote_search_event_and_session_preserve_provenance_paging_and_unavailability() {
    use ctx_history_server::{
        Citation, CitationKind, CollectionStatus, HostedEvent, HostedSearchHit, Provenance,
        SearchResponse, SessionPage,
    };
    let temp = tempdir().unwrap();
    let mut record = seed(&temp.path().join("fixture"));
    record.event_sequence = 7;
    record.occurred_at_unix_ms = Some(1_704_067_201_000);
    record.role = Some("user".into());
    let event = Citation {
        collection: COLLECTION.into(),
        publication: "publication-one".into(),
        revision: "revision-one".into(),
        id: record.event_id.as_uuid().to_string(),
        kind: CitationKind::Event,
    }
    .encode()
    .unwrap();
    let session = Citation {
        collection: COLLECTION.into(),
        publication: "publication-one".into(),
        revision: "revision-one".into(),
        id: record.session_id.as_uuid().to_string(),
        kind: CitationKind::Session,
    }
    .encode()
    .unwrap();
    let hosted = HostedEvent {
        provenance: Provenance {
            collection: COLLECTION.into(),
            publication: "publication-one".into(),
            revision: "revision-one".into(),
            publisher: "authenticated-member".into(),
            origin: "declared-origin".into(),
            view: "team".into(),
            source: record.source.clone(),
        },
        record,
        citation: event.clone(),
        session_citation: session.clone(),
        score: Some(1.0),
    };
    // No local index exists for the reader; this fixture index merely authored
    // the independently known response sent by the remote mock.
    fs::remove_dir_all(temp.path().join("fixture")).unwrap();
    let response_event = hosted.clone();
    let search_hit = HostedSearchHit {
        event_id: hosted.record.event_id.as_uuid(),
        session_id: hosted.record.session_id.as_uuid(),
        event_sequence: hosted.record.event_sequence,
        occurred_at_unix_ms: hosted.record.occurred_at_unix_ms,
        event_type: hosted.record.event_type.clone(),
        role: hosted.record.role.clone(),
        snippet: "sharingneedle 雪…".into(),
        snippet_truncated: true,
        content_status: hosted.record.content.policy_status.clone(),
        provenance: hosted.provenance.clone(),
        citation: hosted.citation.clone(),
        session_citation: hosted.session_citation.clone(),
        score: hosted.score,
    };
    let server = Mock::new(move |request| {
        if request.path.contains("/search?") {
            assert!(request.path.contains("q=needle+%E9%9B%AA"));
            Response::json(
                200,
                &SearchResponse {
                    status: CollectionStatus {
                        principal: "synthetic-reader".into(),
                        collection: COLLECTION.into(),
                        stored_sequence: 5,
                        searchable_sequence: 3,
                        generation: Some("g3".into()),
                        reads_available: true,
                        off_host_checkpoint: None,
                    },
                    results: vec![search_hit.clone()],
                    complete: true,
                    exhaustive: false,
                },
            )
        } else if request.path.contains("/events/") {
            Response::json(200, &response_event)
        } else if request.path.contains("cursor=") {
            Response::Raw(403, "{\"error\":\"revoked\"}".into(), vec![])
        } else {
            Response::json(
                200,
                &SessionPage {
                    events: vec![response_event.clone()],
                    next_cursor: Some("opaque-cursor".into()),
                },
            )
        }
    });
    let client = RemoteClient::new(
        Connection {
            endpoint: server.endpoint(),
            collection: COLLECTION.into(),
        },
        Credentials::read_only(TOKEN.into()).unwrap(),
    )
    .unwrap();
    let found = client.search("needle 雪", 20).unwrap();
    assert!(found.complete);
    assert!(!found.exhaustive);
    assert_eq!(found.status.stored_sequence, 5);
    assert_eq!(found.status.searchable_sequence, 3);
    let hit = &found.results[0];
    assert_eq!(hit.event_id, hosted.record.event_id.as_uuid());
    assert_eq!(hit.session_id, hosted.record.session_id.as_uuid());
    assert_eq!(hit.event_sequence, 7);
    assert_eq!(hit.occurred_at_unix_ms, Some(1_704_067_201_000));
    assert_eq!(hit.event_type, "message");
    assert_eq!(hit.role.as_deref(), Some("user"));
    assert_eq!(hit.snippet, "sharingneedle 雪…");
    assert!(hit.snippet_truncated);
    assert_eq!(hit.content_status, CoreContentPolicyStatus::Selected);
    assert_eq!(hit.citation, event);
    assert_eq!(hit.session_citation, session);
    assert_eq!(hit.score, Some(1.0));
    assert!(serde_json::to_value(hit).unwrap().get("record").is_none());
    assert!(!serde_json::to_string(&found)
        .unwrap()
        .contains("historical text"));
    assert_eq!(hit.provenance.publisher, "authenticated-member");
    assert_eq!(hit.provenance.source, hosted.record.source);
    assert_eq!(client.event(&hit.citation).unwrap().record, hosted.record);
    let page = client.session(&hit.session_citation, None, 50).unwrap();
    assert_eq!(page.events[0].citation, event);
    assert_eq!(page.events[0].record, hosted.record);
    assert_eq!(
        client
            .session(&session, page.next_cursor.as_deref(), 50)
            .unwrap_err(),
        Error::Forbidden
    );
}

#[test]
fn offline_backlog_does_not_retry_after_policy_narrows() {
    let temp = tempdir().unwrap();
    let (server, remote) = recovery::accepting_remote();
    let (store, collector, mut policy) = make_ready(temp.path(), server.endpoint());
    remote.lock().unwrap().failure = Some(503);
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
    expire_backoff(&store);
    policy.revision = 2;
    policy.sources[0].whole_source = false;
    policy.sources[0].work_roots = vec![temp.path().join("allowed-work")];
    store.set_policy(policy).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Idle);
    assert_eq!(collector.tick(), TickOutcome::Idle);
    assert_eq!(server.requests().len(), 2);
    assert_eq!(store.status().unwrap().held, 1);
}

#[test]
fn expired_staging_resends_retained_member_under_the_same_operation() {
    let temp = tempdir().unwrap();
    let mut expected = 0;
    let mut attempt = 0;
    let server = publishing_mock(move |request| {
        attempt += 1;
        if request.method == "POST" {
            expected = serde_json::from_slice::<UploadSpec>(&request.body)
                .unwrap()
                .bytes;
            Response::json(
                200,
                &UploadStatus {
                    publisher: "synthetic-publisher".into(),
                    id: format!("upload-{attempt}"),
                    received_bytes: 0,
                    expected_bytes: expected,
                    expires_at: u64::MAX,
                },
            )
        } else if attempt == 2 {
            Response::Raw(410, "{\"error\":\"expired\"}".into(), vec![])
        } else {
            assert!(request.path.contains("upload-3?offset=0"));
            Response::json(
                200,
                &UploadStatus {
                    publisher: "synthetic-publisher".into(),
                    id: "upload-3".into(),
                    received_bytes: request.body.len() as u64,
                    expected_bytes: expected,
                    expires_at: u64::MAX,
                },
            )
        }
    });
    let (store, collector, _) = make_ready(temp.path(), server.endpoint());
    let path = store.pending_paths().unwrap().pop().unwrap();
    let original: queue::Pending = private_file::read(&path.join("pending.json")).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::StagingExpired));
    expire_backoff(&store);
    for _ in 0..2 {
        assert_eq!(collector.tick(), TickOutcome::Progress);
    }
    let resumed: queue::Pending = private_file::read(&path.join("pending.json")).unwrap();
    assert_eq!(original.operation, resumed.operation);
    let requests = server.requests();
    assert_eq!(requests[2].body, requests[4].body);
    assert_eq!(requests[4].body, fs::read(path.join("payload")).unwrap());
    assert_eq!(store.status().unwrap().stored_sessions, 0);
}
