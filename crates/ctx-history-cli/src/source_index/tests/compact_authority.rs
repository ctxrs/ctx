use super::*;

#[test]
fn human_search_lengths_ids_against_colliding_retained_generation() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (_, active) = compact_collision_pair(temp.path(), "compact authority search needle");
    let event = active.event.event_id.as_uuid().simple().to_string();
    let session = active.event.session_id.as_uuid().simple().to_string();
    let (mut ui, stdout) = test_ui();
    let mut usage = CliUsage::excluded();
    let mut args = lexical_search_args();
    args.query = Some("compact authority search needle".to_owned());
    let mut observation = None;

    run_search(
        args,
        temp.path().to_path_buf(),
        history_snapshot(false, false),
        &mut usage,
        &mut ui,
        |value| observation = Some(value),
    )
    .unwrap();
    ui.flush().unwrap();

    assert_eq!(observation.unwrap().result_count, Some(1));
    let rendered = String::from_utf8(stdout.bytes()).unwrap();
    assert!(rendered.lines().any(|line| {
        line.trim_start()
            .strip_prefix("Session")
            .is_some_and(|value| value.trim() == &session[..9])
    }));
    assert!(rendered.lines().any(|line| {
        line.trim_start()
            .strip_prefix("Event")
            .is_some_and(|value| value.trim() == &event[..9])
    }));
    assert!(!rendered.contains(&active.event.event_id.as_uuid().to_string()));
    assert!(!rendered.contains(&active.event.session_id.as_uuid().to_string()));
}

#[test]
fn mcp_compact_search_lengths_ids_against_colliding_retained_generation() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (_, active) = compact_collision_pair(temp.path(), "mcp compact authority needle");
    let event = active.event.event_id.as_uuid();
    let session = active.event.session_id.as_uuid();
    let mut request = request(RefreshArg::Off);
    request.query = "mcp compact authority needle".to_owned();

    let (full, _, compact, observation) =
        mcp_search_with_compact(request, temp.path(), history_snapshot(false, false)).unwrap();

    assert_eq!(observation.result_count, Some(1));
    assert_eq!(full["results"][0]["ctx_event_id"], event.to_string());
    assert_eq!(full["results"][0]["ctx_session_id"], session.to_string());
    assert_eq!(
        compact["results"][0]["ctx_event_id"],
        event.simple().to_string()[..9]
    );
    assert_eq!(
        compact["results"][0]["ctx_session_id"],
        session.simple().to_string()[..9]
    );
    assert_eq!(
        full["results"][0]["citations"][0]["item_id"],
        event.to_string()
    );
    assert_eq!(
        compact["results"][0]["citations"][0]["item_id"],
        event.to_string()
    );
    assert_eq!(
        compact["results"][0]["citations"][0]["ctx_event_id"],
        event.simple().to_string()[..9]
    );
    assert_eq!(
        compact["results"][0]["citations"][0]["ctx_session_id"],
        session.simple().to_string()[..9]
    );
    let commands =
        serde_json::to_string(&compact["results"][0]["suggested_next_commands"]).unwrap();
    assert!(commands.contains(&event.simple().to_string()[..9]));
    assert!(!commands.contains(&event.to_string()));
    assert!(!commands.contains(&session.to_string()));
}

#[test]
fn locate_compact_output_and_selectors_use_retained_generation() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (retained, active) = compact_collision_pair(temp.path(), "locate compact authority");
    let active_event = active.event.event_id.as_uuid().simple().to_string();
    let active_session = active.event.session_id.as_uuid().simple().to_string();

    for format in [JsonOutputFormat::Text, JsonOutputFormat::Json] {
        for (target, retained_id, active_id) in [
            (
                crate::LocateTarget::Event(crate::LocateEventArgs {
                    id: active_event[..8].to_owned(),
                    format,
                }),
                retained.event.event_id.as_uuid(),
                active.event.event_id.as_uuid(),
            ),
            (
                crate::LocateTarget::Session(crate::LocateSessionArgs {
                    id: Some(active_session[..8].to_owned()),
                    provider: None,
                    provider_session: None,
                    provider_key: None,
                    source_id: None,
                    format,
                }),
                retained.event.session_id.as_uuid(),
                active.event.session_id.as_uuid(),
            ),
        ] {
            let (mut ui, _) = test_ui();
            let mut usage = CliUsage::excluded();
            let error = run_locate(
                crate::LocateArgs { target },
                temp.path().to_path_buf(),
                &mut usage,
                &mut ui,
            )
            .unwrap_err();
            let detail = error.to_string();
            assert!(detail.contains("ambiguous"), "{detail}");
            assert!(detail.contains(&retained_id.to_string()), "{detail}");
            assert!(detail.contains(&active_id.to_string()), "{detail}");
        }
    }

    for full_id in [false, true] {
        let event_id = if full_id {
            active.event.event_id.to_string()
        } else {
            active_event[..9].to_owned()
        };
        let session_id = if full_id {
            active.event.session_id.to_string()
        } else {
            active_session[..9].to_owned()
        };
        for target in [
            crate::LocateTarget::Event(crate::LocateEventArgs {
                id: event_id,
                format: JsonOutputFormat::Text,
            }),
            crate::LocateTarget::Session(crate::LocateSessionArgs {
                id: Some(session_id),
                provider: None,
                provider_session: None,
                provider_key: None,
                source_id: None,
                format: JsonOutputFormat::Text,
            }),
        ] {
            let is_event = matches!(target, crate::LocateTarget::Event(_));
            let (mut ui, stdout) = test_ui();
            run_locate(
                crate::LocateArgs { target },
                temp.path().to_path_buf(),
                &mut CliUsage::excluded(),
                &mut ui,
            )
            .unwrap();
            ui.flush().unwrap();
            let rendered = String::from_utf8(stdout.bytes()).unwrap();
            if is_event {
                assert!(rendered.contains(&active_event[..9]), "{rendered}");
            }
            assert!(rendered.contains(&active_session[..9]), "{rendered}");
        }
    }
}

