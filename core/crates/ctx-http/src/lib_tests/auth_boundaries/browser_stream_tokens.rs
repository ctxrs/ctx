use super::*;
use ctx_http_auth::{
    derive_browser_query_secret, derive_browser_stream_token, BrowserStreamAuthScope,
};

mod dictation;
mod execution_launch;
mod provider_install;
mod workspace_active;

const STREAM_TOKEN_TTL_SECS: i64 = 5 * 60;
const STREAM_TOKEN_MAX_PAST_SKEW_SECS: i64 = 10 * 60;
const STREAM_TOKEN_MAX_FUTURE_SKEW_SECS: i64 = 10 * 60;
