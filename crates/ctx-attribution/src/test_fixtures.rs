//! Authored Core/Git inputs for real-engine tests. No provider captures or
//! expected answers are read from disk; callers own and publish their Core.
use anyhow::{Result, anyhow, ensure};
use ctx_history_core::*;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct AuthoredCommitFixture {
    pub repository: PathBuf,
    pub oid: String,
    pub record: CoreRecord,
    pub certificate: CertifiedSource,
}

/// Creates one isolated Git commit and a typed, admitted Core event describing
/// it. `repository` must not exist. No ctx process, daemon, import, or index is
/// started. Publish `record` with `certificate`, pin that Core generation, then
/// call ordinary `catch_up` and `query`. The expected citation is this record's
/// source/session/event/sequence in the caller's committed Core generation.
pub fn authored_commit_fixture(repository: &Path) -> Result<AuthoredCommitFixture> {
    std::fs::create_dir(repository)?;
    let repository = repository.canonicalize()?;
    std::fs::create_dir(repository.join("isolated-tmp"))?;
    let git = crate::GitExecutable::discover()?;
    let run = |args: &[&str]| -> Result<String> {
        let output = Command::new(git.path())
            .arg("-C")
            .arg(&repository)
            .args(["-c", "commit.gpgsign=false", "-c"])
            .arg(format!(
                "core.hooksPath={}",
                repository.join("absent-hooks").display()
            ))
            .args(args)
            .env_clear()
            .env(
                "SystemRoot",
                std::env::var_os("SystemRoot").unwrap_or_default(),
            )
            .env("TMPDIR", repository.join("isolated-tmp"))
            .env("TMP", repository.join("isolated-tmp"))
            .env("TEMP", repository.join("isolated-tmp"))
            .env("HOME", repository.join("isolated-home"))
            .env("XDG_CONFIG_HOME", repository.join("isolated-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repository.join("absent-global-config"))
            .env("GIT_AUTHOR_NAME", "Authored fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Authored fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()?;
        ensure!(
            output.status.success(),
            "fixture Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    run(&["init", "-q"])?;
    run(&[
        "remote",
        "add",
        "origin",
        "https://github.com/example/attribution-fixture.git",
    ])?;
    std::fs::write(
        repository.join("witness.txt"),
        "authored attribution witness\n",
    )?;
    run(&["add", "witness.txt"])?;
    run(&["commit", "-qm", "authored fixture"])?;
    let oid = run(&["rev-parse", "HEAD"])?;
    let mut record =
        crate::core_materialization::provider_contract_test_support::provider_record("zed", true)
            .map_err(|error| anyhow!(error.to_string()))?;
    record.event_type = "tool_output".to_owned();
    record.occurred_at_unix_ms = Some(1_700_000_000_000);
    let command = "git commit -m 'authored fixture' && git rev-parse HEAD";
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("authored-commit-call")?),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: "terminal".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::Value::String(
                    serde_json::json!({"cmd":command,"workdir":repository}).to_string(),
                ),
            },
            started_at_unix_ms: None,
        }),
        result: Some(ActivityResult {
            status: Some("success".to_owned()),
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Present { value: oid.clone() },
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: vec![
            ProviderDeclaredFact {
                kind: LiteralFactKind::Command,
                value: command.to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::ToolWorkdir,
                value: repository.to_string_lossy().into_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "witness.txt".to_owned(),
            },
        ],
    });
    record.validate_contract()?;
    let observation =
        SourceObservation::new(record.source.clone(), "authored-core-fixture-v1", vec![1])?;
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "authored-core-fixture-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..Default::default()
        },
    )?;
    Ok(AuthoredCommitFixture {
        repository,
        oid,
        record,
        certificate,
    })
}
