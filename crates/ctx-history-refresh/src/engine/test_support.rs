use super::*;

#[derive(Debug, Clone, Copy, Default)]
enum TestAdmissionPersistence {
    #[default]
    Confirmed,
    Retained(&'static str),
    Failed(&'static str),
}

#[derive(Debug, Default)]
pub(crate) struct TestRefreshJournal {
    jobs: Mutex<HashMap<PathBuf, Value>>,
    admission_persistence: TestAdmissionPersistence,
}

impl TestRefreshJournal {
    pub(super) fn failing_before_ack() -> Self {
        Self {
            admission_persistence: TestAdmissionPersistence::Failed(
                "injected durable admission failure",
            ),
            ..Self::default()
        }
    }

    pub(super) fn retaining_after_ack_write(message: &'static str) -> Self {
        Self {
            admission_persistence: TestAdmissionPersistence::Retained(message),
            ..Self::default()
        }
    }
}

impl RefreshJournal for TestRefreshJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        Ok(self.jobs.lock().unwrap().get(data_root).cloned())
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        self.jobs
            .lock()
            .unwrap()
            .insert(data_root.to_path_buf(), value.clone());
        Ok(())
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        if let TestAdmissionPersistence::Failed(message) = self.admission_persistence {
            return DurableAdmissionPersistence::Failed(anyhow!(message));
        }
        if let Err(error) = self.store(data_root, value) {
            return DurableAdmissionPersistence::Failed(error);
        }
        match self.admission_persistence {
            TestAdmissionPersistence::Confirmed => DurableAdmissionPersistence::Confirmed,
            TestAdmissionPersistence::Retained(message) => {
                DurableAdmissionPersistence::Retained(anyhow!(message))
            }
            TestAdmissionPersistence::Failed(_) => unreachable!(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct TestFailTerminalStoreJournal {
    terminal_stores: std::sync::atomic::AtomicUsize,
    fail_on_terminal_store: usize,
}

impl TestFailTerminalStoreJournal {
    pub(super) fn failing_on(fail_on_terminal_store: usize) -> Self {
        Self {
            terminal_stores: std::sync::atomic::AtomicUsize::new(0),
            fail_on_terminal_store,
        }
    }

    pub(super) fn terminal_store_count(&self) -> usize {
        self.terminal_stores.load(Ordering::SeqCst)
    }
}

impl Default for TestFailTerminalStoreJournal {
    fn default() -> Self {
        Self::failing_on(2)
    }
}

impl RefreshJournal for TestFailTerminalStoreJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        Ok(read_daemon_job_status(
            &daemon_source_backed_refresh_job_path(data_root),
        ))
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        let terminal = matches!(
            value.get("request_state").and_then(Value::as_str),
            Some("published" | "failed")
        );
        if terminal
            && self
                .terminal_stores
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1)
                == self.fail_on_terminal_store
        {
            bail!("injected route-finalization persistence failure");
        }
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), value)
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        match self.store(data_root, value) {
            Ok(()) => DurableAdmissionPersistence::Confirmed,
            Err(error) => DurableAdmissionPersistence::Failed(error),
        }
    }
}

#[derive(Debug, Default)]
struct TestFileRefreshJournal;

pub(super) fn daemon_source_backed_refresh_job_path(data_root: &Path) -> PathBuf {
    data_root
        .join("daemon")
        .join("jobs")
        .join("core-refresh.json")
}

pub(super) fn read_daemon_job_status(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub(super) fn write_daemon_job_status(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

impl RefreshJournal for TestFileRefreshJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        Ok(read_daemon_job_status(
            &daemon_source_backed_refresh_job_path(data_root),
        ))
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), value)
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        match self.store(data_root, value) {
            Ok(()) => DurableAdmissionPersistence::Confirmed,
            Err(error) => DurableAdmissionPersistence::Failed(error),
        }
    }
}

#[derive(Debug, Default)]
struct TestRefreshRuntime;

