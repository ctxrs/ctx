use std::net::SocketAddr;

#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::net::{IpAddr, Ipv4Addr};
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::time::{timeout, Duration};

const DEFAULT_LISTEN: &str = "127.0.0.1:15001";
const DEFAULT_MAX_PEEK_BYTES: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
// Keep this list in sync with ctx's policy allowlist.
const LLM_ALLOWLIST: &[&str] = &[
    "api.anthropic.com",
    "chatgpt.com",
    "auth.openai.com",
    "api.openai.com",
    "api.mistral.ai",
    "api.groq.com",
    "api.cohere.ai",
    "api.together.xyz",
    "api.openrouter.ai",
    "openrouter.ai",
    "generativelanguage.googleapis.com",
    "vertex.googleapis.com",
    "dashscope.aliyuncs.com",
    "api.deepseek.com",
];

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProxyMode {
    LlmOnly,
    Allowlist,
    All,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    listen: Option<String>,
    mode: Option<ProxyMode>,
    allowlist: Option<Vec<String>>,
    max_peek_bytes: Option<usize>,
    bypass_uid: Option<u32>,
}

#[derive(Debug, Parser)]
#[command(name = "ctx-egress-proxy")]
struct Args {
    #[arg(long)]
    config: Option<String>,
    #[arg(long)]
    listen: Option<String>,
    #[arg(long)]
    mode: Option<ProxyMode>,
    #[arg(long, value_delimiter = ',')]
    allowlist: Vec<String>,
    #[arg(long)]
    max_peek_bytes: Option<usize>,
    #[arg(long)]
    bypass_uid: Option<u32>,
}

#[derive(Debug, Clone)]
struct ProxyConfig {
    listen: String,
    mode: ProxyMode,
    allowlist: Vec<String>,
    max_peek_bytes: usize,
    bypass_uid: u32,
}

impl ProxyConfig {
    fn from_args(args: Args, file_config: Option<FileConfig>) -> Result<Self> {
        let mut listen = DEFAULT_LISTEN.to_string();
        let mut mode = ProxyMode::LlmOnly;
        let mut allowlist: Vec<String> = Vec::new();
        let mut max_peek_bytes = DEFAULT_MAX_PEEK_BYTES;
        let mut bypass_uid = None;

        if let Some(cfg) = file_config {
            if let Some(value) = cfg.listen {
                listen = value;
            }
            if let Some(value) = cfg.mode {
                mode = value;
            }
            if let Some(value) = cfg.allowlist {
                allowlist = value;
            }
            if let Some(value) = cfg.max_peek_bytes {
                max_peek_bytes = value;
            }
            if let Some(value) = cfg.bypass_uid {
                bypass_uid = Some(value);
            }
        }

        if let Some(value) = args.listen {
            listen = value;
        }
        if let Some(value) = args.mode {
            mode = value;
        }
        if !args.allowlist.is_empty() {
            allowlist = args.allowlist;
        }
        if let Some(value) = args.max_peek_bytes {
            max_peek_bytes = value;
        }
        if let Some(value) = args.bypass_uid {
            bypass_uid = Some(value);
        }
        let Some(bypass_uid) = bypass_uid else {
            anyhow::bail!("bypass_uid is required");
        };
        if bypass_uid == 0 {
            anyhow::bail!("bypass_uid must be non-zero");
        }

        Ok(Self {
            listen,
            mode,
            allowlist,
            max_peek_bytes,
            bypass_uid,
        })
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let file_config = match args.config.as_deref() {
        Some(path) => {
            let raw =
                std::fs::read_to_string(path).with_context(|| format!("reading config {path}"))?;
            Some(serde_json::from_str::<FileConfig>(&raw).context("parsing config")?)
        }
        None => None,
    };
    let config = ProxyConfig::from_args(args, file_config)?;
    drop_to_bypass_uid(config.bypass_uid)?;
    tracing_subscriber::fmt::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building transparent proxy runtime")?;
    runtime.block_on(run_proxy(config))
}

async fn run_proxy(config: ProxyConfig) -> Result<()> {
    let listener = TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("binding transparent proxy at {}", config.listen))?;
    let (effective_uid, effective_gid) = effective_identity();
    tracing::info!(
        bypass_uid = config.bypass_uid,
        effective_uid,
        effective_gid,
        "transparent proxy running with owner bypass uid"
    );
    tracing::info!(listen = %config.listen, mode = ?config.mode, "transparent proxy listening");

    loop {
        let (stream, addr) = listener.accept().await?;
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_stream(stream, config).await {
                tracing::warn!(client = %addr, "transparent proxy error: {err:#}");
            }
        });
    }
}

