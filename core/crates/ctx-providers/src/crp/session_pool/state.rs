use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::events::NormalizedEvent;

use super::super::runtime::CrpProcess;
use crate::adapters::ProviderUnknownEventHook;

#[derive(Debug, Clone, Deserialize)]
pub(in crate::crp::session_pool) struct CrpSessionStatusDetails {
    pub(in crate::crp::session_pool) quiescent: bool,
}

pub(in crate::crp::session_pool) struct SessionSnapshot {
    pub(in crate::crp::session_pool) session_key: String,
    pub(in crate::crp::session_pool) session: Arc<CrpSession>,
    pub(in crate::crp::session_pool) last_used: Instant,
    pub(in crate::crp::session_pool) draining: bool,
    pub(in crate::crp::session_pool) shutdown_reason: Option<String>,
}

pub(in crate::crp) fn session_shutdown_reason(session: &CrpSession) -> Option<String> {
    session.process.shutdown.borrow().clone()
}

pub(in crate::crp::session_pool) fn session_is_live(session: &CrpSession) -> bool {
    !session.draining.load(Ordering::SeqCst) && session_shutdown_reason(session).is_none()
}

pub(in crate::crp) struct CrpSession {
    pub(in crate::crp) process: Arc<CrpProcess>,
    pub(in crate::crp) opened: AtomicBool,
    pub(in crate::crp) opening: AtomicBool,
    pub(in crate::crp) status_supported: AtomicBool,
    pub(in crate::crp) draining: AtomicBool,
    pub(in crate::crp) launch_policy_signature: Option<String>,
    pub(in crate::crp) launch_env_signature: u64,
    last_used: StdMutex<Instant>,
}

impl CrpSession {
    pub(in crate::crp::session_pool) fn new(
        process: Arc<CrpProcess>,
        supports_session_status: bool,
        launch_policy_signature: Option<String>,
        launch_env_signature: u64,
    ) -> Self {
        Self {
            process,
            opened: AtomicBool::new(false),
            opening: AtomicBool::new(false),
            status_supported: AtomicBool::new(supports_session_status),
            draining: AtomicBool::new(false),
            launch_policy_signature,
            launch_env_signature,
            last_used: StdMutex::new(Instant::now()),
        }
    }

    pub(in crate::crp::session_pool) fn touch(&self) {
        if let Ok(mut last_used) = self.last_used.lock() {
            *last_used = Instant::now();
        }
    }

    pub(in crate::crp) fn last_used(&self) -> Instant {
        self.last_used
            .lock()
            .map(|instant| *instant)
            .unwrap_or_else(|_| Instant::now())
    }
}

pub(in crate::crp) struct CrpPromptRequest {
    pub(in crate::crp) session_key: String,
    pub(in crate::crp) input: crate::adapters::TurnInput,
    pub(in crate::crp) workdir: PathBuf,
    pub(in crate::crp) env: HashMap<String, String>,
    pub(in crate::crp) event_sink: mpsc::Sender<NormalizedEvent>,
    pub(in crate::crp) provider_unknown_event: Option<ProviderUnknownEventHook>,
    pub(in crate::crp) provider_session_ref_claim:
        Option<crate::adapters::ProviderSessionRefClaimHook>,
    pub(in crate::crp) cancel_rx: oneshot::Receiver<()>,
}