impl RefreshRuntime for TestRefreshRuntime {
    fn metadata(&self, _data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        match operation {
            RefreshOperation::Refresh => RefreshRuntimeMetadata::default(),
            RefreshOperation::Import => RefreshRuntimeMetadata {
                operation,
                daemon_mode: "full".to_owned(),
                trigger: "import",
                trigger_provenance: "explicit_source_catalog",
            },
        }
    }

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext> {
        let test_home = data_root.parent().unwrap_or(data_root).join("test-home");
        Ok(DiscoveryContext::from_process(test_home))
    }
}

pub(super) fn test_refresh_runtime() -> Arc<dyn RefreshRuntime> {
    Arc::new(TestRefreshRuntime)
}

pub(super) fn test_refresh_engine() -> CoreRefreshEngine {
    CoreRefreshEngine::new(Arc::new(TestFileRefreshJournal), test_refresh_runtime())
}

pub(super) fn test_refresh_engine_with_executor(
    executor: Arc<dyn SourceBackedRefreshExecutor>,
) -> CoreRefreshEngine {
    CoreRefreshEngine::with_executor(
        Arc::new(TestFileRefreshJournal),
        test_refresh_runtime(),
        executor,
    )
}

pub(super) fn test_refresh_engine_with_executor_and_admitted_routes(
    executor: Arc<dyn SourceBackedRefreshExecutor>,
    routes: impl IntoIterator<Item = SourceRouteIdentity>,
) -> CoreRefreshEngine {
    test_refresh_engine_with_journal_executor_and_admitted_routes(
        Arc::new(TestFileRefreshJournal),
        executor,
        routes,
    )
}

pub(super) fn test_refresh_engine_with_journal_executor_and_admitted_routes(
    journal: Arc<dyn RefreshJournal>,
    executor: Arc<dyn SourceBackedRefreshExecutor>,
    routes: impl IntoIterator<Item = SourceRouteIdentity>,
) -> CoreRefreshEngine {
    let observations = routes
        .into_iter()
        .map(|route| (route, Some("ab".repeat(32))))
        .collect::<BTreeMap<_, _>>();
    CoreRefreshEngine::with_runtime_for_test(
        journal,
        test_refresh_runtime(),
        executor,
        Arc::new(move |_, _, _, _| Ok(observations.clone())),
    )
}

pub(super) fn test_refresh_submission(request_id: &str) -> RefreshRequest {
    RefreshRequest::selected_import(request_id.to_owned(), RefreshSelection::All)
}

pub(super) fn status_value(engine: &CoreRefreshEngine, request_id: &str) -> Value {
    engine
        .status(request_id)
        .expect("refresh status")
        .schema_v1_fields()
        .clone()
}

pub(super) fn pin_test_published_generation(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    crate::pin_published_generation(data_root, &TestFileRefreshJournal)
}

pub(super) fn pin_test_active_verified_generation(
    data_root: &Path,
) -> Result<PinnedSourceBackedGeneration> {
    crate::pin_active_verified_generation(data_root, &TestFileRefreshJournal)
}

impl CoreRefreshEngine {
    pub fn status_for_test(&self, request_id: &str) -> Option<Value> {
        self.status(request_id)
            .map(|status| status.schema_v1_fields().clone())
    }

    pub fn dirty_route_ids_for_test(&self) -> BTreeSet<SourceRouteIdentity> {
        self.lock_state().dirty_routes.route_ids()
    }

    pub fn route_is_permanently_blocked_for_test(&self, route: &SourceRouteIdentity) -> bool {
        self.lock_state().dirty_routes.is_permanently_blocked(route)
    }

    pub fn exhaustive_route_obligations_for_test(&self) -> BTreeSet<SourceRouteIdentity> {
        let state = self.lock_state();
        state
            .hermes_routes_requiring_exhaustive_recovery
            .union(&state.routes_requiring_exhaustive_reconciliation)
            .cloned()
            .collect()
    }
}
