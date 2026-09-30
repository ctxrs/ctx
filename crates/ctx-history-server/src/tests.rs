use crate::*;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use ctx_history_archive::{ArchiveIdentity, Selection, SessionMember};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::Arc};
use tower::ServiceExt;

mod access;
mod access_migration;
mod cancellation;
mod cancellation_recovery;
mod credentials;
mod enrollment_expiry;
mod inventory;
mod predecessors;
mod recovery;
pub(crate) mod repair;
mod revisions;
mod search;
mod startup;
mod telemetry;

struct Fixture {
    identity: ArchiveIdentity,
    member: SessionMember,
    bytes: Vec<u8>,
}

fn fixture(root: &Path, name: &str, bodies: &[&str]) -> Fixture {
    let source = SourceKey::derive(
        "synthetic",
        "synthetic-jsonl",
        "v1",
        1,
        SourceAnchor::CatalogLineage(Sha256::digest(name.as_bytes()).into()),
    )
    .unwrap();
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session-1").unwrap()).unwrap();
    let session = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .unwrap();
    let mut writer = GenerationWriter::open(
        root.join("input"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (position, body) in bodies.iter().enumerate() {
        let key = NativeItemKey::native_id("event", TypedKey::utf8(position.to_string()).unwrap())
            .unwrap();
        let id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id: session,
            logical_item_kind: "message",
            native_item_key: &key,
            subrecord_selector: None,
        })
        .unwrap();
        let mut record = CoreRecord::new_selected(
            id,
            session,
            source.clone(),
            position as u64,
            "message",
            "synthetic-v1",
            *body,
        )
        .unwrap();
        record.role = Some("user".into());
        writer.add_core_record(record).unwrap();
    }
    let observation = SourceObservation::new(source, "synthetic-revision", vec![1]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "synthetic-v1",
                [1; 32],
                ScannedSourceCounts {
                    complete_records: bodies.len() as u64,
                    retained_records: bodies.len() as u64,
                    indexed_documents: bodies.len() as u64,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(root.join("input")).unwrap();
    let identity = ArchiveIdentity {
        origin: format!("origin-{name}"),
        view: "team".into(),
    };
    let archive = root.join("archive");
    ctx_history_archive::export(&index, &archive, identity.clone(), &Selection::default()).unwrap();
    let mut members = Vec::new();
    ctx_history_archive::visit_members(&archive, |member| {
        members.push(member);
        Ok(())
    })
    .unwrap();
    assert_eq!(members.len(), 1);
    let member = members.remove(0);
    let bytes = fs::read(archive.join(&member.path)).unwrap();
    Fixture {
        identity,
        member,
        bytes,
    }
}

fn bootstrap(root: &Path) -> (HistoryServer, TokenFile) {
    let server = HistoryServer::open(ServerConfig::new(root.join("server"))).unwrap();
    let path = root.join("device.json");
    server.bootstrap("team", &path).unwrap();
    let token = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    (server, token)
}

fn stage(
    server: &HistoryServer,
    token: &str,
    collection: &str,
    fixture: &Fixture,
    publication: &str,
    previous: Option<String>,
    key: &str,
) -> PublishRequest {
    let upload = server
        .begin_upload(
            token,
            collection,
            UploadSpec {
                sha256: fixture.member.sha256.clone(),
                bytes: fixture.member.bytes,
            },
        )
        .unwrap();
    let mut offset = 0;
    for chunk in fixture.bytes.chunks(1024) {
        let status = server
            .upload_chunk(token, collection, &upload.id, offset, chunk)
            .unwrap();
        offset = status.received_bytes;
    }
    PublishRequest {
        operation: Operation {
            idempotency_key: key.into(),
            publication: publication.into(),
            writer_epoch: 1,
            policy_revision: 1,
            expected_sequence: previous.as_ref().map(|_| {
                server
                    .publication_state(token, collection, publication)
                    .unwrap()
                    .sequence
            }),
            expected_revision: previous,
            revision: fixture.member.sha256.clone(),
        },
        identity: fixture.identity.clone(),
        member: fixture.member.clone(),
        upload: upload.id,
    }
}

fn find(
    server: &HistoryServer,
    token: &str,
    collection: &str,
    query: &str,
) -> Vec<HostedSearchHit> {
    server
        .search(
            token,
            collection,
            SearchRequest {
                q: query.into(),
                limit: 20,
            },
        )
        .unwrap()
        .results
}

#[test]
fn durable_receipt_retry_restart_and_searchable_prefix() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(&root.path().join("source"), "alpha", &["durable apricot"]);
    let (server, token) = bootstrap(root.path());
    let request = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &input,
        "pub-a",
        None,
        "accept-1",
    );
    let accepted = server
        .publish(&token.credential.secret, &token.collection, request.clone())
        .unwrap();
    assert_eq!(accepted.sequence, 1);
    assert_eq!(
        server
            .status(&token.credential.secret, &token.collection)
            .unwrap()
            .searchable_sequence,
        0
    );
    let retry = server
        .publish(&token.credential.secret, &token.collection, request.clone())
        .unwrap();
    assert_eq!(
        serde_json::to_value(retry).unwrap(),
        serde_json::to_value(&accepted).unwrap()
    );
    let mut conflict = request.clone();
    conflict.operation.policy_revision = 2;
    assert!(matches!(
        server.publish(&token.credential.secret, &token.collection, conflict),
        Err(Error::Conflict)
    ));
    drop(server);
    let server = HistoryServer::open(ServerConfig::new(root.path().join("server"))).unwrap();
    assert_eq!(server.local_health().unwrap().pending_operations, 1);
    assert_eq!(
        server
            .index_pending(&token.collection, 1)
            .unwrap()
            .searchable_sequence,
        1
    );
    let found = find(
        &server,
        &token.credential.secret,
        &token.collection,
        "apricot",
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].snippet, "durable apricot");
    assert_eq!(found[0].provenance.publisher, token.principal);
    let direct = server
        .read_event(
            &token.credential.secret,
            &token.collection,
            &found[0].citation,
        )
        .unwrap();
    assert_eq!(direct.record.content.meaningful_text(), "durable apricot");
    assert_eq!(direct.record.event_id.as_uuid(), found[0].event_id);
    server.rebuild_collection(&token.collection).unwrap();
    server.index_pending(&token.collection, 16).unwrap();
    assert_eq!(
        find(
            &server,
            &token.credential.secret,
            &token.collection,
            "apricot"
        )[0]
        .citation,
        found[0].citation
    );
}

