use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use url::Url;

mod ansi;
mod complete;
mod extract;
mod trailing;

pub(in crate::daemon::providers::claude_setup_token_login) use ansi::normalize_claude_login_line;
pub(in crate::daemon::providers::claude_setup_token_login) use complete::auth_url_looks_complete;
pub(in crate::daemon::providers::claude_setup_token_login) use extract::extract_auth_url;
pub(in crate::daemon::providers::claude_setup_token_login) use trailing::read_trailing_claude_login_lines;

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    false
}
