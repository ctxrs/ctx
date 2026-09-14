use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct AdvancingAvailability(AtomicU64);

impl crate::DaemonAvailabilityPort for AdvancingAvailability {
    fn ensure_available(
        &self,
        _: &Path,
        _: crate::DaemonTrigger,
        _: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        bail!("a responsive daemon must not be restarted")
    }

    fn pause(&self, _: StdDuration) -> Result<()> {
        if self.0.fetch_add(150, Ordering::SeqCst) >= 600 {
            bail!("test stopped an unbounded observation loop")
        }
        Ok(())
    }
}

#[test]
fn responsive_stalled_wait_returns_without_discarding_the_durable_request() -> Result<()> {
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let engine = Arc::new(CoreRefreshEngine::new());
    let server_engine = engine.clone();
    let server_root = data_root.path().to_owned();
    let request_id = Uuid::new_v4().to_string();
    let availability = AdvancingAvailability::default();
    let start = StdInstant::now();
    let intent = RefreshIntent::SelectedImport(RefreshSelection::All);
    let (result, exchanges) = foreground_transport_fixture(
        data_root.path(),
        move |request| {
            server_engine
                .handle_ipc_request(&server_root, request)?
                .context("wire response")
        },
        || -> Result<SourceBackedRefreshObservation> {
            enqueue_equivalent_wait_refresh_request(
                &availability,
                data_root.path(),
                &request_id,
                intent.clone(),
                RefreshRequestTrigger::Import,
            )?;
            wait_for_published_generation_inner(
                &availability,
                data_root.path(),
                request_id.clone(),
                PublishedGenerationWait {
                    mode: SourceBackedRefreshMode::Wait,
                    intent,
                    trigger: RefreshRequestTrigger::Import,
                    allow_daemon_autostart: true,
                    retain_peer: false,
                    report_progress: None,
                },
                || start + StdDuration::from_secs(availability.0.load(Ordering::SeqCst)),
            )
        },
    )?;
    let error = result.err().context("stalled observation must fail")?;
    assert!(
        error.to_string().contains("no observable progress"),
        "{error:#}"
    );
    assert!(error.to_string().contains(&request_id));
    assert!(error.to_string().contains("outcome is unknown"));
    assert_eq!(availability.0.load(Ordering::SeqCst), 300);
    assert_eq!(
        exchanges
            .iter()
            .filter(|(request, _)| request["op"] == SOURCE_REFRESH_REQUEST_OP)
            .count(),
        1,
        "stalled observation must not resubmit the request"
    );
    assert!(engine.has_pending_request());
    assert_eq!(
        engine.status(&request_id).unwrap()["request_state"],
        "admission_pending"
    );
    let recovered = CoreRefreshEngine::new();
    assert!(recovered.recover_interrupted_publication(data_root.path())?);
    assert_eq!(
        recovered.status(&request_id).unwrap()["request_state"],
        "admission_pending"
    );
    Ok(())
}