#[test]
fn member_isolation_duplicate_owner_and_revocation_close_every_data_path() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(&root.path().join("source"), "beta", &["private pear"]);
    let (server, admin) = bootstrap(root.path());
    let invite = server
        .invite(
            &admin.credential.secret,
            &admin.collection,
            InviteRequest {
                principal: None,
                name: Some("member".into()),
                grants: Grants {
                    read: true,
                    publish: true,
                    manage: false,
                },
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 3600,
            },
        )
        .unwrap();
    let member = server.redeem(&invite.enrollment.secret).unwrap();
    assert!(matches!(
        server.redeem(&invite.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    let request = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &input,
        "owned",
        None,
        "key-a",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, request)
        .unwrap();
    server.index_pending(&admin.collection, 16).unwrap();
    let result = find(
        &server,
        &member.credential.secret,
        &admin.collection,
        "pear",
    )
    .remove(0);
    let duplicate = stage(
        &server,
        &member.credential.secret,
        &admin.collection,
        &input,
        "other-name",
        None,
        "key-b",
    );
    assert!(matches!(
        server.publish(&member.credential.secret, &admin.collection, duplicate),
        Err(Error::Conflict)
    ));
    let separate = server.create_collection("other collection").unwrap();
    server
        .set_grants(
            &member.principal,
            &separate,
            Grants {
                read: true,
                publish: true,
                manage: false,
            },
        )
        .unwrap();
    assert!(matches!(
        server.read_event(&member.credential.secret, &separate, &result.citation),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.status(&member.credential.secret, &separate),
        Err(Error::Forbidden)
    ));
    let separate_invite = server
        .invite(
            &admin.credential.secret,
            &separate,
            InviteRequest {
                principal: Some(member.principal.clone()),
                name: None,
                grants: Grants {
                    read: true,
                    publish: true,
                    manage: false,
                },
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 3600,
            },
        )
        .unwrap();
    let separate_device = server.redeem(&separate_invite.enrollment.secret).unwrap();
    assert_eq!(separate_device.principal, member.principal);
    assert_ne!(separate_device.credential.id, member.credential.id);
    server
        .status(&separate_device.credential.secret, &separate)
        .unwrap();
    assert!(matches!(
        server.read_event(
            &separate_device.credential.secret,
            &separate,
            &result.citation
        ),
        Err(Error::NotFound)
    ));
    server
        .revoke_member(
            &admin.credential.secret,
            &admin.collection,
            &member.principal,
        )
        .unwrap();
    assert!(matches!(
        server.read_event(
            &member.credential.secret,
            &admin.collection,
            &result.citation
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.read_session(
            &member.credential.secret,
            &admin.collection,
            &result.session_citation,
            SessionRequest::default()
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.status(&member.credential.secret, &admin.collection),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.search(
            &member.credential.secret,
            &admin.collection,
            SearchRequest {
                q: "pear".into(),
                limit: 20
            }
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.status(&member.credential.secret, &separate),
        Err(Error::Forbidden)
    ));
    server
        .status(&separate_device.credential.secret, &separate)
        .unwrap();
    server
        .admin_revoke_principal(&admin.credential.secret, &member.principal)
        .unwrap();
    for (token, collection) in [
        (&member.credential.secret, &admin.collection),
        (&separate_device.credential.secret, &separate),
    ] {
        assert!(matches!(
            server.whoami(token, collection),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.status(token, collection),
            Err(Error::Forbidden)
        ));
    }
    assert_eq!(
        find(&server, &admin.credential.secret, &admin.collection, "pear").len(),
        1
    );
    server.revoke_credential(&admin.credential.id).unwrap();
    assert!(matches!(
        server.receipt(&admin.credential.secret, &admin.collection, "key-a"),
        Err(Error::Forbidden)
    ));
}

#[test]
fn correction_exact_old_citations_pagination_withdrawal_and_stale_retry() {
    let root = tempfile::tempdir().unwrap();
    let first = fixture(
        &root.path().join("first"),
        "gamma",
        &["old plum one", "old plum two", "old plum three"],
    );
    let second = fixture(&root.path().join("second"), "gamma", &["corrected peach"]);
    let (server, token) = bootstrap(root.path());
    let request = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &first,
        "pub",
        None,
        "first",
    );
    server
        .publish(&token.credential.secret, &token.collection, request.clone())
        .unwrap();
    server.index_pending(&token.collection, 1).unwrap();
    let old = find(&server, &token.credential.secret, &token.collection, "plum");
    assert_eq!(old.len(), 3);
    let page = server
        .read_session(
            &token.credential.secret,
            &token.collection,
            &old[0].session_citation,
            SessionRequest {
                limit: 2,
                cursor: None,
            },
        )
        .unwrap();
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.events[0].record.event_sequence, 0);
    let last = server
        .read_session(
            &token.credential.secret,
            &token.collection,
            &old[0].session_citation,
            SessionRequest {
                limit: 2,
                cursor: page.next_cursor,
            },
        )
        .unwrap();
    assert_eq!(last.events.len(), 1);
    assert!(last.next_cursor.is_none());
    let correction = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &second,
        "pub",
        Some(first.member.sha256.clone()),
        "second",
    );
    server
        .publish(
            &token.credential.secret,
            &token.collection,
            correction.clone(),
        )
        .unwrap();
    assert!(matches!(
        server.read_event(
            &token.credential.secret,
            &token.collection,
            &old[0].citation
        ),
        Err(Error::Unavailable)
    ));
    server.index_pending(&token.collection, 1).unwrap();
    assert!(find(&server, &token.credential.secret, &token.collection, "plum").is_empty());
    assert_eq!(
        find(
            &server,
            &token.credential.secret,
            &token.collection,
            "peach"
        )
        .len(),
        1
    );
    let historical = server
        .read_event(
            &token.credential.secret,
            &token.collection,
            &old[0].citation,
        )
        .unwrap();
    assert!(historical
        .record
        .content
        .meaningful_text()
        .contains("old plum"));
    let withdrawal = WithdrawRequest {
        operation: Operation {
            idempotency_key: "withdraw".into(),
            publication: "pub".into(),
            writer_epoch: 1,
            policy_revision: 2,
            expected_revision: Some(second.member.sha256.clone()),
            expected_sequence: Some(2),
            revision: "withdrawn-v1".into(),
        },
    };
    server
        .withdraw(&token.credential.secret, &token.collection, withdrawal)
        .unwrap();
    assert!(
        !server
            .status(&token.credential.secret, &token.collection)
            .unwrap()
            .reads_available
    );
    // Lost-ack retries return history; they never reopen the tombstoned writer.
    assert_eq!(
        server
            .publish(&token.credential.secret, &token.collection, request)
            .unwrap()
            .sequence,
        1
    );
    server.index_pending(&token.collection, 1).unwrap();
    assert!(find(
        &server,
        &token.credential.secret,
        &token.collection,
        "peach"
    )
    .is_empty());
    assert!(matches!(
        server.read_event(
            &token.credential.secret,
            &token.collection,
            &old[0].citation
        ),
        Err(Error::NotFound)
    ));
    let mut stale = correction;
    stale.operation.idempotency_key = "different-key".into();
    assert!(matches!(
        server.publish(&token.credential.secret, &token.collection, stale),
        Err(Error::Conflict)
    ));
}

#[test]
fn correction_before_indexing_reconstructs_exact_bounded_prefixes() {
    let root = tempfile::tempdir().unwrap();
    let first = fixture(&root.path().join("first"), "delta", &["obsolete kiwi"]);
    let second = fixture(&root.path().join("second"), "delta", &["current mango"]);
    let (server, token) = bootstrap(root.path());
    let first_request = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &first,
        "pub",
        None,
        "first",
    );
    server
        .publish(&token.credential.secret, &token.collection, first_request)
        .unwrap();
    let second_request = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &second,
        "pub",
        Some(first.member.sha256),
        "second",
    );
    server
        .publish(&token.credential.secret, &token.collection, second_request)
        .unwrap();
    let first_status = server.index_pending(&token.collection, 1).unwrap();
    assert_eq!(first_status.searchable_sequence, 1);
    assert!(!first_status.reads_available);
    let second_status = server.index_pending(&token.collection, 1).unwrap();
    assert_eq!(second_status.searchable_sequence, 2);
    assert!(second_status.reads_available);
    assert!(find(&server, &token.credential.secret, &token.collection, "kiwi").is_empty());
    assert_eq!(
        find(
            &server,
            &token.credential.secret,
            &token.collection,
            "mango"
        )
        .len(),
        1
    );
}

