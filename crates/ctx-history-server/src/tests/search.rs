use super::*;
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CoreActivity,
    CoreContentPolicyStatus, CORE_ACTIVITY_REVISION,
};
use ctx_history_read_application::{SEARCH_SNIPPET_MAX_BYTES, SEARCH_SNIPPET_MAX_CHARS};

fn rewrite_records(input: &mut Fixture, mut update: impl FnMut(&mut CoreRecord)) {
    let mut bytes = Vec::new();
    for line in input
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut record = CoreRecord::decode_stored(line).unwrap();
        update(&mut record);
        bytes.extend(record.encode_stored().unwrap());
        bytes.push(b'\n');
    }
    input.member.sha256 = hex::encode(Sha256::digest(&bytes));
    input.member.bytes = bytes.len() as u64;
    input.bytes = bytes;
}

fn publish_and_index(server: &HistoryServer, token: &TokenFile, input: &Fixture) {
    let upload = server
        .begin_upload(
            &token.credential.secret,
            &token.collection,
            UploadSpec {
                sha256: input.member.sha256.clone(),
                bytes: input.member.bytes,
            },
        )
        .unwrap();
    // Exercise large retained records without thousands of tiny fsynced chunks.
    server
        .upload_chunk(
            &token.credential.secret,
            &token.collection,
            &upload.id,
            0,
            &input.bytes,
        )
        .unwrap();
    server
        .publish(
            &token.credential.secret,
            &token.collection,
            PublishRequest {
                operation: Operation {
                    idempotency_key: "publish".into(),
                    publication: "pub".into(),
                    writer_epoch: 1,
                    policy_revision: 1,
                    expected_revision: None,
                    expected_sequence: None,
                    revision: input.member.sha256.clone(),
                },
                identity: input.identity.clone(),
                member: input.member.clone(),
                upload: upload.id,
            },
        )
        .unwrap();
    server.index_pending(&token.collection, 16).unwrap();
}

fn query(server: &HistoryServer, token: &TokenFile, q: &str, limit: usize) -> SearchResponse {
    server
        .search(
            &token.credential.secret,
            &token.collection,
            SearchRequest { q: q.into(), limit },
        )
        .unwrap()
}

