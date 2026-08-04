use super::*;

type SelectedRouteDeltas = Arc<Mutex<Vec<BTreeSet<SourceRouteIdentity>>>>;

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

fn verified_publication_for_observations(
    execution: &SourceBackedRefreshExecution<'_>,
    observations: &BTreeMap<SourceRouteIdentity, String>,
) -> Result<SourceBackedRefreshPublication> {
    let previous_generation = open_verified_index(execution.index_root)
        .ok()
        .map(|index| index.generation_id().to_owned());
    let request_id = execution.request_id.to_owned();
    let operation = execution.operation;
    let scope = execution.scope.clone();
    let metadata_observations = observations.clone();
    let route_results = observations
        .keys()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect::<Vec<_>>();
    let covered_publication = execution.covered_publication.clone();
    let published =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::semantic::committed_generation_recovery_error)?
            .commit_with_publication_metadata(
                |_| true,
                move |context| {
                    let mut publication =
                        empty_test_publication(context.generation_id().to_owned());
                    publication.route_results = route_results.clone();
                    covered_publication.apply_receipt(&mut publication);
                    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                        previous_generation.clone(),
                        context.generation_id().to_owned(),
                        &publication,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                    SourceBackedPublicationMetadata {
                        request_id: request_id.clone(),
                        operation,
                        refresh_scope: scope.clone(),
                        receipt: receipt.to_json(),
                        route_observations: metadata_observations.clone(),
                    }
                    .encode()
                },
            )?;
    let mut publication = empty_test_publication(published.receipt().generation_id.clone());
    publication.route_results = observations
        .keys()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect();
    execution
        .covered_publication
        .apply_receipt(&mut publication);
    Ok(publication)
}

fn complete_verified_fully_covered_demand(
    data_root: &Path,
    route: &SourceRouteIdentity,
    route_observation: &str,
    demand_id: &str,
    demand_previous_generation: Option<String>,
    panic_on_delta: bool,
) -> (Arc<CoreRefreshEngine>, SelectedRouteDeltas) {
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let observations = BTreeMap::from([(route.clone(), route_observation.to_owned())]);
    let executor_observations = observations.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let executor_executions = Arc::clone(&executions);
    let selected_deltas = Arc::new(Mutex::new(Vec::new()));
    let executor_deltas = Arc::clone(&selected_deltas);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            if executor_executions.fetch_add(1, Ordering::SeqCst) == 0 {
                executor_entered.wait();
                executor_release.wait();
                return verified_publication_for_observations(&execution, &executor_observations);
            }
            let selected = executor_observations
                .keys()
                .filter(|route| !execution.covered_route_ids.contains(*route))
                .cloned()
                .collect::<BTreeSet<_>>();
            executor_deltas.lock().unwrap().push(selected.clone());
            if panic_on_delta {
                panic!("injected crash after exact delta became durable");
            }
            let selected_observations = executor_observations
                .iter()
                .filter(|(route, _)| selected.contains(*route))
                .map(|(route, observation)| (route.clone(), observation.clone()))
                .collect();
            verified_publication_for_observations(&execution, &selected_observations)
        },
    )));
    coordinator.initialize_watch_route_authority([route.clone()]);
    coordinator.enqueue_periodic(data_root).unwrap();
    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.to_path_buf();
        let predecessor = scope.spawn(move || {
            runner
                .run_next(&runner_root)
                .expect("verified predecessor publication")
        });
        entered.wait();
        coordinator
            .enqueue_fresh_demand_for_test(
                demand_previous_generation,
                demand_id.to_owned(),
                observations
                    .iter()
                    .map(|(route, observation)| (route.clone(), Some(observation.clone())))
                    .collect(),
            )
            .unwrap();
        release.wait();
        let predecessor = predecessor.join().unwrap();
        assert!(!predecessor.failed, "{:#}", predecessor.job);
    });
    (coordinator, selected_deltas)
}

struct RunningAllDemand<'a> {
    publication_observations: BTreeMap<SourceRouteIdentity, String>,
    admission_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    previous_generation: Option<String>,
    request_id: &'a str,
    published_generation: &'a str,
}