#[test]
fn compact_search_keeps_original_refresh_pair_across_pointer_rotation() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (_, active) = compact_collision_pair(temp.path(), "rotationcompactauthorityneedle");
    let mut request = request(RefreshArg::Off);
    request.query = "rotationcompactauthorityneedle".to_owned();
    let refresh = refresh_for_search(
        &request,
        RefreshArg::Off,
        temp.path(),
        ctx_history_read_application::retained_peer_read_for_search(&request, true),
    )
    .unwrap();
    let pinned_generation = refresh.pin.generation_id().to_owned();
    let event = active.event.event_id.as_uuid().simple().to_string();
    let session = active.event.session_id.as_uuid().simple().to_string();

    let rotated = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 89, 3),
        "new pointer generation",
    );
    append_fixture_session(temp.path(), std::slice::from_ref(&rotated), 91);
    assert_ne!(
        open_index(temp.path()).unwrap().generation_id(),
        pinned_generation
    );

    let (full, compact, index) = search_existing_generation_with_compact_projection(
        &request,
        refresh.pin.into_index(),
        temp.path(),
    )
    .unwrap();

    assert_eq!(index.generation_id(), pinned_generation);
    assert_eq!(full["results"].as_array().unwrap().len(), 1);
    assert_eq!(
        full["results"][0]["ctx_event_id"],
        active.event.event_id.to_string()
    );
    assert_eq!(compact["results"][0]["ctx_event_id"], event[..9]);
    assert_eq!(compact["results"][0]["ctx_session_id"], session[..9]);
}

#[test]
fn search_compact_filters_require_peer_even_with_full_id_output() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let (retained, active) = compact_collision_pair(temp.path(), "selector authority needle");
    let session = active.event.session_id.as_uuid().simple().to_string();
    for exclude in [false, true] {
        for format in [JsonOutputFormat::Json, JsonOutputFormat::Text] {
            let mut args = lexical_search_args();
            args.query = Some("selector authority needle".to_owned());
            args.format = format;
            args.verbose = true;
            if exclude {
                args.exclude_sessions = vec![session[..8].to_owned()];
            } else {
                args.session = Some(session[..8].to_owned());
            }
            let (mut ui, _) = test_ui();
            let error = run_search(
                args,
                temp.path().to_path_buf(),
                history_snapshot(false, false),
                &mut CliUsage::excluded(),
                &mut ui,
                |_| {},
            )
            .unwrap_err();
            let detail = error.to_string();
            assert!(detail.contains("ambiguous"), "{detail}");
            assert!(
                detail.contains(&retained.event.session_id.to_string()),
                "{detail}"
            );
            assert!(
                detail.contains(&active.event.session_id.to_string()),
                "{detail}"
            );
        }
    }

    for selector in [active.event.session_id.to_string(), session[..9].to_owned()] {
        let mut args = lexical_search_args();
        args.query = Some("selector authority needle".to_owned());
        args.format = JsonOutputFormat::Json;
        args.session = Some(selector);
        let (mut ui, stdout) = test_ui();
        run_search(
            args,
            temp.path().to_path_buf(),
            history_snapshot(false, false),
            &mut CliUsage::excluded(),
            &mut ui,
            |_| {},
        )
        .unwrap();
        ui.flush().unwrap();
        let full: Value = serde_json::from_slice(&stdout.bytes()).unwrap();
        assert_eq!(full["results"].as_array().unwrap().len(), 1);
        assert_eq!(
            full["results"][0]["ctx_event_id"],
            active.event.event_id.to_string()
        );
        assert_eq!(
            full["results"][0]["citations"][0]["ctx_session_id"],
            active.event.session_id.to_string()
        );
    }
}
