mod common;

use ctx_provider_install::install_state::InstallTarget;

async fn test_daemon() -> (ctx_daemon::test_support::TestDaemon, tempfile::TempDir) {
    let common::FakeDaemonFixture { data_dir, daemon } =
        common::fake_daemon_fixture("http://127.0.0.1:4399").await;
    (daemon, data_dir)
}

#[tokio::test]
async fn start_install_seeds_same_start_event_into_info_polling_and_history() {
    let (daemon, _data_dir) = test_daemon().await;

    let (install_id, started_new) = daemon
        .start_install("title_generation_local".to_string(), None)
        .await;

    assert!(
        started_new,
        "new install should be marked as freshly started"
    );

    let info = daemon
        .get_install_info(install_id)
        .await
        .expect("missing install info");
    let polling_info = daemon
        .get_install_polling_info(install_id)
        .await
        .expect("missing polling install info");
    let events = daemon
        .get_install_events(install_id)
        .await
        .expect("missing install history");

    assert_eq!(
        events.len(),
        1,
        "fresh install should seed exactly one start event"
    );
    assert_eq!(events[0].stage, "start");
    assert_eq!(
        events[0].message,
        "Starting install for title_generation_local"
    );
    assert_eq!(
        info.last_event.as_ref().map(|event| &event.message),
        Some(&events[0].message)
    );
    assert_eq!(
        polling_info.last_event.as_ref().map(|event| &event.message),
        Some(&events[0].message)
    );
    assert_eq!(info.progress_pct, Some(2));
    assert_eq!(polling_info.progress_pct, Some(2));
}

#[tokio::test]
async fn start_install_dedupes_concurrent_requests_and_seeds_shared_start_event() {
    let (daemon, _data_dir) = test_daemon().await;

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let daemon = daemon.clone();
        tasks.push(tokio::spawn(async move {
            daemon
                .start_install("acp-crp-bridge".to_string(), Some(InstallTarget::Container))
                .await
        }));
    }

    let mut install_ids = Vec::new();
    let mut started_new_count = 0usize;
    for task in tasks {
        let (install_id, started_new) = task.await.expect("join start_install task");
        install_ids.push(install_id);
        if started_new {
            started_new_count += 1;
        }
    }

    assert_eq!(
        started_new_count, 1,
        "concurrent start_install callers must share one tracked running install"
    );
    assert!(
        install_ids
            .windows(2)
            .all(|pair| pair.first() == pair.get(1)),
        "all concurrent start_install callers should receive the same install id: {install_ids:#?}"
    );

    let install_id = *install_ids.first().expect("missing shared install id");
    let info = daemon
        .get_install_info(install_id)
        .await
        .expect("missing shared install info");
    let events = daemon
        .get_install_events(install_id)
        .await
        .expect("missing shared install events");

    assert_eq!(
        info.last_event.as_ref().map(|event| event.stage.as_str()),
        Some("start")
    );
    assert_eq!(info.progress_pct, Some(2));
    assert_eq!(
        events.len(),
        1,
        "joined installs should share one canonical start event"
    );
}