fn complete_running_all_with_demand(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    routes: &BTreeSet<SourceRouteIdentity>,
    demand: RunningAllDemand<'_>,
) -> (String, String) {
    let RunningAllDemand {
        publication_observations,
        admission_observations,
        previous_generation,
        request_id: demand_request_id,
        published_generation,
    } = demand;
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
                    previous_generation,
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
    let route = route_identity(0x61);
    let route_observation = observation(0xa1);
    let demand_id = Uuid::from_u128(0x28101).to_string();
    let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
        &data_root,
        &route,
        &route_observation,
        &demand_id,
        None,
        false,
    );
    let sampled_route = route;
    let sampled_observation = route_observation;
    let resolution = coordinator
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("covered logical demand resolution");

    assert_eq!(request_id(&resolution.job), demand_id);
    assert!(!resolution.did_work);
    assert!(!resolution.failed);
    assert_eq!(resolution.job["scanned_routes"], 0);
    assert!(selected_deltas.lock().unwrap().is_empty());
    assert!(!coordinator.has_pending_request());
}

#[test]
fn fully_covered_resolver_samples_after_seen_fence_and_extends_matching_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x69);
    let route_observation = observation(0xa9);
    let demand_id = Uuid::from_u128(0x28109).to_string();
    let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
        &data_root,
        &route,
        &route_observation,
        &demand_id,
        None,
        false,
    );
    let seen_during_capture = EventWatermark::new(0, 4);
    coordinator.set_route_event_watermark_for_test(route.clone(), seen_during_capture);
    let sampled = Arc::new(AtomicBool::new(false));
    let sampled_by_resolver = Arc::clone(&sampled);
    let sampled_route = route.clone();
    let sampled_observation = route_observation.clone();

    let resolution = coordinator
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            sampled_by_resolver.store(true, Ordering::SeqCst);
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("fully covered logical resolution");
    let certificate = resolution
        .coverage_certificate()
        .expect("matching post-publication observation certificate");

    assert!(sampled.load(Ordering::SeqCst));
    assert_eq!(certificate.request_id(), demand_id);
    assert_eq!(
        certificate.exact_route_boundaries().collect::<Vec<_>>(),
        vec![(&route, seen_during_capture, route_observation.as_str())]
    );
    assert!(selected_deltas.lock().unwrap().is_empty());
}

#[test]
fn fully_covered_resolver_mismatch_and_unavailable_samples_execute_exact_delta() {
    for (case, sampled_observation) in
        [("mismatch", Some(observation(0xbb))), ("unavailable", None)]
    {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
        let route = route_identity(0x6a);
        let route_observation = observation(0xaa);
        let demand_id = Uuid::now_v7().to_string();
        let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
            &data_root,
            &route,
            &route_observation,
            &demand_id,
            None,
            false,
        );
        coordinator.set_route_event_watermark_for_test(route.clone(), EventWatermark::new(0, 5));
        let sampled = Arc::new(AtomicBool::new(false));
        let sampled_by_resolver = Arc::clone(&sampled);
        let sampled_route = route.clone();

        let resolution = coordinator
            .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
                sampled_by_resolver.store(true, Ordering::SeqCst);
                Ok(BTreeMap::from([(sampled_route, sampled_observation)]))
            })
            .unwrap_or_else(|| panic!("{case} exact delta resolution"));

        assert!(sampled.load(Ordering::SeqCst), "{case}");
        assert!(resolution.did_work, "{case}");
        assert_eq!(request_id(&resolution.job), demand_id, "{case}");
        assert_eq!(resolution.job["request_state"], "published", "{case}");
        assert_eq!(
            selected_deltas.lock().unwrap().as_slice(),
            &[BTreeSet::from([route.clone()])],
            "{case}"
        );
    }
}

