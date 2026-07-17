#[test]
fn pi_append_unit_uses_per_file_publication_owner() {
    let source_root = "/fixture/pi/sessions";
    let source_path = "/fixture/pi/sessions/session.jsonl";
    let source = explicit_path_source(CaptureProvider::Pi, source_root.into());
    let work = SourceImportFileWork {
        file: SourceImportFile {
            provider: CaptureProvider::Pi,
            source_format: "pi_session_jsonl".to_owned(),
            source_root: source_root.to_owned(),
            source_path: source_path.to_owned(),
            file_size_bytes: 10,
            file_modified_at_ms: 20,
            import_revision: 1,
            observed_at_ms: 20,
            metadata: json!({}),
        },
        reason: ImportPendingReason::FreshNew,
        estimated_bytes: 10,
        last_attempt_at_ms: None,
        has_active_publication: false,
    };
    let unit = AppendInventoryUnit::SourceFile {
        source: &source,
        work: &work,
        inventory_generation: 1,
    };

    assert_eq!(
        unit.publication_material_owner("pi_session_jsonl"),
        ctx_history_store::ProviderFilePublicationMaterialOwner::source_file(
            CaptureProvider::Pi,
            "pi_session_jsonl",
            source_path,
        )
    );
}

#[test]
fn codex_append_unit_preserves_catalog_root_publication_owner() {
    let source_root = "/fixture/codex/sessions";
    let source_path = "/fixture/codex/sessions/session.jsonl";
    let source = explicit_path_source(CaptureProvider::Codex, source_root.into());
    let work = CatalogImportWork {
        session: CatalogSession {
            provider: CaptureProvider::Codex,
            source_format: "codex_session_jsonl_tree".to_owned(),
            source_root: source_root.to_owned(),
            source_path: source_path.to_owned(),
            external_session_id: Some("session".to_owned()),
            parent_external_session_id: None,
            agent_type: ctx_history_core::AgentType::Primary,
            role_hint: None,
            external_agent_id: None,
            cwd: None,
            session_started_at_ms: Some(1),
            file_size_bytes: 10,
            file_modified_at_ms: 20,
            import_revision: 1,
            cataloged_at_ms: 20,
            metadata: json!({"file_observation_token_v1": "token"}),
        },
        reason: ImportPendingReason::FreshNew,
        estimated_bytes: 10,
        last_attempt_at_ms: None,
        has_active_publication: false,
    };
    let unit = AppendInventoryUnit::Catalog {
        source: &source,
        work: &work,
        inventory_generation: 1,
    };

    assert_eq!(
        unit.publication_material_owner("codex_session_jsonl"),
        ctx_history_store::ProviderFilePublicationMaterialOwner::catalog_root(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            source_root,
        )
    );
}
