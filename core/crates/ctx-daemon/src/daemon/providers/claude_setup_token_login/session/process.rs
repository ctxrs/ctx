use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::{mpsc, oneshot};

use super::super::auth_url::{
    auth_url_looks_complete, claude_login_hit_unsupported_manual_fallback,
    claude_manual_fallback_is_terminal, extract_claude_setup_token,
    extract_preferred_claude_auth_url, read_trailing_claude_login_lines,
    refresh_claude_auth_url_from_capture_path, should_replace_observed_claude_auth_url,
    CLAUDE_UNSUPPORTED_MANUAL_FALLBACK_ERROR,
};
use super::super::runtime::{spawn_claude_setup_token_command, ClaudeLoginSpawn};

#[path = "process/line_observation.rs"]
mod line_observation;
#[path = "process/monitor.rs"]
mod monitor;
#[path = "process/start.rs"]
mod start;

const CLAUDE_LOGIN_NO_AUTH_URL_TIMEOUT: Duration = Duration::from_secs(8);
const CLAUDE_LOGIN_URL_WAIT: Duration = Duration::from_secs(20);
const CLAUDE_LOGIN_URL_SETTLE_WAIT: Duration = Duration::from_millis(500);
const CLAUDE_LOGIN_CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CLAUDE_LOGIN_COMPLETION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CLAUDE_LOGIN_EXIT_GRACE_WAIT: Duration = Duration::from_millis(400);

pub(in crate::daemon::providers::claude_setup_token_login) use monitor::monitor_claude_login;
pub(in crate::daemon::providers::claude_setup_token_login) use start::start_claude_login_process;

pub(in crate::daemon::providers::claude_setup_token_login) struct ClaudeLoginProcess {
    line_rx: mpsc::UnboundedReceiver<String>,
    buffered_lines: Vec<String>,
    pub(in crate::daemon::providers::claude_setup_token_login) auth_url: Option<String>,
    browser_open_capture_path: PathBuf,
    exit_rx: oneshot::Receiver<anyhow::Result<portable_pty::ExitStatus>>,
    killer: Arc<StdMutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    _browser_open_shim_dir: tempfile::TempDir,
}
