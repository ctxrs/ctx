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

struct TestStatusWriterJournal {
    writer: TestStatusWriter,
}

impl RefreshJournal for TestStatusWriterJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        Ok(read_daemon_job_status(
            &daemon_source_backed_refresh_job_path(data_root),
        ))
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        (self.writer)(&daemon_source_backed_refresh_job_path(data_root), value)
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        match self.store(data_root, value) {
            Ok(()) => DurableAdmissionPersistence::Confirmed,
            Err(error)
                if read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root))
                    .as_ref()
                    == Some(value) =>
            {
                DurableAdmissionPersistence::Retained(error)
            }
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
        Ok(DiscoveryContext::from_process(data_root.join("test-home")))
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

pub(super) fn test_refresh_engine_with_status_writer(
    executor: Arc<dyn SourceBackedRefreshExecutor>,
    writer: TestStatusWriter,
) -> CoreRefreshEngine {
    CoreRefreshEngine::with_executor(
        Arc::new(TestStatusWriterJournal { writer }),
        test_refresh_runtime(),
        executor,
    )
}

pub(super) fn test_refresh_submission(request_id: &str) -> RefreshSubmission {
    RefreshSubmission::new(
        request_id.to_owned(),
        RefreshOperation::Refresh,
        None,
        SourceBackedRefreshScope::All,
        true,
        false,
    )
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

pub(super) fn open_test_published_generation(data_root: &Path) -> Result<Option<VerifiedIndex>> {
    crate::publication::open_published_generation(data_root, &TestFileRefreshJournal)
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

    pub fn logical_continuation_is_fully_covered_for_test(&self, request_id: &str) -> bool {
        self.lock_state()
            .manual_all_continuations
            .get(request_id)
            .is_some_and(ManualAllContinuation::is_fully_covered)
    }
}