#[test]
fn unavailable_fully_covered_sample_persists_exact_delta_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x6f);
    let route_observation = observation(0xaf);
    let demand_id = Uuid::from_u128(0x2810b).to_string();
    let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
        &data_root,
        &route,
        &route_observation,
        &demand_id,
        None,
        true,
    );
    coordinator.set_route_event_watermark_for_test(route.clone(), EventWatermark::new(0, 6));
    let sampled_route = route.clone();

    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        coordinator.run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, None)]))
        })
    }));
    assert!(crashed.is_err());
    assert_eq!(
        selected_deltas.lock().unwrap().as_slice(),
        &[BTreeSet::from([route.clone()])]
    );
    let interrupted = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("durable exact delta before execution");
    assert_eq!(interrupted["request_id"], demand_id);
    assert_eq!(interrupted["request_state"], "running");
    assert_eq!(
        interrupted["logical_demand"]["invalidated_routes"],
        json!([route.as_str()])
    );
    drop(coordinator);

    let restarted_deltas = Arc::new(Mutex::new(Vec::new()));
    let recorded_deltas = Arc::clone(&restarted_deltas);
    let restart_route = route.clone();
    let restart_observation = route_observation.clone();
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = BTreeSet::from([restart_route.clone()])
                .difference(&execution.covered_route_ids)
                .cloned()
                .collect::<BTreeSet<_>>();
            recorded_deltas.lock().unwrap().push(selected.clone());
            let observations = selected
                .into_iter()
                .map(|route| (route, restart_observation.clone()))
                .collect();
            verified_publication_for_observations(&execution, &observations)
        },
    ));
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted.status(&demand_id).unwrap()["request_state"],
        "queued"
    );
    let recovered = restarted
        .run_next(&data_root)
        .expect("recovered exact delta");
    assert_eq!(request_id(&recovered.job), demand_id);
    assert_eq!(recovered.job["request_state"], "published");
    assert_eq!(
        restarted_deltas.lock().unwrap().as_slice(),
        &[BTreeSet::from([route])]
    );
}

#[test]
fn regular_publication_fence_skips_sampling_without_a_post_admission_event() {
    let route = route_identity(0x6c);
    let observation = observation(0xac);
    let coordinator = CoreRefreshEngine::new();
    coordinator.initialize_watch_route_authority([route.clone()]);
    let request = coordinator.enqueue_for_test(None);
    let request_id = request_id(&request);
    coordinator
        .admit_refresh_scope_for_test(&request_id, &SourceBackedRefreshScope::All)
        .unwrap();
    coordinator.set_route_observations_for_test(
        &request_id,
        BTreeMap::from([(route.clone(), observation.clone())]),
    );
    let admitted = EventWatermark::new(0, 0);
    assert_eq!(
        coordinator.route_event_watermark_for_test(&route),
        Some(admitted)
    );
    let mut sampled = false;

    let fence = coordinator.regular_post_publication_route_coverage_fence_for_test(
        &request_id,
        |_routes| {
            sampled = true;
            Ok(BTreeMap::new())
        },
    );

    assert!(!sampled);
    assert_eq!(
        fence.certified_boundary(&route, admitted, &observation),
        admitted
    );
}