#[test]
fn chunk_retry_incomplete_corrupt_and_expired_inputs_never_acknowledge() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("source"),
        "epsilon",
        &["valid synthetic berry"],
    );
    let (server, token) = bootstrap(root.path());
    let spec = UploadSpec {
        sha256: input.member.sha256.clone(),
        bytes: input.member.bytes,
    };
    let upload = server
        .begin_upload(&token.credential.secret, &token.collection, spec.clone())
        .unwrap();
    let split = input.bytes.len() / 2;
    let chunk = &input.bytes[..split];
    server
        .upload_chunk(
            &token.credential.secret,
            &token.collection,
            &upload.id,
            0,
            chunk,
        )
        .unwrap();
    assert_eq!(
        server
            .upload_chunk(
                &token.credential.secret,
                &token.collection,
                &upload.id,
                0,
                chunk
            )
            .unwrap()
            .received_bytes,
        split as u64
    );
    assert!(matches!(
        server.upload_chunk(
            &token.credential.secret,
            &token.collection,
            &upload.id,
            0,
            &vec![0; split]
        ),
        Err(Error::Conflict)
    ));
    let request = PublishRequest {
        operation: Operation {
            idempotency_key: "partial".into(),
            publication: "pub".into(),
            writer_epoch: 1,
            policy_revision: 1,
            expected_revision: None,
            expected_sequence: None,
            revision: input.member.sha256.clone(),
        },
        identity: input.identity.clone(),
        member: input.member.clone(),
        upload: upload.id.clone(),
    };
    assert!(matches!(
        server.publish(&token.credential.secret, &token.collection, request.clone()),
        Err(Error::Conflict)
    ));
    assert_eq!(
        server
            .status(&token.credential.secret, &token.collection)
            .unwrap()
            .stored_sequence,
        0
    );
    server
        .upload_chunk(
            &token.credential.secret,
            &token.collection,
            &upload.id,
            split as u64,
            &vec![0; input.bytes.len() - split],
        )
        .unwrap();
    assert!(matches!(
        server.publish(&token.credential.secret, &token.collection, request),
        Err(Error::Invalid(_))
    ));
    let fresh = server
        .begin_upload(&token.credential.secret, &token.collection, spec)
        .unwrap();
    server
        .lock()
        .unwrap()
        .execute("UPDATE uploads SET expires=0 WHERE id=?1", [&fresh.id])
        .unwrap();
    assert!(matches!(
        server.upload_status(&token.credential.secret, &token.collection, &fresh.id),
        Err(Error::Expired)
    ));
    assert_eq!(
        server
            .status(&token.credential.secret, &token.collection)
            .unwrap()
            .stored_sequence,
        0
    );
    let valid = stage(
        &server,
        &token.credential.secret,
        &token.collection,
        &input,
        "pub",
        None,
        "valid",
    );
    assert_eq!(
        server
            .publish(&token.credential.secret, &token.collection, valid)
            .unwrap()
            .sequence,
        1
    );
}

