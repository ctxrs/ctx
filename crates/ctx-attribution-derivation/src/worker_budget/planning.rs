use std::ffi::OsString;
use std::sync::{Arc, OnceLock};

use super::*;

static DEFAULT_PROVIDER_WORKER_BUDGET: OnceLock<Result<Arc<ProviderWorkerBudget>, ProtocolError>> =
    OnceLock::new();

pub fn default_provider_worker_budget() -> Result<Arc<ProviderWorkerBudget>, ProtocolError> {
    let budget = DEFAULT_PROVIDER_WORKER_BUDGET.get_or_init(|| {
        let available = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        configured_provider_worker_limits(std::env::var_os(CORE_PREPARATION_WORKERS_ENV), available)
            .map(ProviderWorkerBudget::new)
            .map(Arc::new)
    });
    budget.as_ref().map(Arc::clone).map_err(Clone::clone)
}

pub(crate) fn configured_provider_worker_limits(
    value: Option<OsString>,
    available_parallelism: usize,
) -> Result<ProviderWorkerLimits, ProtocolError> {
    let requested = match value {
        None => 1,
        Some(value) => {
            let value = value.into_string().map_err(|_| {
                ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "CTX_CORE_PREPARATION_WORKERS must be valid UTF-8",
                )
            })?;
            if value == "0" {
                1
            } else {
                if value.is_empty()
                    || value.starts_with('0')
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(invalid_worker_budget());
                }
                value
                    .parse::<usize>()
                    .map_err(|_| invalid_worker_budget())?
            }
        }
    };
    let preparation_workers = requested
        .min(available_parallelism.max(1))
        .min(MAX_CONFIGURED_CORE_PREPARATION_WORKERS);
    Ok(ProviderWorkerLimits {
        preparation_workers,
        // The public launcher maps total 1/2/4/8/16/32 to helper
        // preparation 1/1/2/4/8/16. That canonical helper value therefore
        // carries the exact private control headroom 0/0/1/2/4/8. Derive
        // finish capacity only after the helper value is clamped to this
        // process's effective CPU capacity.
        finish_workers: (preparation_workers / 2).min(MAX_PUBLICATION_WORKERS),
    })
}

fn invalid_worker_budget() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::InvalidRequest,
        "CTX_CORE_PREPARATION_WORKERS must be canonical unsigned decimal",
    )
}

impl ProviderWorkerBudget {
    pub(crate) fn new(limits: ProviderWorkerLimits) -> Self {
        Self {
            limits,
            activity: Mutex::new(WorkerActivity::default()),
            activity_changed: Condvar::new(),
        }
    }

    pub(crate) fn isolated(preparation_workers: usize) -> Arc<Self> {
        Arc::new(Self::new(ProviderWorkerLimits {
            preparation_workers,
            finish_workers: (preparation_workers / 2).min(MAX_PUBLICATION_WORKERS),
        }))
    }

    pub const fn preparation_workers(&self) -> usize {
        self.limits.preparation_workers
    }

    pub const fn finish_workers(&self) -> usize {
        self.limits.finish_workers
    }
}
