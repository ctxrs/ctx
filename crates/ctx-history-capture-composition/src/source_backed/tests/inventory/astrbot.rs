use super::*;
use crate::test_support_paths::complete_lexical_events;

#[test]
fn released_astrbot_root_scans_only_its_inventory_and_cannot_absorb_named_peer() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let create_database = |path: &Path, session: &str, marker: &str| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "pragma user_version = 4;
                 create table conversations (
                     id integer primary key,
                     inner_conversation_id text,
                     conversation_id text,
                     platform_id text,
                     user_id text,
                     content text not null,
                     title text,
                     persona_id text,
                     token_usage text,
                     created_at integer,
                     updated_at integer
                 );
                 create table platform_message_history (
                     id integer primary key,
                     platform_id text,
                     user_id text,
                     sender_id text,
                     sender_name text,
                     content text,
                     llm_checkpoint_id text,
                     created_at integer
                 );",
            )
            .unwrap();
        connection
            .execute(
                "insert into conversations (
                     id, inner_conversation_id, conversation_id, platform_id, user_id,
                     content, title, persona_id, token_usage, created_at, updated_at
                 ) values (1, ?1, ?2, 'webchat', 'user', ?3, 'title', 'persona',
                           '{\"prompt\":1,\"completion\":2}', 1780000000001, 1780000000001)",
                params![
                    session,
                    format!("conversation-{session}"),
                    serde_json::json!([{
                        "id": format!("message-{session}"),
                        "role": "user",
                        "content": marker,
                    }])
                    .to_string(),
                ],
            )
            .unwrap();
    };
    let first = home.join(".astrbot/data/data_v4.db");
    let second = temp.path().join("astrbot-second/data_v4.db");
    create_database(&first, "first-session", "astrbotrootalpha");
    create_database(&second, "second-session", "astrbotrootbeta");
    let roots = [("first", first), ("second", second)]
        .map(|(id, path)| ProviderRootDefinition {
            id: id.to_owned(),
            provider: CaptureProvider::AstrBot,
            path,
            group: None,
            kind: None,
        })
        .to_vec();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(roots);
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        &context,
        CaptureProvider::AstrBot,
    );
    assert_eq!(report.sources.len(), 2);
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        report.sources,
        report.issues,
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    assert_eq!(build.executable_route_count(), 2);
    let applied = &build.registry.applied_provider_roots().unwrap().2;
    assert_eq!(
        applied[0].source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(
        applied[1].source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );

    refresh_source_backed_generation(
        temp.path().join("index"),
        &build.registry,
        WriterOptions::default(),
    )
    .unwrap();
    let published = VerifiedIndex::open(temp.path().join("index")).unwrap();
    for (root, own, peer) in [
        ("first", "astrbotrootalpha", "astrbotrootbeta"),
        ("second", "astrbotrootbeta", "astrbotrootalpha"),
    ] {
        let allowed_source_keys = published
            .manifest()
            .provider_root_source_tokens(&[root.to_owned()], &[])
            .unwrap();
        let filters = ctx_history_index::EventSearchFilters {
            allowed_source_keys: Some(allowed_source_keys),
            ..ctx_history_index::EventSearchFilters::default()
        };
        assert_eq!(
            complete_lexical_events(&published, own, filters.clone(), 10).len(),
            1
        );
        assert!(complete_lexical_events(&published, peer, filters, 10).is_empty());
    }
}
