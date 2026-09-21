//! Real preparation/publication/blame over synthetic Core inputs.
//! These witnesses carry an actual isolated Git commit; they are not native captures.
use super::native_shell::{git, materialize_and_query};
use super::*;
use crate::core_materialization::provider_contract_test_support::{PROVIDERS, provider_record};
use crate::protocol::{LiteralFactKind, ProviderDeclaredFact};
use crate::query::AttributionOutcome;

#[test]
fn provider_admission_controls_real_blame_without_losing_neutral_file_evidence() -> TestResult {
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("repository");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-q"])?;
    git(&repo, &["config", "user.name", "fixture"])?;
    git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/shell-evidence.git",
        ],
    )?;
    std::fs::write(repo.join("witness.txt"), "provider witness\n")?;
    git(&repo, &["add", "witness.txt"])?;
    git(&repo, &["commit", "-qm", "fixture"])?;
    let oid = git(&repo, &["rev-parse", "HEAD"])?;
    let command = "git commit -m fixture && git rev-parse HEAD";
    for provider in PROVIDERS {
        for released in [true, false] {
            for mode in [
                "native-child",
                "copied",
                "missing-parent",
                "missing-event",
                "null-event",
                "unsupported",
            ] {
                let mut record = provider_record(provider, released)?;
                record.event_type = "tool_output".to_owned();
                record.occurred_at_unix_ms = Some(1_700_000_000_000);
                let mut activity = super::native_shell::record(&repo, command, &oid)?
                    .content
                    .activity
                    .unwrap();
                activity.invocation.as_mut().unwrap().tool = match provider {
                    "gemini" => "run_shell_command",
                    "openclaw" => "exec",
                    "zed" => "terminal",
                    _ => "bash",
                }
                .to_owned();
                // Non-Codex providers emit a literal Command fact; do not depend
                // on the exact Codex-only JSON-string argument decoder.
                activity.facts.push(ProviderDeclaredFact {
                    kind: LiteralFactKind::Command,
                    value: command.to_owned(),
                });
                record.content.activity = Some(activity);
                match mode {
                    "copied" => {
                        record.event_copy = Some(crate::protocol::ProviderNativeEventCopy {
                            ancestor_session_id: provider_record(provider, true)?
                                .parent_session_id
                                .unwrap(),
                            ancestor_event_id: stable_entity(
                                &record.source,
                                StableEntityKind::Event,
                                0x42,
                            )?,
                            proof: crate::protocol::ProviderNativeCopyProof::NativeEventIdentity,
                        })
                    }
                    "missing-parent" => record.parent_session_id = None,
                    "missing-event" => record.native_event_id = None,
                    "null-event" => record.native_event_id = Some(TypedKey::Null),
                    "unsupported" => record.parser_revision.push_str("-unknown"),
                    _ => {}
                }
                let eligible = mode == "native-child" && (released || provider != "gemini")
                    // Released contracts did not require the optional native ID mirror.
                    || matches!(mode, "missing-event" | "null-event") && released;
                record.validate_contract()?;
                let original = record.encode_stored()?;
                let outcomes = materialize_and_query(
                    &temp.path().join(format!("{provider}-{released}-{mode}")),
                    record.clone(),
                    &oid,
                )?;
                assert_eq!(
                    outcomes,
                    if eligible {
                        vec![AttributionOutcome::Possible]
                    } else {
                        vec![]
                    },
                    "{provider} released={released} {mode}"
                );
                assert_eq!(original, record.encode_stored()?);
            }
        }
    }
    Ok(())
}