#[tokio::test]
async fn running_router_invite_enroll_grant_revoke_and_reject_unknown_filters() {
    let root = tempfile::tempdir().unwrap();
    let (server, admin) = bootstrap(root.path());
    let app = router(Arc::new(server));
    let invite = InviteRequest {
        principal: None,
        name: Some("reader and publisher".into()),
        grants: Grants {
            read: true,
            publish: true,
            manage: false,
        },
        enrollment_ttl_seconds: 600,
        credential_ttl_seconds: 3600,
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/collections/{}/invite", admin.collection))
                .header(
                    "authorization",
                    format!("Bearer {}", admin.credential.secret),
                )
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&invite).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let enrollment: EnrollmentFile =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/enroll")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&EnrollRequest {
                        enrollment: enrollment.enrollment.secret,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let member: TokenFile =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/collections/{}/search?q=hi&semantic=true",
                    admin.collection
                ))
                .header(
                    "authorization",
                    format!("Bearer {}", member.credential.secret),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/collections/{}/members/{}/revoke",
                    admin.collection, member.principal
                ))
                .header(
                    "authorization",
                    format!("Bearer {}", admin.credential.secret),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/collections/{}/status", admin.collection))
                .header(
                    "authorization",
                    format!("Bearer {}", member.credential.secret),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[test]
fn explicit_non_loopback_requires_trusted_ingress() {
    let root = tempfile::tempdir().unwrap();
    let mut config = ServerConfig::new(root.path().join("server"));
    config.bind = "0.0.0.0:7332".parse().unwrap();
    assert!(matches!(
        HistoryServer::open(config.clone()),
        Err(Error::Invalid(_))
    ));
    config.trusted_ingress = true;
    assert!(HistoryServer::open(config).is_ok());
}
