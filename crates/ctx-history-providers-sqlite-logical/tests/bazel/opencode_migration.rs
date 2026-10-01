use super::*;

#[test]
fn migrated_assistant_retains_text_time_tool_title_output_metadata_and_tokens() {
    // Authored from the released migration: text time, completed tool title and
    // reconstructible token total are dropped. This is not a native capture.
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let database = temp.path().join("source/opencode.db");
    let writer = create_opencode_wal_database(&database, "toolquestion");
    let index_root = temp.path().join("index");
    writer.execute_batch(
        "insert into message values ('message-2','session-1',10,90,'{\"role\":\"assistant\"}');
         insert into part values ('part-2','message-2','session-1',20,30,'{\"type\":\"text\",\"text\":\"toolanswer\",\"metadata\":{\"note\":\"text state\"},\"time\":{\"start\":20,\"end\":30}}');
         create table session_v2 (id text primary key, parent_id text, directory text, branch text, agent text, time_created integer, time_updated integer);
         insert into session_v2 select * from session;
         create table session_message (id text primary key, session_id text, type text, seq integer, time_created integer, time_updated integer, data text);
         create unique index session_message_session_seq_idx on session_message(session_id,seq);
         insert into session_message values ('message-1','session-1','user',0,1,1,'{\"text\":\"toolquestion\",\"time\":{\"created\":1}}');",
    ).unwrap();
    writer.execute("insert into part values ('part-3','message-2','session-1',40,50,?1)", [json!({
        "type":"tool","callID":"read-call","tool":"read","metadata":{"origin":"native"},
        "state":{"status":"completed","input":{"filePath":"sample.rs"},"output":"sample contents",
                 "title":"Read sample.rs","metadata":{"output":"preserved output metadata","nested":{"note":"preserved"}},
                 "time":{"start":40,"end":50}}
    }).to_string()]).unwrap();
    let tokens = json!({"input":10,"output":5,"reasoning":3,"cache":{"read":2,"write":1}});
    let mut legacy_tokens = tokens.clone();
    legacy_tokens["total"] = json!(21);
    writer
        .execute(
            "insert into part values ('part-4','message-2','session-1',60,60,?1)",
            [json!({
                "type":"step-finish","reason":"stop","cost":0.25,"tokens":legacy_tokens
            })
            .to_string()],
        )
        .unwrap();
    writer.execute("update message set data=?1 where id='message-2'", [json!({
        "role":"assistant","parentID":"message-1","agent":"build","modelID":"example", "providerID":"example",
        "time":{"created":10,"completed":90},"cost":0.25,"finish":"stop","tokens":legacy_tokens
    }).to_string()]).unwrap();
    let current = json!({"time":{"created":10,"completed":90},"cost":0.25,"finish":"stop","tokens":tokens,"content":[
        {"type":"text","text":"toolanswer","state":{"note":"text state"}},
        {"type":"tool","id":"read-call","name":"read","providerState":{"origin":"native"},
         "state":{"status":"completed","input":{"filePath":"sample.rs"},
                  "content":[{"type":"text","text":"sample contents"}],
                  "metadata":{"output":"preserved output metadata","nested":{"note":"preserved"}}},"time":{"created":40,"completed":50}}
    ]});
    writer
        .execute(
            "insert into session_message values ('message-2','session-1','assistant',1,10,90,?1)",
            [current.to_string()],
        )
        .unwrap();
    let refresh = || {
        let before = ["opencode.db", "opencode.db-wal"]
            .map(|name| fs::read(database.with_file_name(name)).unwrap());
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route_with_data_root(
            &mut registry,
            provider_source_for_path(CaptureProvider::OpenCode, database.clone()),
            SourceBackedRouteSelection::Automatic,
            &data_root,
        )
        .unwrap();
        let report = SourceBackedRefreshExecutor::new(registry, WriterOptions::default())
            .refresh_scope_with_detailed_progress(
                &index_root,
                SourceBackedRefreshScope::All,
                |_| Ok(()),
            )
            .unwrap();
        let after = ["opencode.db", "opencode.db-wal"]
            .map(|name| fs::read(database.with_file_name(name)).unwrap());
        assert_eq!(before, after, "enrichment must not change provider DB/WAL");
        report
    };
    let published = || {
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        let event = only_matching_event(&index, "toolanswer");
        index
            .core_record_by_id(event.event_id.as_uuid())
            .unwrap()
            .unwrap()
    };
    let cold = refresh();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.sources[0].counts().retained_records, 2);
    let initial = published();
    assert_eq!(initial.occurred_at_unix_ms, Some(10));
    let structured = initial.content.structured_content.as_ref().unwrap();
    assert_eq!(structured["time"], json!({"created":10,"completed":90}));
    assert_eq!(
        structured["content"][0]["time"],
        json!({"created":20,"completed":30})
    );
    assert_eq!(
        structured["content"][0]["state"],
        json!({"note":"text state"})
    );
    assert_eq!(
        structured["content"][1]["time"],
        json!({"created":40,"completed":50})
    );
    assert_eq!(structured["tokens"], tokens);
    assert_eq!(structured["cost"], json!(0.25));
    assert_eq!(structured["content"][1]["state"]["title"], "Read sample.rs");
    assert_eq!(
        structured["content"][1]["state"]["metadata"],
        current["content"][1]["state"]["metadata"]
    );
    assert_eq!(
        structured["content"][1]["providerState"],
        current["content"][1]["providerState"]
    );
    let replay = refresh();
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    assert!(!replay.successful_route_outcomes[0].changed);
    assert_eq!(published(), initial);

    // Only legacy evidence changes: the v2 row and its timestamps are identical.
    writer.execute_batch("update part set data=json_set(data,'$.state.title','Read updated sample') where id='part-3'").unwrap();
    let updated = refresh();
    assert!(updated.failed_routes.is_empty());
    assert_ne!(
        updated.sources[0].content_digest(),
        cold.sources[0].content_digest()
    );
    let revised = published();
    assert_eq!(revised.event_id, initial.event_id);
    assert_eq!(
        revised.content.structured_content.as_ref().unwrap()["content"][1]["state"]["title"],
        "Read updated sample"
    );
    assert_eq!(refresh().commit.generation_id, updated.commit.generation_id);

    writer.execute_batch("update part set data=json_set(data,'$.time.start',21,'$.time.end',31) where id='part-2'").unwrap();
    let retimed = refresh();
    assert!(retimed.failed_routes.is_empty());
    assert_ne!(
        retimed.sources[0].content_digest(),
        updated.sources[0].content_digest()
    );
    let revised = published();
    assert_eq!(revised.event_id, initial.event_id);
    assert_eq!(revised.session_id, initial.session_id);
    assert_eq!(revised.occurred_at_unix_ms, Some(10));
    assert_eq!(
        revised.content.structured_content.as_ref().unwrap()["content"][0]["time"],
        json!({"created":21,"completed":31})
    );
    assert_eq!(refresh().commit.generation_id, retimed.commit.generation_id);

    for mutation in [
        "update session_message set data=json_set(data,'$.content[1].state.title','conflicting title') where id='message-2'",
        "update session_message set data=json_set(data,'$.content[1].state.input.filePath','different.rs') where id='message-2'",
        "update session_message set data=json_remove(data,'$.content[1].state.metadata.nested.note') where id='message-2'",
        "update session_message set data=json_set(data,'$.content[1].state.metadata.output','different') where id='message-2'",
        "update session_message set data=json_remove(data,'$.content[1].state.metadata.output') where id='message-2'",
        "update session_message set data=json_set(data,'$.content[0].time',json('{\"created\":22,\"completed\":31}')) where id='message-2'",
        "update session_message set data=json_set(data,'$.content[1].time.completed',51) where id='message-2'",
        "update session_message set data=replace(data,'\"output\":\"preserved output metadata\"','\"output\":\"preserved output metadata\",\"output\":\"preserved output metadata\"') where id='message-2'",
    ] {
        writer.execute_batch(mutation).unwrap();
        let refused = refresh();
        assert!(refused.successful_route_outcomes.is_empty(), "{mutation}");
        assert_eq!(refused.failed_routes.len(), 1, "{mutation}");
        assert!(refused.failed_routes[0].carried_forward, "{mutation}");
        assert_eq!(refused.commit.generation_id, retimed.commit.generation_id, "{mutation}");
        assert_eq!(published(), revised, "{mutation}");
        writer.execute("update session_message set data=?1 where id='message-2'", [current.to_string()]).unwrap();
    }
    // Already-present equal native fields also pass, without overwriting them.
    writer
        .execute_batch(
            "update session_message set data=json_set(data,
        '$.content[1].state.title','Read updated sample',
        '$.content[0].time',json('{\"created\":21,\"completed\":31}')) where id='message-2'",
        )
        .unwrap();
    let recovered = refresh();
    assert!(recovered.failed_routes.is_empty());
    assert_eq!(published(), revised);
    assert_eq!(
        refresh().commit.generation_id,
        recovered.commit.generation_id
    );

    // Two individually bounded legacy titles cannot be silently discarded when
    // their combined enriched Core content exceeds its aggregate limit.
    let title = "t".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES / 2 - 128);
    writer
        .execute(
            "update part set data=json_set(data,'$.state.title',?1) where id='part-3'",
            [title],
        )
        .unwrap();
    writer.execute_batch("insert into part select 'part-5',message_id,session_id,time_created,time_updated,json_set(data,'$.callID','second-read') from part where id='part-3'").unwrap();
    let mut oversized = current.clone();
    let mut second = current["content"][1].clone();
    second["id"] = json!("second-read");
    oversized["content"].as_array_mut().unwrap().push(second);
    writer
        .execute(
            "update session_message set data=?1 where id='message-2'",
            [oversized.to_string()],
        )
        .unwrap();
    let refused = refresh();
    assert_eq!(refused.failed_routes.len(), 1);
    assert!(refused.failed_routes[0].carried_forward);
    assert_eq!(refused.commit.generation_id, recovered.commit.generation_id);
    assert!(refused.source_failures.failures()[0]
        .detail
        .contains("structured"));
    assert_eq!(published(), revised);
}