async fn handle_stream(mut stream: TcpStream, config: ProxyConfig) -> Result<()> {
    let original = original_dst(&stream).context("reading original dst")?;
    let mut buf = vec![0u8; config.max_peek_bytes];
    let n = timeout(READ_TIMEOUT, stream.read(&mut buf)).await??;
    if n == 0 {
        return Ok(());
    }
    let host = sniff_host(&buf[..n]).unwrap_or_default();
    if host.is_empty() {
        anyhow::bail!("missing hostname in initial bytes");
    }
    if !allowed_host(&host, config.mode, &config.allowlist) {
        anyhow::bail!("host {host} blocked by allowlist");
    }

    let mut upstream = connect_upstream(original)
        .await
        .with_context(|| format!("connecting to upstream {original}"))?;
    upstream.write_all(&buf[..n]).await?;

    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

async fn connect_upstream(addr: SocketAddr) -> Result<TcpStream> {
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    }
    .context("creating upstream socket")?;
    socket
        .connect(addr)
        .await
        .context("connecting upstream socket")
}

#[cfg(target_os = "linux")]
fn drop_to_bypass_uid(uid: u32) -> Result<()> {
    let gid = uid as libc::gid_t;
    let uid = uid as libc::uid_t;
    let clear_groups = unsafe { libc::setgroups(0, std::ptr::null::<libc::gid_t>()) };
    if clear_groups != 0 {
        return Err(io::Error::last_os_error()).context("setgroups");
    }
    let set_gid = unsafe { libc::setgid(gid) };
    if set_gid != 0 {
        return Err(io::Error::last_os_error()).context("setgid");
    }
    let set_uid = unsafe { libc::setuid(uid) };
    if set_uid != 0 {
        return Err(io::Error::last_os_error()).context("setuid");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn drop_to_bypass_uid(_uid: u32) -> Result<()> {
    anyhow::bail!("bypass_uid is only supported on linux")
}

#[cfg(target_os = "linux")]
fn effective_identity() -> (u32, u32) {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    (uid, gid)
}

#[cfg(not(target_os = "linux"))]
fn effective_identity() -> (u32, u32) {
    (0, 0)
}

fn sniff_host(buf: &[u8]) -> Option<String> {
    if let Some(host) = parse_http_host(buf) {
        return Some(host);
    }
    parse_tls_sni(buf)
}

fn normalize_allowlist_entry(entry: &str) -> Option<String> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(trimmed) {
        if let Some(host) = url.host_str() {
            return Some(host.to_ascii_lowercase());
        }
    }
    let host = trimmed
        .split('/')
        .next()
        .unwrap_or(trimmed)
        .split(':')
        .next()
        .unwrap_or(trimmed)
        .trim();
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn host_matches(host: &str, entry: &str) -> bool {
    host == entry || host.ends_with(&format!(".{entry}"))
}

fn allowed_host(host: &str, mode: ProxyMode, allowlist: &[String]) -> bool {
    if matches!(mode, ProxyMode::All) {
        return true;
    }
    let host = host.to_ascii_lowercase();
    let mut entries = Vec::new();
    if matches!(mode, ProxyMode::LlmOnly) {
        entries.extend(
            LLM_ALLOWLIST
                .iter()
                .filter_map(|entry| normalize_allowlist_entry(entry)),
        );
    }
    if matches!(mode, ProxyMode::Allowlist) {
        entries.extend(
            allowlist
                .iter()
                .filter_map(|entry| normalize_allowlist_entry(entry)),
        );
    }
    entries.iter().any(|entry| host_matches(&host, entry))
}

fn parse_http_host(buf: &[u8]) -> Option<String> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    let status = req.parse(buf).ok()?;
    if !status.is_complete() {
        return None;
    }
    let method = req.method.unwrap_or("");
    if method.eq_ignore_ascii_case("CONNECT") {
        let path = req.path.unwrap_or("");
        return Some(strip_host_port(path));
    }
    for header in req.headers.iter() {
        if header.name.eq_ignore_ascii_case("host") {
            let value = String::from_utf8_lossy(header.value);
            return Some(strip_host_port(value.trim()));
        }
    }
    None
}

fn strip_host_port(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.find(']') {
            return trimmed[..=end].to_string();
        }
    }
    trimmed.split(':').next().unwrap_or(trimmed).to_string()
}

