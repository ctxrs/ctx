use super::*;

fn observation(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

fn publication_for_routes(
    generation: &str,
    routes: &BTreeSet<SourceRouteIdentity>,
) -> SourceBackedRefreshPublication {
    let mut publication = test_publication(generation);
    publication.route_results = routes
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect();
    publication
}

fn complete_running_all_with_demand(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    routes: &BTreeSet<SourceRouteIdentity>,
    publication_observations: BTreeMap<SourceRouteIdentity, String>,
    admission_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    demand_previous_generation: Option<String>,
    demand_request_id: &str,
    published_generation: &str,
) -> (String, String) {
    coordinator.initialize_watch_route_authority(routes.iter().cloned());
    let predecessor = coordinator.enqueue_periodic(data_root).unwrap();
    let predecessor_request_id = request_id(&predecessor);
    let mut demand = None;
    let run = coordinator
        .run_next_with(
            |running_request_id, running| {
                running.admit_refresh_scope_for_test(
                    running_request_id,
                    &SourceBackedRefreshScope::All,
                )?;
                running
                    .set_route_observations_for_test(running_request_id, publication_observations);
                demand = Some(running.enqueue_fresh_demand_for_test(
                    demand_previous_generation,
                    demand_request_id.to_owned(),
                    admission_observations,
                )?);
                Ok(publication_for_routes(published_generation, routes))
            },
            || Ok(Some(published_generation.to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("running automatic all-provider refresh");
    assert!(!run.failed, "{:#}", run.job);
    let demand_request_id = request_id(demand.as_ref().expect("fresh logical demand"));
    (predecessor_request_id, demand_request_id)
}

#[test]
fn running_cold_all_satisfies_fresh_demand_with_one_full_pass() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x61), route_identity(0x62)]);
    let observations = BTreeMap::from([
        (route_identity(0x61), observation(0xa1)),
        (route_identity(0x62), observation(0xa2)),
    ]);
    let admission = observations
        .iter()
        .map(|(route, value)| (route.clone(), Some(value.clone())))
        .collect();
    let demand_id = Uuid::from_u128(0x28101).to_string();
    let executor_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&executor_calls);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!(
                "covered demand must not launch another refresh attempt"
            ))
        },
    ));

    let (_, resolved_id) = complete_running_all_with_demand(
        &coordinator,
        &data_root,
        &routes,
        observations,
        admission,
        None,
        &demand_id,
        "full-pass-generation",
    );
    let resolution = coordinator
        .run_next(&data_root)
        .expect("covered logical demand resolution");

    assert_eq!(resolved_id, demand_id);
    assert_eq!(request_id(&resolution.job), demand_id);
    assert!(!resolution.did_work);
    assert!(!resolution.failed);
    assert_eq!(resolution.job["scanned_routes"], 0);
    assert_eq!(executor_calls.load(Ordering::SeqCst), 0);
    assert!(!coordinator.has_pending_request());
}

#[test]
fn post_snapshot_change_executes_only_exact_uncovered_delta() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let stable = route_identity(0x63);
    let changed = route_identity(0x64);
    let routes = BTreeSet::from([stable.clone(), changed.clone()]);
    let stable_observation = observation(0xb1);
    let publication_observations = BTreeMap::from([
        (stable.clone(), stable_observation.clone()),
        (changed.clone(), observation(0xb2)),
    ]);
    let admission_observations = BTreeMap::from([
        (stable.clone(), Some(stable_observation)),
        (changed.clone(), Some(observation(0xb3))),
    ]);
    let demand_id = Uuid::from_u128(0x28102).to_string();
    let coordinator = CoreRefreshEngine::new();
    complete_running_all_with_demand(
        &coordinator,
        &data_root,
        &routes,
        publication_observations,
        admission_observations,
        None,
        &demand_id,
        "predecessor-generation",
    );

    let mut selected_delta = None;
    let successor = coordinator
        .run_next_with(
            |request_id, running| {
                let covered = running
                    .admit_refresh_scope_for_test(request_id, &SourceBackedRefreshScope::All)?;
                selected_delta = Some(
                    routes
                        .difference(&covered)
                        .cloned()
                        .collect::<BTreeSet<_>>(),
                );
                Ok(publication_for_routes("delta-generation", &routes))
            },
            || Ok(Some("delta-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("exact uncovered delta");

    assert!(!successor.failed, "{:#}", successor.job);
    assert_eq!(request_id(&successor.job), demand_id);
    assert_eq!(selected_delta, Some(BTreeSet::from([changed])));
    assert!(!coordinator.has_pending_request());
}

#[test]
fn logical_demand_keeps_its_own_terminal_request_resolution() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x65);
    let routes = BTreeSet::from([route.clone()]);
    let token = observation(0xc1);
    let demand_id = Uuid::from_u128(0x28103).to_string();
    let coordinator = CoreRefreshEngine::new();
    let (predecessor_id, _) = complete_running_all_with_demand(
        &coordinator,
        &data_root,
        &routes,
        BTreeMap::from([(route.clone(), token.clone())]),
        BTreeMap::from([(route, Some(token))]),
        Some("caller-admission-generation".to_owned()),
        &demand_id,
        "shared-generation",
    );
    coordinator
        .run_next(&data_root)
        .expect("logical demand resolution");

    let predecessor = coordinator.status(&predecessor_id).unwrap();
    let demand = coordinator.status(&demand_id).unwrap();
    assert_ne!(predecessor["request_id"], demand["request_id"]);
    assert_eq!(demand["request_id"], demand_id);
    assert_eq!(demand["request_state"], "published");
    assert_eq!(demand["published_generation"], "shared-generation");
    assert_eq!(demand["receipt"], predecessor["receipt"]);
    assert_eq!(demand["receipt"]["previous_generation"], Value::Null);
    assert_eq!(
        demand["request_outcome"]["previous_generation"],
        "caller-admission-generation"
    );
}