#[test]
fn ordinary_twenty_large_hits_have_relevant_snippets_and_full_exact_reads() {
    let root = tempfile::tempdir().unwrap();
    let body = format!(
        "{}The lateharvest decision retains café and 東京 exactly. End of retained event.",
        "Ordinary research notes. ".repeat(4096)
    );
    let mut input = fixture(
        &root.path().join("source"),
        "large-search",
        &[body.as_str(); 20],
    );
    rewrite_records(&mut input, |record| {
        record.occurred_at_unix_ms = Some(1_000 + record.event_sequence as i64);
    });
    assert!(input.bytes.len() > 1_800_000);
    let (server, token) = bootstrap(root.path());
    publish_and_index(&server, &token, &input);

    let response = query(&server, &token, "lateharvest", 20);
    assert_eq!(response.results.len(), 20);
    assert!(response.complete && response.exhaustive);
    let wire = serde_json::to_vec(&response).unwrap();
    assert!(wire.len() <= SEARCH_RESPONSE_MAX_BYTES);
    let json: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    for hit in json["results"].as_array().unwrap() {
        assert!(hit.get("record").is_none());
        assert!(hit.get("content").is_none());
    }
    for hit in &response.results {
        assert!(hit.snippet.contains("lateharvest"));
        assert!(hit.snippet_truncated);
        assert!(hit.snippet.chars().count() <= SEARCH_SNIPPET_MAX_CHARS);
        assert!(hit.snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
        assert_eq!(hit.content_status, CoreContentPolicyStatus::Selected);
        assert_eq!(hit.event_type, "message");
        assert_eq!(hit.role.as_deref(), Some("user"));
        assert_eq!(
            hit.occurred_at_unix_ms,
            Some(1_000 + hit.event_sequence as i64)
        );
        assert_eq!(hit.provenance.publisher, token.principal);
        assert_eq!(hit.provenance.revision, input.member.sha256);
        assert!(hit
            .provenance
            .source
            .exact_descriptor_eq(&input.member.source));
        assert!(hit
            .score
            .is_some_and(|score| score.is_finite() && score > 0.0));
        let exact = server
            .read_event(&token.credential.secret, &token.collection, &hit.citation)
            .unwrap();
        assert_eq!(exact.record.event_id.as_uuid(), hit.event_id);
        assert_eq!(exact.record.session_id.as_uuid(), hit.session_id);
        assert_eq!(exact.record.event_sequence, hit.event_sequence);
        assert_eq!(
            exact.record.content.normalized_body.as_deref(),
            Some(body.as_str())
        );
        assert_eq!(exact.citation, hit.citation);
        assert_eq!(exact.session_citation, hit.session_citation);
    }
    let session = server
        .read_session(
            &token.credential.secret,
            &token.collection,
            &response.results[0].session_citation,
            SessionRequest::default(),
        )
        .unwrap();
    assert_eq!(session.events.len(), 20);
    assert!(session.next_cursor.is_none());
    assert!(session
        .events
        .iter()
        .all(|event| { event.record.content.normalized_body.as_deref() == Some(body.as_str()) }));
}

#[test]
fn search_snippets_find_late_structured_and_activity_fragments() {
    let root = tempfile::tempdir().unwrap();
    let body = "Ordinary opening summary. ".repeat(100);
    let structured = serde_json::json!({
        "detail": format!("{}structuredclementine conclusion", "prior material ".repeat(400))
    });
    let activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("call-search").unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".into()),
            server: Some("synthetic".into()),
            tool: "lookup".into(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::json!({
                    "query": format!("{}invocationcurrant argument", "earlier input ".repeat(400))
                }),
            },
            started_at_unix_ms: None,
        }),
        result: Some(ActivityResult {
            status: Some("ok".into()),
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Present {
                value: format!("{}resultkumquat conclusion", "earlier output ".repeat(400)),
            },
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: Vec::new(),
    };
    let mut input = fixture(&root.path().join("source"), "fragments", &[&body, &body]);
    rewrite_records(&mut input, |record| {
        if record.event_sequence == 0 {
            record.content.structured_content = Some(structured.clone());
        } else {
            record.content.activity = Some(activity.clone());
        }
    });
    let (server, token) = bootstrap(root.path());
    publish_and_index(&server, &token, &input);
    for (term, sequence) in [
        ("structuredclementine", 0),
        ("invocationcurrant", 1),
        ("resultkumquat", 1),
    ] {
        assert!(!body.contains(term));
        let response = query(&server, &token, term, 20);
        assert!(response.complete && response.exhaustive);
        assert_eq!(response.results.len(), 1);
        let hit = &response.results[0];
        assert_eq!(hit.event_sequence, sequence);
        assert!(hit.snippet.contains(term));
        assert!(hit.snippet_truncated);
        assert!(hit.snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
        let exact = server
            .read_event(&token.credential.secret, &token.collection, &hit.citation)
            .unwrap();
        assert_eq!(
            exact.record.content.normalized_body.as_deref(),
            Some(body.as_str())
        );
        if sequence == 0 {
            assert_eq!(
                exact.record.content.structured_content,
                Some(structured.clone())
            );
        } else {
            assert_eq!(exact.record.content.activity, Some(activity.clone()));
        }
    }
}

#[test]
fn serialized_search_budget_counts_escaped_metadata_and_the_response_envelope() {
    assert_eq!(SEARCH_RESPONSE_MAX_BYTES, 128 * 1024);
    let mut role_bytes = 32_768;
    for expected_hits in [1, 0] {
        let root = tempfile::tempdir().unwrap();
        let role = "\\".repeat(role_bytes);
        let mut input = fixture(
            &root.path().join("source"),
            "byte-budget",
            &["bounded citrus", "bounded citrus"],
        );
        rewrite_records(&mut input, |record| record.role = Some(role.clone()));
        let (server, token) = bootstrap(root.path());
        publish_and_index(&server, &token, &input);
        let response = query(&server, &token, "citrus", 20);
        assert_eq!(response.results.len(), expected_hits);
        assert!(!response.complete && !response.exhaustive);
        assert_eq!(
            response.status.stored_sequence,
            response.status.searchable_sequence
        );
        assert!(serde_json::to_vec(&response).unwrap().len() <= SEARCH_RESPONSE_MAX_BYTES);
        if expected_hits == 0 {
            continue;
        }

        let hit = &response.results[0];
        assert_eq!(hit.snippet, "bounded citrus");
        assert!(!hit.snippet_truncated);
        assert_eq!(hit.role.as_deref(), Some(role.as_str()));
        let exact = server
            .read_event(&token.credential.secret, &token.collection, &hit.citation)
            .unwrap();
        assert_eq!(exact.record.role, Some(role));
        let limited = query(&server, &token, "citrus", 1);
        assert!(limited.complete && !limited.exhaustive);
        assert_eq!(limited.results.len(), 1);
        assert_eq!(limited.results[0].event_id, hit.event_id);

        // On the second pass a hit by itself fits, but its response does not.
        // IDs/digests have fixed encoded lengths and the authored metadata stays
        // identical, so this exercises the envelope rather than just hit bytes.
        let mut boundary = hit.clone();
        boundary.role = Some(String::new());
        let fixed_bytes = serde_json::to_vec(&boundary).unwrap().len();
        role_bytes = (SEARCH_RESPONSE_MAX_BYTES - fixed_bytes) / 2;
        assert!(role_bytes <= 64 * 1024);
        boundary.role = Some("\\".repeat(role_bytes));
        assert!(serde_json::to_vec(&boundary).unwrap().len() <= SEARCH_RESPONSE_MAX_BYTES);
        let mut over_budget = response;
        over_budget.results = vec![boundary];
        assert!(serde_json::to_vec(&over_budget).unwrap().len() > SEARCH_RESPONSE_MAX_BYTES);
    }
}