#[test]
fn regular_publication_fence_samples_only_advanced_route_and_bounds_later_event() {
    let advanced = route_identity(0x6d);
    let steady = route_identity(0x6e);
    let advanced_observation = observation(0xad);
    let steady_observation = observation(0xae);
    let coordinator = CoreRefreshEngine::new();
    coordinator.initialize_watch_route_authority([advanced.clone(), steady.clone()]);
    let request = coordinator.enqueue_for_test(None);
    let request_id = request_id(&request);
    coordinator
        .admit_refresh_scope_for_test(&request_id, &SourceBackedRefreshScope::All)
        .unwrap();
    coordinator.set_route_observations_for_test(
        &request_id,
        BTreeMap::from([
            (advanced.clone(), advanced_observation.clone()),
            (steady.clone(), steady_observation.clone()),
        ]),
    );
    let admitted = EventWatermark::new(0, 0);
    let seen_before_fence = EventWatermark::new(0, 1);
    let event_after_fence = EventWatermark::new(0, 2);
    coordinator.set_route_event_watermark_for_test(advanced.clone(), seen_before_fence);
    let sampled_route = advanced.clone();
    let sampled_observation = advanced_observation.clone();

    let fence =
        coordinator.regular_post_publication_route_coverage_fence_for_test(&request_id, |routes| {
            assert_eq!(routes, &BTreeSet::from([sampled_route.clone()]));
            coordinator
                .set_route_event_watermark_for_test(sampled_route.clone(), event_after_fence);
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        });

    let advanced_boundary = fence.certified_boundary(&advanced, admitted, &advanced_observation);
    assert_eq!(advanced_boundary, seen_before_fence);
    assert_eq!(
        fence.certified_boundary(&steady, admitted, &steady_observation),
        admitted
    );
    assert_eq!(
        coordinator.route_event_watermark_for_test(&advanced),
        Some(event_after_fence)
    );
    assert!(advanced_boundary < event_after_fence);
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
        RunningAllDemand {
            publication_observations,
            admission_observations,
            previous_generation: None,
            request_id: &demand_id,
            published_generation: "predecessor-generation",
        },
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
    let token = observation(0xc1);
    let demand_id = Uuid::from_u128(0x28103).to_string();
    let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
        &data_root,
        &route,
        &token,
        &demand_id,
        Some("caller-admission-generation".to_owned()),
        false,
    );
    let predecessor_id = SourceBackedPublicationMetadata::decode(
        &open_verified_index(&source_backed_index_root(&data_root)).unwrap(),
    )
    .unwrap()
    .request_id;
    let sampled_route = route;
    let sampled_observation = token;
    coordinator
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("logical demand resolution");
    assert!(selected_deltas.lock().unwrap().is_empty());

    let predecessor = coordinator.status(&predecessor_id).unwrap();
    let demand = coordinator.status(&demand_id).unwrap();
    assert_ne!(predecessor["request_id"], demand["request_id"]);
    assert_eq!(demand["request_id"], demand_id);
    assert_eq!(demand["request_state"], "published");
    assert_eq!(
        demand["published_generation"],
        predecessor["published_generation"]
    );
    assert_eq!(demand["receipt"], predecessor["receipt"]);
    assert_eq!(demand["receipt"]["previous_generation"], Value::Null);
    assert_eq!(
        demand["request_outcome"]["previous_generation"],
        "caller-admission-generation"
    );
}

#[test]
fn fully_covered_logical_terminal_recovers_stable_uuid_and_exact_response() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x70);
    let route_observation = observation(0xb0);
    let demand_id = Uuid::from_u128(0x2810c).to_string();
    let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
        &data_root,
        &route,
        &route_observation,
        &demand_id,
        None,
        false,
    );
    let metadata = SourceBackedPublicationMetadata::decode(
        &open_verified_index(&source_backed_index_root(&data_root)).unwrap(),
    )
    .unwrap();
    assert_ne!(metadata.request_id, demand_id);
    let sampled_route = route;
    let sampled_observation = route_observation;
    coordinator
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("fully covered logical terminal");
    assert!(selected_deltas.lock().unwrap().is_empty());
    let exact_response = coordinator.status(&demand_id).unwrap();
    assert_eq!(exact_response["request_id"], demand_id);
    assert_eq!(exact_response["request_state"], "published");
    drop(coordinator);

    let restarted = CoreRefreshEngine::new();
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(restarted.status(&demand_id).unwrap(), exact_response);
    assert!(restarted.status(&metadata.request_id).is_none());
    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("stable logical terminal after restart");
    assert_eq!(durable["request_id"], demand_id);
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
        RunningAllDemand {
            publication_observations: BTreeMap::from([(route.clone(), observation(0xd1))]),
            admission_observations: BTreeMap::from([(route.clone(), None)]),
            previous_generation: None,
            request_id: &demand_id,
            published_generation: "indeterminate-predecessor",
        },
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
    let executor_observations = BTreeMap::from([(route.clone(), token.clone())]);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            if observed_calls.fetch_add(1, Ordering::SeqCst) != 0 {
                bail!("recovered covered demand must not execute a delta");
            }
            verified_publication_for_observations(&execution, &executor_observations)
        },
    ));
    restarted.initialize_watch_route_authority(routes.iter().cloned());
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let predecessor = restarted
        .run_next(&data_root)
        .expect("recovered predecessor publication");
    assert!(!predecessor.failed, "{:#}", predecessor.job);
    let sampled_route = route;
    let sampled_observation = token;
    let resolution = restarted
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("recovered logical demand resolution");

    assert_eq!(request_id(&resolution.job), demand_id);
    assert!(!resolution.did_work);
    assert_eq!(executor_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        restarted.status(&demand_id).unwrap()["request_state"],
        "published"
    );
}

