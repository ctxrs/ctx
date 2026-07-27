use super::*;
use crate::{HostEnvelope, HostMessage, MAX_FRAME_PAYLOAD_BYTES};
use uuid::Uuid;

fn page_with_observations(observations: Vec<ProOutputObservation>) -> ProOutputMaterializationPage {
    ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 1,
        source: OutputSourceIdentity {
            provider: "test-provider".to_owned(),
            namespace_id: "test-namespace".to_owned(),
            source_id: "test-source".to_owned(),
        },
        source_epoch: 0,
        observed_revision: "revision-1".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::NewSource,
        expected_prior_source_epoch: None,
        expected_prior_cursor: None,
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: STANDARD.encode(b"cursor-1"),
        },
        terminal: false,
        observations,
    }
}

fn observation(sequence: u64, content: TransientOutputContent) -> ProOutputObservation {
    ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("unit-{sequence}"),
            native_sequence: sequence,
            native_record_id: Some(format!("record-{sequence}")),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: None,
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: None,
        associations: OutputAssociations {
            direct_session_id: "direct-session".to_owned(),
            root_session_id: "root-session".to_owned(),
            parent_session_id: None,
            provider_session_id: None,
            agent_id: None,
            repository: None,
        },
        call_id: None,
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome: OutputOutcome::Success,
            exit_code: Some(0),
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "provider-record".to_owned(),
            payload_base64: STANDARD.encode(b"locator-1"),
        },
        content,
    }
}

#[test]
fn accepted_sixteen_mibibyte_content_fits_the_self_contained_frame() {
    let content = vec![b'"'; MAX_OUTPUT_CONTENT_BYTES];
    let transient = TransientOutputContent::from_bytes(&content)
        .unwrap_or_else(|| panic!("contract maximum must be accepted"));
    let page = page_with_observations(vec![observation(1, transient)]);
    page.validate()
        .unwrap_or_else(|error| panic!("maximum page must validate: {error:?}"));
    let envelope = HostEnvelope {
        sequence: 1,
        request_id: Uuid::from_u128(1),
        message: HostMessage::MaterializeOutputPage(page),
    };
    let payload_len = serde_json::to_vec(&envelope)
        .map(|payload| payload.len())
        .unwrap_or_else(|error| panic!("output page must encode: {error}"));
    assert!(payload_len < MAX_FRAME_PAYLOAD_BYTES);
    assert!(TransientOutputContent::from_bytes(&vec![0; MAX_OUTPUT_CONTENT_BYTES + 1]).is_none());
}

#[test]
fn aggregate_metadata_that_exceeds_the_frame_is_rejected() {
    let empty = TransientOutputContent::from_bytes(b"")
        .unwrap_or_else(|| panic!("empty content must be accepted"));
    let observations = (1..=MAX_OUTPUT_OBSERVATIONS_PER_PAGE)
        .map(|sequence| {
            let mut observation = observation(sequence as u64, empty.clone());
            observation.command = Some(OutputCommandContext {
                tool_name: "exec_command".to_owned(),
                command: "x".repeat(MAX_OUTPUT_COMMAND_BYTES),
                working_directory: Some("/workspace".to_owned()),
            });
            observation
        })
        .collect();
    let page = page_with_observations(observations);

    assert_eq!(
        page.validate().map_err(|error| error.class),
        Err(ErrorClass::Bounds)
    );
}

