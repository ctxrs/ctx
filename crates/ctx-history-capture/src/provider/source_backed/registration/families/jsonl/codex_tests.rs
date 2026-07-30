use std::{
    fs::{self, OpenOptions},
    io::Write,
};

use super::*;
use crate::ProviderCatalogSupport;

#[test]
fn active_source_family_contract_prompt_history_terminal_inventory_accepts_deferred_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let history = temp.path().join("history.jsonl");
    let first = serde_json::json!({
        "session_id": "terminal-session",
        "ts": 1_785_139_200,
        "text": "before terminal callback",
    });
    fs::write(&history, format!("{first}\n")).unwrap();
    let input = CodexPromptHistorySourceBackedInputV0::explicit(
        &history,
        CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0,
    );
    let retained = observe_codex_prompt_history_source_backed_explicit_v0(&input).unwrap();
    let scan =
        scan_codex_prompt_history_source_backed_v0(retained.clone(), None, |_| Ok(())).unwrap();
    let route = ProviderSource {
        provider: CaptureProvider::Codex,
        path: history.clone(),
        exists: true,
        source_format: "codex_history_jsonl",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let inventory =
        certify_source_inventory(&route, std::slice::from_ref(&scan.certificate)).unwrap();
    let state = Mutex::new(Some(CodexPromptTerminalEvidence {
        certificate: scan.certificate.clone(),
        inventory: inventory.clone(),
    }));
    assert!(bind_codex_prompt_target(
        &state,
        SourceBackedRevalidationTarget::Source(&scan.certificate),
    ));

    let second = serde_json::json!({
        "session_id": "terminal-session",
        "ts": 1_785_139_201,
        "text": "mutated between callbacks",
    });
    writeln!(
        OpenOptions::new().append(true).open(&history).unwrap(),
        "{second}"
    )
    .unwrap();
    let capture = move |expected: &CertifiedSource| {
        revalidate_codex_prompt_history_source_backed_v0(&retained, expected)
            .map_err(route_error)?;
        certify_source_inventory(&route, std::slice::from_ref(expected))
    };
    assert!(revalidate_codex_prompt_inventory(
        &state, &capture, &inventory,
    ));

    let mut rewritten = fs::read(&history).unwrap();
    let offset = rewritten
        .windows(b"before terminal callback".len())
        .position(|window| window == b"before terminal callback")
        .unwrap();
    rewritten[offset] = b'B';
    fs::write(&history, rewritten).unwrap();
    assert!(!revalidate_codex_prompt_inventory(
        &state, &capture, &inventory,
    ));
}
