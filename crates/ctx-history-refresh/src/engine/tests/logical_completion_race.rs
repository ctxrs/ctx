use super::*;

use super::logical_demand::complete_verified_fully_covered_demand;

fn observation(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

#[test]
fn successor_admitted_during_logical_sampling_is_durably_unfenced() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x7a);
    let route_observation = observation(0xba);
    let first_demand_id = Uuid::from_u128(0x28120).to_string();
    let second_demand_id = Uuid::from_u128(0x28121).to_string();
    let (coordinator, selected_deltas) = complete_verified_fully_covered_demand(
        &data_root,
        &route,
        &route_observation,
        &first_demand_id,
        None,
        false,
    );

    let admitting_coordinator = Arc::clone(&coordinator);
    let admitted_route = route.clone();
    let admitted_observation = route_observation.clone();
    let admitted_request_id = second_demand_id.clone();
    let sampled_route = route.clone();
    let sampled_observation = route_observation.clone();
    let first_resolution = coordinator
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            admitting_coordinator.enqueue_fresh_demand_for_test(
                None,
                admitted_request_id,
                BTreeMap::from([(admitted_route, Some(admitted_observation))]),
            )?;
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("first logical demand resolution");
    assert_eq!(request_id(&first_resolution.job), first_demand_id);

    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("durable terminal root with queued successor");
    assert_eq!(
        durable["queued_successors"][0]["logical_demand"]["predecessor_finished"],
        true
    );

    let second_resolution = coordinator
        .run_next(&data_root)
        .expect("successor must not remain predecessor-fenced");
    assert_eq!(request_id(&second_resolution.job), second_demand_id);
    assert!(second_resolution.did_work);
    assert!(!second_resolution.failed, "{:#}", second_resolution.job);
    assert_eq!(
        selected_deltas.lock().unwrap().as_slice(),
        &[BTreeSet::from([route])]
    );
    assert!(!coordinator.has_pending_request());
}
