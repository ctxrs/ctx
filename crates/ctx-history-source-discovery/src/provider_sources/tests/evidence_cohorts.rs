use ctx_history_core::CaptureProvider;

use crate::provider_source_specs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceCohort {
    CodexStream,
    ContentBlockJsonl,
    SharedNativeJsonl,
    OpenCodeSqliteParts,
    DedicatedSqliteMessages,
    DirectorySidecarJson,
    FileEventProject,
    TaskDirectoryJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureOutcomeEvidence {
    ExplicitSuccessAndFailure,
    ExplicitOnlyWhenNativeFieldPresent,
    NoExplicitSuccess,
    NoResultEvent,
}

const REPRESENTATIVE_OUTCOME_AUDIT: &[(CaptureProvider, EvidenceCohort, FixtureOutcomeEvidence)] =
    &[
        (
            CaptureProvider::Codex,
            EvidenceCohort::CodexStream,
            FixtureOutcomeEvidence::ExplicitSuccessAndFailure,
        ),
        (
            CaptureProvider::Claude,
            EvidenceCohort::ContentBlockJsonl,
            FixtureOutcomeEvidence::ExplicitOnlyWhenNativeFieldPresent,
        ),
        (
            CaptureProvider::Cursor,
            EvidenceCohort::SharedNativeJsonl,
            FixtureOutcomeEvidence::NoExplicitSuccess,
        ),
        (
            CaptureProvider::OpenCode,
            EvidenceCohort::OpenCodeSqliteParts,
            FixtureOutcomeEvidence::ExplicitSuccessAndFailure,
        ),
        (
            CaptureProvider::KiroCli,
            EvidenceCohort::DedicatedSqliteMessages,
            FixtureOutcomeEvidence::NoResultEvent,
        ),
        (
            CaptureProvider::Auggie,
            EvidenceCohort::DirectorySidecarJson,
            FixtureOutcomeEvidence::NoResultEvent,
        ),
        (
            CaptureProvider::OpenHands,
            EvidenceCohort::FileEventProject,
            FixtureOutcomeEvidence::ExplicitOnlyWhenNativeFieldPresent,
        ),
        (
            CaptureProvider::Cline,
            EvidenceCohort::TaskDirectoryJson,
            FixtureOutcomeEvidence::ExplicitSuccessAndFailure,
        ),
    ];

const PROVIDER_EVIDENCE_COHORTS: &[(CaptureProvider, EvidenceCohort)] = &[
    (CaptureProvider::Codex, EvidenceCohort::CodexStream),
    (
        CaptureProvider::GrokBuild,
        EvidenceCohort::SharedNativeJsonl,
    ),
    (
        CaptureProvider::DeepSeekHarness,
        EvidenceCohort::SharedNativeJsonl,
    ),
    (CaptureProvider::Pi, EvidenceCohort::ContentBlockJsonl),
    (CaptureProvider::Claude, EvidenceCohort::ContentBlockJsonl),
    (
        CaptureProvider::OpenCode,
        EvidenceCohort::OpenCodeSqliteParts,
    ),
    (CaptureProvider::Kilo, EvidenceCohort::OpenCodeSqliteParts),
    (
        CaptureProvider::MiMoCode,
        EvidenceCohort::OpenCodeSqliteParts,
    ),
    (
        CaptureProvider::KiroCli,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::Crush,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::Goose,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::Antigravity,
        EvidenceCohort::SharedNativeJsonl,
    ),
    (CaptureProvider::Gemini, EvidenceCohort::SharedNativeJsonl),
    (CaptureProvider::Tabnine, EvidenceCohort::SharedNativeJsonl),
    (CaptureProvider::Cursor, EvidenceCohort::SharedNativeJsonl),
    (
        CaptureProvider::Zed,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::CopilotCli,
        EvidenceCohort::SharedNativeJsonl,
    ),
    (
        CaptureProvider::FactoryAiDroid,
        EvidenceCohort::SharedNativeJsonl,
    ),
    (CaptureProvider::QwenCode, EvidenceCohort::SharedNativeJsonl),
    (
        CaptureProvider::KimiCodeCli,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (
        CaptureProvider::Auggie,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (CaptureProvider::Junie, EvidenceCohort::DirectorySidecarJson),
    (
        CaptureProvider::Firebender,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::Xopc,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::ForgeCode,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::DeepAgents,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::MistralVibe,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (CaptureProvider::Mux, EvidenceCohort::DirectorySidecarJson),
    (
        CaptureProvider::RovoDev,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (
        CaptureProvider::OpenClaw,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (
        CaptureProvider::Hermes,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (CaptureProvider::NanoClaw, EvidenceCohort::FileEventProject),
    (
        CaptureProvider::AstrBot,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::Shelley,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::Continue,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (CaptureProvider::OpenHands, EvidenceCohort::FileEventProject),
    (CaptureProvider::Cline, EvidenceCohort::TaskDirectoryJson),
    (CaptureProvider::RooCode, EvidenceCohort::TaskDirectoryJson),
    (
        CaptureProvider::Lingma,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (CaptureProvider::Qoder, EvidenceCohort::SharedNativeJsonl),
    (
        CaptureProvider::Warp,
        EvidenceCohort::DedicatedSqliteMessages,
    ),
    (
        CaptureProvider::CodeBuddy,
        EvidenceCohort::DirectorySidecarJson,
    ),
    (CaptureProvider::Fx, EvidenceCohort::DirectorySidecarJson),
];

#[test]
fn every_registered_provider_is_routed_to_one_evidence_cohort() {
    let registered = provider_source_specs();
    assert_eq!(
        registered.len(),
        43,
        "update the evidence matrix deliberately"
    );
    assert_eq!(PROVIDER_EVIDENCE_COHORTS.len(), registered.len());

    for spec in registered {
        let routes = PROVIDER_EVIDENCE_COHORTS
            .iter()
            .filter(|(provider, _)| *provider == spec.provider)
            .count();
        assert_eq!(
            routes,
            1,
            "{} must belong to exactly one evidence cohort",
            spec.provider.as_str()
        );
    }

    for (provider, _) in PROVIDER_EVIDENCE_COHORTS {
        assert!(
            registered.iter().any(|spec| spec.provider == *provider),
            "matrix contains unregistered provider {}",
            provider.as_str()
        );
    }
}

#[test]
fn every_evidence_cohort_has_a_deliberate_result_outcome_representative() {
    assert_eq!(REPRESENTATIVE_OUTCOME_AUDIT.len(), 8);
    for cohort in [
        EvidenceCohort::CodexStream,
        EvidenceCohort::ContentBlockJsonl,
        EvidenceCohort::SharedNativeJsonl,
        EvidenceCohort::OpenCodeSqliteParts,
        EvidenceCohort::DedicatedSqliteMessages,
        EvidenceCohort::DirectorySidecarJson,
        EvidenceCohort::FileEventProject,
        EvidenceCohort::TaskDirectoryJson,
    ] {
        assert_eq!(
            REPRESENTATIVE_OUTCOME_AUDIT
                .iter()
                .filter(|(_, audited_cohort, _)| *audited_cohort == cohort)
                .count(),
            1,
            "{cohort:?} must have exactly one audited representative"
        );
    }
}