#[test]
fn restart_after_predecessor_publication_recovers_predecessor_root_and_logical_continuation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x6b);
    let route_observation = observation(0xab);
    let observations = BTreeMap::from([(route.clone(), route_observation.clone())]);
    let demand_id = Uuid::from_u128(0x2810a).to_string();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_observations = observations.clone();
    let status_writer = Arc::new(|path: &Path, job: &Value| {
        let continuation_finished = job
            .get("queued_successors")
            .and_then(Value::as_array)
            .and_then(|successors| successors.first())
            .and_then(|successor| successor.get("logical_demand"))
            .and_then(|demand| demand.get("predecessor_finished"))
            .and_then(Value::as_bool)
            == Some(true);
        if continuation_finished {
            panic!("injected crash while persisting predecessor-bound continuation");
        }
        write_daemon_job_status(path, job)
    });
    let first = Arc::new(CoreRefreshEngine::with_status_writer_for_test(
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            executor_entered.wait();
            executor_release.wait();
            verified_publication_for_observations(&execution, &executor_observations)
        }),
        status_writer,
    ));
    first.initialize_watch_route_authority([route.clone()]);
    let predecessor = first.enqueue_periodic(&data_root).unwrap();
    let predecessor_id = request_id(&predecessor);

    let crashed = std::thread::scope(|scope| {
        let runner = Arc::clone(&first);
        let runner_root = data_root.clone();
        let run = scope.spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runner.run_next(&runner_root)
            }))
        });
        entered.wait();
        let demand = first
            .enqueue_fresh_demand_for_test(
                None,
                demand_id.clone(),
                observations
                    .iter()
                    .map(|(route, observation)| (route.clone(), Some(observation.clone())))
                    .collect(),
            )
            .unwrap();
        assert_eq!(request_id(&demand), demand_id);
        release.wait();
        run.join().unwrap()
    });
    assert!(crashed.is_err());
    let interrupted = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("published predecessor root before continuation persistence");
    assert_eq!(interrupted["request_id"], predecessor_id);
    assert_eq!(interrupted["request_state"], "published");
    assert_eq!(interrupted["queued_successors"][0]["request_id"], demand_id);
    assert_eq!(
        interrupted["queued_successors"][0]["logical_demand"]["predecessor_finished"],
        false
    );
    drop(first);

    let recaptures = Arc::new(AtomicUsize::new(0));
    let observed_recaptures = Arc::clone(&recaptures);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_recaptures.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("covered logical continuation must not recapture"))
        },
    ));
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let recovered_root = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("recovered predecessor-rooted continuation");
    assert_eq!(recovered_root["request_id"], predecessor_id);
    assert_eq!(recovered_root["request_state"], "published");
    assert_eq!(
        recovered_root["queued_successors"][0]["request_id"],
        demand_id
    );
    assert_eq!(
        recovered_root["queued_successors"][0]["logical_demand"]["predecessor_finished"],
        true
    );

    let sampled_route = route.clone();
    let sampled_observation = route_observation.clone();
    let resolution = restarted
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("recovered logical continuation resolution");
    assert_eq!(request_id(&resolution.job), demand_id);
    assert!(!resolution.did_work);
    assert_eq!(recaptures.load(Ordering::SeqCst), 0);
}