#[test]
fn metadata_heavy_aggregate_requires_multiple_self_contained_pages() {
    let empty = TransientOutputContent::from_bytes(b"")
        .unwrap_or_else(|| panic!("empty content must be accepted"));
    let observations = (1..=64)
        .map(|sequence| {
            let mut observation = observation(sequence, empty.clone());
            observation.command = Some(OutputCommandContext {
                tool_name: "exec_command".to_owned(),
                command: "x".repeat(MAX_OUTPUT_COMMAND_BYTES),
                working_directory: Some("/workspace".to_owned()),
            });
            observation.locator.payload_base64 =
                STANDARD.encode(vec![b'l'; MAX_OUTPUT_LOCATOR_BYTES]);
            observation
        })
        .collect();
    let page = page_with_observations(observations);
    page.validate()
        .unwrap_or_else(|error| panic!("partitioned metadata page must validate: {error:?}"));
    let envelope = HostEnvelope {
        sequence: u64::MAX,
        request_id: Uuid::from_u128(1),
        message: HostMessage::MaterializeOutputPage(page),
    };
    let payload_len = serde_json::to_vec(&envelope)
        .map(|payload| payload.len())
        .unwrap_or_else(|error| panic!("metadata page must encode: {error}"));

    assert!(payload_len < MAX_FRAME_PAYLOAD_BYTES);
    assert!(payload_len.saturating_mul(4) > MAX_FRAME_PAYLOAD_BYTES);
}

#[test]
fn debug_redacts_complete_output_content() {
    let canary = b"TRANSIENT_OUTPUT_DEBUG_CANARY";
    let content = TransientOutputContent::from_bytes(canary)
        .unwrap_or_else(|| panic!("small content must be accepted"));
    let debug = format!("{content:?}");
    assert!(!debug.contains(std::str::from_utf8(canary).unwrap_or_default()));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn output_debug_redacts_paths_and_native_identities() {
    let canary = "OUTPUT_DEBUG_PRIVACY_CANARY";
    let mut observation = observation(
        1,
        TransientOutputContent::from_bytes(b"output")
            .unwrap_or_else(|| panic!("small content must be accepted")),
    );
    observation.coordinate.unit_key = format!("{canary}-unit");
    observation.coordinate.native_record_id = Some(format!("{canary}-record"));
    observation.associations.direct_session_id = format!("{canary}-direct");
    observation.associations.root_session_id = format!("{canary}-root");
    observation.associations.repository = Some(OutputRepositoryContext {
        repository_id: format!("{canary}-repository"),
        checkout_id: Some(format!("{canary}-checkout")),
        worktree_id: Some(format!("{canary}-worktree")),
        object_format: Some("sha256".to_owned()),
    });
    observation.command = Some(OutputCommandContext {
        tool_name: format!("{canary}-tool"),
        command: format!("read {canary}/secret"),
        working_directory: Some(format!("/{canary}/workspace")),
    });
    let mut page = page_with_observations(vec![observation]);
    page.source = OutputSourceIdentity {
        provider: format!("{canary}-provider"),
        namespace_id: format!("{canary}-namespace"),
        source_id: format!("{canary}-source"),
    };

    let debug = format!(
        "{page:?} {:?} {:?} {:?}",
        page.source, page.observations[0], page.observations[0].command
    );
    assert!(!debug.contains(canary));
    assert!(debug.contains("observation_count"));
    assert!(debug.contains("has_working_directory"));
}

#[test]
fn inventory_begin_materializer_revision_is_required_and_bounded() {
    let missing = OutputInventoryBegan {
        generation: 1,
        materializer_revision: String::new(),
    };
    assert!(missing.validate().is_err());

    let oversized = OutputInventoryBegan {
        generation: 1,
        materializer_revision: "r".repeat(MAX_OUTPUT_IDENTITY_BYTES + 1),
    };
    assert_eq!(
        oversized.validate().map_err(|error| error.class),
        Err(ErrorClass::Bounds)
    );
}

#[test]
fn certified_provider_cursor_bound_is_accepted_exactly() {
    let exact = OutputNativeCursor {
        version: 1,
        payload_base64: STANDARD.encode(vec![0_u8; MAX_OUTPUT_CURSOR_BYTES]),
    };
    exact
        .validate()
        .unwrap_or_else(|error| panic!("exact cursor bound must validate: {error:?}"));

    let oversized = OutputNativeCursor {
        version: 1,
        payload_base64: STANDARD.encode(vec![0_u8; MAX_OUTPUT_CURSOR_BYTES + 1]),
    };
    assert_eq!(
        oversized.validate().map_err(|error| error.class),
        Err(ErrorClass::Bounds)
    );
}