#[test]
fn indeterminate_admission_observation_is_not_covered() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x66);
    let routes = BTreeSet::from([route.clone()]);
    let demand_id = Uuid::from_u128(0x28104).to_string();
    let coordinator = CoreRefreshEngine::new();
    complete_running_all_with_demand(
        &coordinator,
        &data_root,
        &routes,
        BTreeMap::from([(route.clone(), observation(0xd1))]),
        BTreeMap::from([(route.clone(), None)]),
        None,
        &demand_id,
        "indeterminate-predecessor",
    );

    let mut covered_routes = None;
    coordinator
        .run_next_with(
            |request_id, running| {
                covered_routes = Some(
                    running
                        .admit_refresh_scope_for_test(request_id, &SourceBackedRefreshScope::All)?,
                );
                Ok(publication_for_routes("indeterminate-delta", &routes))
            },
            || Ok(Some("indeterminate-delta".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("indeterminate route delta");

    assert_eq!(covered_routes, Some(BTreeSet::new()));
    assert_eq!(
        coordinator.status(&demand_id).unwrap()["request_state"],
        "published"
    );
}

#[test]
fn event_after_certified_boundary_survives_for_exact_delta() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x68);
    let routes = BTreeSet::from([route.clone()]);
    let token = observation(0xf1);
    let demand_id = Uuid::from_u128(0x28106).to_string();
    let coordinator = CoreRefreshEngine::new();
    coordinator.initialize_watch_route_authority(routes.iter().cloned());
    coordinator.enqueue_periodic(&data_root).unwrap();
    let predecessor = coordinator
        .run_next_with(
            |request_id, running| {
                running.admit_refresh_scope_for_test(request_id, &SourceBackedRefreshScope::All)?;
                running.set_route_observations_for_test(
                    request_id,
                    BTreeMap::from([(route.clone(), token.clone())]),
                );
                running.enqueue_fresh_demand_for_test(
                    None,
                    demand_id.clone(),
                    BTreeMap::from([(route.clone(), Some(token.clone()))]),
                )?;
                running.record_watch_routes(
                    [(route.clone(), EventWatermark::new(0, 1))],
                    ledger_now_ms(),
                );
                Ok(publication_for_routes("boundary-generation", &routes))
            },
            || Ok(Some("boundary-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("predecessor publication at admitted boundary");
    assert!(!predecessor.failed, "{:#}", predecessor.job);

    let mut covered_routes = None;
    coordinator
        .run_next_with(
            |request_id, running| {
                covered_routes = Some(
                    running
                        .admit_refresh_scope_for_test(request_id, &SourceBackedRefreshScope::All)?,
                );
                Ok(publication_for_routes("post-boundary-delta", &routes))
            },
            || Ok(Some("post-boundary-delta".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("post-boundary exact delta");

    assert_eq!(covered_routes, Some(BTreeSet::new()));
    assert_eq!(
        coordinator.status(&demand_id).unwrap()["request_state"],
        "published"
    );
}

#[test]
fn restart_before_publication_preserves_logical_demand_fence() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x67);
    let routes = BTreeSet::from([route.clone()]);
    let token = observation(0xe1);
    let demand_id = Uuid::from_u128(0x28105).to_string();
    let first = CoreRefreshEngine::new();
    first.initialize_watch_route_authority(routes.iter().cloned());
    first.enqueue_periodic(&data_root).unwrap();
    let interrupted = first
        .run_next_with(
            |running_request_id, running| {
                running.admit_refresh_scope_for_test(
                    running_request_id,
                    &SourceBackedRefreshScope::All,
                )?;
                let demand = running.enqueue_fresh_demand_for_test(
                    None,
                    demand_id.clone(),
                    BTreeMap::from([(route.clone(), Some(token.clone()))]),
                )?;
                assert_eq!(request_id(&demand), demand_id);
                running.persist_job_status_for_test(&data_root, running_request_id)?;
                Err(anyhow!(
                    "simulate process exit before predecessor publication"
                ))
            },
            || Ok(None),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("interrupted predecessor");
    assert!(interrupted.failed);
    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("durable running predecessor and logical demand");
    assert_eq!(durable["queued_successors"][0]["request_id"], demand_id);
    assert!(durable["queued_successors"][0]["logical_demand"].is_object());
    drop(first);

    let executor_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&executor_calls);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("recovered covered demand must not execute a delta"))
        },
    ));
    restarted.initialize_watch_route_authority(routes.iter().cloned());
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let predecessor = restarted
        .run_next_with(
            |request_id, running| {
                running.admit_refresh_scope_for_test(request_id, &SourceBackedRefreshScope::All)?;
                running.set_route_observations_for_test(
                    request_id,
                    BTreeMap::from([(route.clone(), token.clone())]),
                );
                Ok(publication_for_routes("recovered-generation", &routes))
            },
            || Ok(Some("recovered-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("recovered predecessor publication");
    assert!(!predecessor.failed, "{:#}", predecessor.job);
    let resolution = restarted
        .run_next(&data_root)
        .expect("recovered logical demand resolution");

    assert_eq!(request_id(&resolution.job), demand_id);
    assert!(!resolution.did_work);
    assert_eq!(executor_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        restarted.status(&demand_id).unwrap()["request_state"],
        "published"
    );
}
