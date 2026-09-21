use super::*;
use crate::core_materialization::provider_contract_test_support::{PROVIDERS, provider_record};
use crate::protocol::{
    ProviderNativeCopyProof, ProviderNativeEventCopy, SourceAnchor, SourceKey, TypedKey,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

macro_rules! current_positive {
    ($name:ident, $provider:literal) => {
        #[test]
        fn $name() -> TestResult {
            assert_eq!(
                producer_authority_disposition(&provider_record($provider, false)?),
                ProducerAuthorityDisposition::EligibleUnique
            );
            Ok(())
        }
    };
}
current_positive!(current_mux_child, "mux");
current_positive!(current_openclaw_legacy_child, "openclaw");
current_positive!(current_crush_child, "crush");
current_positive!(current_opencode_task_child, "opencode");
current_positive!(current_zed_child, "zed");

#[test]
fn current_provider_contracts_and_released_support_keep_exact_boundaries() -> TestResult {
    for provider in PROVIDERS {
        for released in [true, false] {
            let record = provider_record(provider, released)?;
            let expected = if provider == "gemini" && !released {
                ProducerAuthorityDisposition::AbstainUnknown
            } else {
                ProducerAuthorityDisposition::EligibleUnique
            };
            assert_eq!(
                producer_authority_disposition(&record),
                expected,
                "{provider} released={released}"
            );
            for missing in ["scope", "parent", "relationship"] {
                let mut miss = record.clone();
                match missing {
                    "scope" => miss.agent_scope = Some(AgentScope::Primary),
                    "parent" => miss.parent_session_id = None,
                    "relationship" => miss.session_relationship = None,
                    _ => unreachable!(),
                }
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} {missing}"
                );
            }
            for revision in [
                format!("{}-unknown", record.parser_revision),
                "unknown".to_owned(),
            ] {
                let mut miss = record.clone();
                miss.parser_revision = revision;
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} revision"
                );
            }
            for field in ["provider", "format", "schema", "identity"] {
                let mut miss = record.clone();
                let source = &record.source;
                miss.source = SourceKey::derive(
                    if field == "provider" {
                        "kilo"
                    } else {
                        source.provider()
                    },
                    if field == "format" {
                        "unknown"
                    } else {
                        source.source_format()
                    },
                    if field == "schema" {
                        "unknown"
                    } else {
                        source.schema_variant()
                    },
                    if field == "identity" {
                        99
                    } else {
                        source.provider_identity_version()
                    },
                    SourceAnchor::provider_native("test", TypedKey::utf8("source")?)?,
                )?;
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} {field}"
                );
            }
            let mut copied = record.clone();
            // Copies win even over an unknown tuple or unresolved parent claim.
            copied.event_copy = Some(ProviderNativeEventCopy {
                ancestor_session_id: provider_record(provider, true)?.parent_session_id.unwrap(),
                ancestor_event_id: crate::protocol::derive_event_id(
                    crate::protocol::EventIdentityInput {
                        source: &record.source,
                        session_id: record.session_id,
                        logical_item_kind: "event",
                        native_item_key: &crate::protocol::NativeItemKey::native_id(
                            "event",
                            TypedKey::utf8("ancestor-event")?,
                        )?,
                        subrecord_selector: None,
                    },
                )?,
                proof: ProviderNativeCopyProof::NativeEventIdentity,
            });
            copied.validate_contract()?;
            assert_eq!(
                producer_authority_disposition(&copied),
                ProducerAuthorityDisposition::IneligibleCopied,
                "{provider}"
            );
            if !released {
                for field in ["session", "empty-session", "event", "null-event"] {
                    let mut miss = record.clone();
                    match field {
                        "session" => miss.provider_session_id = None,
                        "empty-session" => miss.provider_session_id = Some(String::new()),
                        "event" => miss.native_event_id = None,
                        "null-event" => miss.native_event_id = Some(TypedKey::Null),
                        _ => unreachable!(),
                    }
                    assert_eq!(
                        producer_authority_disposition(&miss),
                        ProducerAuthorityDisposition::AbstainUnknown,
                        "{provider} {field}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn only_current_mux_openclaw_allow_missing_root_and_no_provider_invents_relationship() -> TestResult
{
    for provider in PROVIDERS {
        let mut current = provider_record(provider, false)?;
        current.root_session_id = provider_record(provider, true)?.parent_session_id;
        assert_eq!(
            producer_authority_disposition(&current).permits_positive_authority(),
            provider != "gemini",
            "{provider}"
        );
        for relation in [
            None,
            Some(ProviderNativeSessionRelationship::Forked),
            Some(ProviderNativeSessionRelationship::ResumedFrom),
            Some(ProviderNativeSessionRelationship::WorkflowChild),
        ] {
            current.session_relationship = relation;
            assert_eq!(
                producer_authority_disposition(&current),
                ProducerAuthorityDisposition::AbstainUnknown,
                "{provider}"
            );
        }
    }
    for provider in ["mux", "openclaw"] {
        let mut old = provider_record(provider, true)?;
        old.root_session_id = None;
        assert_eq!(
            producer_authority_disposition(&old),
            ProducerAuthorityDisposition::AbstainUnknown
        );
    }
    // Gemini v2 cannot resolve a recording from a directory parent hint. Even a
    // fabricated edge must not turn this unreviewed identity-v2 tuple positive.
    let mut gemini = provider_record("gemini", false)?;
    gemini.parent_session_id = provider_record("gemini", true)?.parent_session_id;
    gemini.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    assert_eq!(
        producer_authority_disposition(&gemini),
        ProducerAuthorityDisposition::AbstainUnknown
    );
    Ok(())
}