#[test]
fn covered_reasoning_requires_structured_retention_without_any_restored_fields() {
    // Authored boundary controls for the native reasoning mapping. Each legacy
    // part fits Core on its own. Combining the two 5 MiB parts exceeds Core's
    // aggregate limit even though the migrated SQLite JSON still fits its cap.
    for (part_mib, legacy_assistant, admits_migration) in
        [(5, true, false), (1, true, true), (5, false, true)]
    {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let database = temp.path().join("source/opencode.db");
        let writer = create_opencode_wal_database(&database, "reasoningquestion");
        let index_root = temp.path().join("index");
        let texts = ["a", "b"].map(|letter| letter.repeat(part_mib * 1024 * 1024));
        let legacy = [
            json!({"type":"reasoning","text":texts[0],
                   "metadata":{"note":"first reasoning"},"time":{"start":20,"end":30}}),
            json!({"type":"reasoning","text":texts[1],
                   "metadata":{"note":"second reasoning"},"time":{"start":40,"end":50}}),
        ];
        if legacy_assistant {
            writer.execute_batch(
                "insert into message values ('message-2','session-1',10,90,'{\"role\":\"assistant\",\"time\":{\"created\":10,\"completed\":90}}');",
            ).unwrap();
            for (id, created, updated, part) in [
                ("part-2", 20, 30, &legacy[0]),
                ("part-3", 40, 50, &legacy[1]),
            ] {
                writer
                    .execute(
                        "insert into part values (?1,'message-2','session-1',?2,?3,?4)",
                        params![id, created, updated, part.to_string()],
                    )
                    .unwrap();
            }
        }
        let refresh = || {
            let before = ["opencode.db", "opencode.db-wal"]
                .map(|name| fs::read(database.with_file_name(name)).unwrap());
            let mut registry = SourceBackedProviderRegistry::new();
            register_landed_source_backed_route_with_data_root(
                &mut registry,
                provider_source_for_path(CaptureProvider::OpenCode, database.clone()),
                SourceBackedRouteSelection::Automatic,
                &data_root,
            )
            .unwrap();
            let report = SourceBackedRefreshExecutor::new(registry, WriterOptions::default())
                .refresh_scope_with_detailed_progress(
                    &index_root,
                    SourceBackedRefreshScope::All,
                    |_| Ok(()),
                )
                .unwrap();
            let after = ["opencode.db", "opencode.db-wal"]
                .map(|name| fs::read(database.with_file_name(name)).unwrap());
            assert!(before == after, "refresh must not change provider DB/WAL");
            report
        };
        let published = || {
            let index = VerifiedIndex::open_pinned(&index_root).unwrap();
            let user = only_matching_event(&index, "reasoningquestion");
            index
                .events_for_session(user.session_id.as_uuid())
                .unwrap()
                .into_iter()
                .map(|event| {
                    index
                        .core_record_by_id(event.event_id.as_uuid())
                        .unwrap()
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        let baseline = refresh();
        assert!(baseline.failed_routes.is_empty());
        let before = published();
        assert_eq!(before.len(), if legacy_assistant { 3 } else { 1 });
        if legacy_assistant {
            for (core, part) in before[1..].iter().zip(&legacy) {
                let structured = core.content.structured_content.as_ref().unwrap();
                assert_eq!(structured["metadata"], part["metadata"]);
                assert_eq!(structured["time"], part["time"]);
                assert!(structured == part, "legacy reasoning Core must be exact");
                assert_eq!(core.content.meaningful_text().len(), part_mib * 1024 * 1024);
            }
        }
        assert_eq!(
            refresh().commit.generation_id,
            baseline.commit.generation_id
        );

        writer.execute_batch(
            "create table session_v2 (id text primary key, parent_id text, directory text, branch text, agent text, time_created integer, time_updated integer);
             insert into session_v2 select * from session;
             create table session_message (id text primary key, session_id text, type text, seq integer, time_created integer, time_updated integer, data text);
             create unique index session_message_session_seq_idx on session_message(session_id,seq);
             insert into session_message values ('message-1','session-1','user',0,1,1,'{\"text\":\"reasoningquestion\",\"time\":{\"created\":1}}');",
        ).unwrap();
        // Reasoning already carries its exact state/time in v2: there is no
        // omitted title or text timestamp to restore in any of these cases.
        let current = json!({"time":{"created":10,"completed":90},"content":[
            {"type":"reasoning","text":texts[0],
             "state":{"note":"first reasoning"},"time":{"created":20,"completed":30}},
            {"type":"reasoning","text":texts[1],
             "state":{"note":"second reasoning"},"time":{"created":40,"completed":50}}
        ]});
        writer.execute(
            "insert into session_message values ('message-2','session-1','assistant',1,10,90,?1)",
            [current.to_string()],
        ).unwrap();
        let migrated = refresh();
        if !admits_migration {
            assert!(migrated.successful_route_outcomes.is_empty());
            assert_eq!(migrated.failed_routes.len(), 1);
            assert!(migrated.failed_routes[0].carried_forward);
            assert_eq!(migrated.commit.generation_id, baseline.commit.generation_id);
            assert!(migrated.source_failures.failures()[0]
                .detail
                .contains("covered legacy assistant exceeds Core structured-content limits"));
            assert!(
                published() == before,
                "refusal must retain every prior Core field"
            );
            continue;
        }
        assert!(migrated.failed_routes.is_empty());
        assert_eq!(migrated.sources[0].counts().retained_records, 2);
        let after = published();
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].occurred_at_unix_ms, Some(10));
        assert!(after[1].content.meaningful_text() == texts.join("\n"));
        if legacy_assistant {
            let structured = after[1].content.structured_content.as_ref().unwrap();
            assert!(
                structured == &current,
                "fitting migrated Core must be exact"
            );
            assert_eq!(structured["content"][0]["state"], legacy[0]["metadata"]);
            assert_eq!(structured["content"][1]["state"], legacy[1]["metadata"]);
            assert_eq!(
                structured["content"][0]["time"],
                json!({"created":20,"completed":30})
            );
            assert_eq!(
                structured["content"][1]["time"],
                json!({"created":40,"completed":50})
            );
        } else {
            // The user still proves overlap, but this large assistant has no
            // legacy parts. Its existing optional-structure behavior is intact.
            assert!(after[1].content.structured_content.is_none());
        }
        let replay = refresh();
        assert_eq!(replay.commit.generation_id, migrated.commit.generation_id);
        assert!(!replay.successful_route_outcomes[0].changed);
        assert!(
            published() == after,
            "no-op must preserve exact Core records"
        );
    }
}