fn parse_tls_sni(buf: &[u8]) -> Option<String> {
    if buf.len() < 5 {
        return None;
    }
    if buf[0] != 22 {
        return None;
    }
    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + record_len {
        return None;
    }
    let mut pos = 5;
    if pos + 4 > buf.len() {
        return None;
    }
    let handshake_type = buf[pos];
    pos += 1;
    if handshake_type != 1 {
        return None;
    }
    let handshake_len =
        ((buf[pos] as usize) << 16) | ((buf[pos + 1] as usize) << 8) | buf[pos + 2] as usize;
    pos += 3;
    if pos + handshake_len > buf.len() {
        return None;
    }
    if pos + 34 > buf.len() {
        return None;
    }
    pos += 2;
    pos += 32;
    if pos + 1 > buf.len() {
        return None;
    }
    let session_id_len = buf[pos] as usize;
    pos += 1 + session_id_len;
    if pos + 2 > buf.len() {
        return None;
    }
    let cipher_suites_len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
    pos += 2 + cipher_suites_len;
    if pos + 1 > buf.len() {
        return None;
    }
    let compression_methods_len = buf[pos] as usize;
    pos += 1 + compression_methods_len;
    if pos + 2 > buf.len() {
        return None;
    }
    let extensions_len = u16::from_be_bytes([buf[pos], buf[pos + 1]]) as usize;
    pos += 2;
    let end = pos + extensions_len;
    if end > buf.len() {
        return None;
    }
    while pos + 4 <= end {
        let ext_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let ext_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + ext_len > end {
            return None;
        }
        if ext_type == 0 {
            return parse_sni_extension(&buf[pos..pos + ext_len]);
        }
        pos += ext_len;
    }
    None
}

fn parse_sni_extension(buf: &[u8]) -> Option<String> {
    if buf.len() < 2 {
        return None;
    }
    let list_len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if buf.len() < 2 + list_len {
        return None;
    }
    let mut pos = 2;
    while pos + 3 <= 2 + list_len {
        let name_type = buf[pos];
        let name_len = u16::from_be_bytes([buf[pos + 1], buf[pos + 2]]) as usize;
        pos += 3;
        if pos + name_len > buf.len() {
            return None;
        }
        if name_type == 0 {
            return std::str::from_utf8(&buf[pos..pos + name_len])
                .ok()
                .map(strip_host_port);
        }
        pos += name_len;
    }
    None
}

#[cfg(target_os = "linux")]
fn original_dst(stream: &TcpStream) -> Result<SocketAddr> {
    let fd = stream.as_raw_fd();
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_IP,
            libc::SO_ORIGINAL_DST,
            &mut addr as *mut _ as *mut _,
            &mut len as *mut _,
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error()).context("getsockopt(SO_ORIGINAL_DST)");
    }
    let ip = IpAddr::V4(Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)));
    let port = u16::from_be(addr.sin_port);
    Ok(SocketAddr::new(ip, port))
}

#[cfg(not(target_os = "linux"))]
fn original_dst(_stream: &TcpStream) -> Result<SocketAddr> {
    anyhow::bail!("SO_ORIGINAL_DST is only supported on linux")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_args() -> Args {
        Args {
            config: None,
            listen: None,
            mode: None,
            allowlist: Vec::new(),
            max_peek_bytes: None,
            bypass_uid: None,
        }
    }

    #[test]
    fn llm_only_allows_openrouter_root_host() {
        assert!(allowed_host("openrouter.ai", ProxyMode::LlmOnly, &[]));
    }

    #[test]
    fn proxy_config_accepts_dedicated_bypass_uid() {
        let cfg = FileConfig {
            listen: None,
            mode: None,
            allowlist: None,
            max_peek_bytes: None,
            bypass_uid: Some(43_558),
        };
        let config = ProxyConfig::from_args(empty_args(), Some(cfg)).unwrap();
        assert_eq!(config.bypass_uid, 43_558);
    }

    #[test]
    fn proxy_config_requires_bypass_uid() {
        let err = ProxyConfig::from_args(empty_args(), None).unwrap_err();
        assert!(err.to_string().contains("bypass_uid is required"));
    }

    #[test]
    fn proxy_config_rejects_root_bypass_uid() {
        let args = Args {
            bypass_uid: Some(0),
            ..empty_args()
        };
        let err = ProxyConfig::from_args(args, None).unwrap_err();
        assert!(err.to_string().contains("bypass_uid must be non-zero"));
    }

    #[test]
    fn llm_only_allows_openrouter_api_subdomain() {
        assert!(allowed_host("api.openrouter.ai", ProxyMode::LlmOnly, &[]));
    }

    #[test]
    fn llm_only_allows_openai_auth_host() {
        assert!(allowed_host("auth.openai.com", ProxyMode::LlmOnly, &[]));
    }

    #[test]
    fn llm_only_blocks_unlisted_hosts() {
        assert!(!allowed_host("example.com", ProxyMode::LlmOnly, &[]));
    }
}
