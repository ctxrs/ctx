use super::*;
use hickory_resolver::TokioResolver;
use serde_json::Value;
use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Capture provenance belongs to an explicit add/refresh, not extraction config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureMetadata {
    pub contributor: Option<String>,
    pub captured_at_unix_secs: Option<u64>,
}

pub fn apply_capture_metadata(facts: &mut FileFacts, capture: &CaptureMetadata) -> Result<()> {
    if let Some(contributor) = &capture.contributor {
        ensure!(
            !contributor.trim().is_empty()
                && contributor.len() <= 512
                && !contributor.chars().any(char::is_control),
            "invalid capture contributor"
        );
    }
    let timestamp = capture.captured_at_unix_secs.map(Ok).unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
    })?;
    let root = facts
        .nodes
        .first_mut()
        .context("capture has no source node")?;
    root.metadata["captured_at_unix_secs"] = json!(timestamp);
    if let Some(contributor) = &capture.contributor {
        root.metadata["contributor"] = json!(contributor);
    }
    Ok(())
}

/// Explicit retrieval with format inference. Caller-relative identity is retained.
/// Tweet/arXiv metadata requests use public APIs; configured endpoint overrides
/// support compatible gateways and local fixtures. No ordinary local read calls this.
pub fn extract_url(url: &str, relative: &str, options: &IngestOptions) -> Result<FileFacts> {
    extract_url_with_resolver(url, relative, options, None)
}

fn extract_url_with_resolver(
    url: &str,
    relative: &str,
    options: &IngestOptions,
    resolver: Option<TokioResolver>,
) -> Result<FileFacts> {
    validate(options)?;
    validate_relative(relative)?;
    // DNS, HTTP response reads and explicit downloader execution share this
    // retrieval deadline. Subsequent decoding/semantic extraction has its own limits.
    let deadline = Instant::now() + Duration::from_secs(options.timeout_secs);
    let source = safe_url(url, false)?;
    let mut retrieved_url = None;
    let mut facts = if let Some(adapter) = options.converters.get("url") {
        url_addresses(&source, options.allow_private_urls, deadline, resolver)?;
        let mut adapter = adapter.clone();
        adapter.args = adapter
            .args
            .iter()
            .map(|arg| arg.replace("{url}", source.as_str()))
            .collect();
        let data = convert::run_bytes_until(
            &adapter,
            None,
            None,
            deadline,
            options.max_input_bytes as usize,
            &[],
        )?;
        downloaded(&data, None, &source, relative, options)?
    } else if tweet_id(&source).is_some() {
        let mut endpoint = safe_url(
            options
                .tweet_oembed_endpoint
                .as_deref()
                .unwrap_or("https://publish.twitter.com/oembed"),
            false,
        )?;
        endpoint
            .query_pairs_mut()
            .append_pair("url", source.as_str())
            .append_pair("omit_script", "true");
        let (data, _, final_url) = fetch(&endpoint, options, deadline, resolver)?;
        retrieved_url = Some(final_url);
        let value: Value = serde_json::from_slice(&data).context("invalid tweet oEmbed JSON")?;
        let html = value["html"].as_str().context("tweet oEmbed has no HTML")?;
        ensure!(
            !documents::html_text(html).trim().is_empty(),
            "tweet oEmbed has no text"
        );
        let mut facts = extract_text_as(
            relative,
            html,
            &content_fingerprint(&data, options)?,
            options,
            "html",
        )?;
        facts.nodes[0].metadata["source_type"] = json!("tweet");
        facts.nodes[0].metadata["tweet_id"] = json!(tweet_id(&source));
        for key in ["author_name", "author_url", "provider_name", "url"] {
            if let Some(value) = value[key].as_str() {
                ensure!(value.len() <= 4096, "tweet metadata exceeds limit");
                facts.nodes[0].metadata[key] = json!(value);
            }
        }
        facts
    } else if let Some(id) = arxiv_id(&source) {
        let mut endpoint = safe_url(
            options
                .arxiv_api_endpoint
                .as_deref()
                .unwrap_or("https://export.arxiv.org/api/query"),
            false,
        )?;
        endpoint.query_pairs_mut().append_pair("id_list", &id);
        let (data, _, final_url) = fetch(&endpoint, options, deadline, resolver)?;
        retrieved_url = Some(final_url);
        let metadata = arxiv_metadata(&data, &id)?;
        let body = format!(
            "# {}\n\n{}",
            metadata["title"].as_str().unwrap_or(""),
            metadata["abstract"].as_str().unwrap_or("")
        );
        let mut facts = extract_text_as(
            relative,
            &body,
            &content_fingerprint(&data, options)?,
            options,
            "md",
        )?;
        facts.nodes[0].metadata["source_type"] = json!("arxiv");
        for (key, value) in metadata
            .as_object()
            .context("arXiv metadata is not an object")?
        {
            facts.nodes[0].metadata[key] = value.clone();
        }
        facts
    } else {
        let (data, mime, final_url) = fetch(&source, options, deadline, resolver)?;
        let facts = downloaded(&data, mime.as_deref(), &final_url, relative, options)?;
        retrieved_url = Some(final_url);
        facts
    };
    facts.nodes[0].metadata["source_url"] = json!(source.as_str());
    if let Some(url) = retrieved_url {
        facts.nodes[0].metadata["retrieved_url"] = json!(url.as_str());
    }
    apply_capture_metadata(&mut facts, &CaptureMetadata::default())?;
    Ok(facts)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .context("URL retrieval timed out")
}

fn redirect_target(current: &reqwest::Url, location: &str) -> Result<reqwest::Url> {
    ensure!(
        location.len() <= 8192,
        "URL redirect Location exceeds limit"
    );
    let next = current
        .join(location)
        .map_err(|_| anyhow::anyhow!("invalid URL redirect Location"))?;
    let next = safe_url(next.as_str(), false)?;
    ensure!(
        current.scheme() != "https" || next.scheme() == "https",
        "HTTPS redirect downgrade prohibited"
    );
    Ok(next)
}

fn fetch(
    url: &reqwest::Url,
    options: &IngestOptions,
    deadline: Instant,
    resolver: Option<TokioResolver>,
) -> Result<(Vec<u8>, Option<String>, reqwest::Url)> {
    let mut current = url.clone();
    let mut visited = std::collections::HashSet::new();
    for redirects in 0..=5 {
        current.set_fragment(None);
        ensure!(
            visited.insert(current.as_str().to_owned()),
            "URL redirect loop"
        );
        let addresses = url_addresses(
            &current,
            options.allow_private_urls,
            deadline,
            resolver.clone(),
        )?;
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(current.host_str().context("URL host missing")?, &addresses)
            .build()?;
        let response = client
            .get(current.clone())
            .timeout(remaining(deadline)?)
            .send()
            .map_err(|_| anyhow::anyhow!("URL request failed"))?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            ensure!(redirects < 5, "URL redirect limit exceeded");
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .context("URL redirect has no valid Location")?;
            current = redirect_target(&current, location)?;
            continue;
        }
        ensure!(
            response.status().is_success(),
            "URL returned HTTP {}",
            response.status()
        );
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            });
        let mut data = Vec::new();
        response
            .take(options.max_input_bytes + 1)
            .read_to_end(&mut data)
            .context("cannot read URL response")?;
        remaining(deadline)?;
        ensure!(
            data.len() as u64 <= options.max_input_bytes,
            "URL response exceeds byte limit"
        );
        return Ok((data, mime, current));
    }
    unreachable!("redirect loop returns at its fixed limit")
}

fn downloaded(
    data: &[u8],
    mime: Option<&str>,
    url: &reqwest::Url,
    relative: &str,
    options: &IngestOptions,
) -> Result<FileFacts> {
    let format = infer_format(data, mime, url, relative)?;
    let temp = tempfile::Builder::new()
        .suffix(&format!(".{format}"))
        .tempfile()?;
    std::fs::write(temp.path(), data)?;
    let mut facts = extract(
        temp.path(),
        relative,
        &content_fingerprint(data, options)?,
        options,
    )?;
    facts.nodes[0].metadata["format"] = json!(format);
    if let Some(mime) = mime {
        facts.nodes[0].metadata["content_type"] = json!(mime);
    }
    Ok(facts)
}

fn infer_format(
    data: &[u8],
    mime: Option<&str>,
    url: &reqwest::Url,
    relative: &str,
) -> Result<String> {
    if data.starts_with(b"%PDF-") {
        return Ok("pdf".into());
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("png".into());
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Ok("jpg".into());
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Ok("gif".into());
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        return Ok("webp".into());
    }
    if data.starts_with(b"PK\x03\x04") {
        let zip =
            zip::ZipArchive::new(std::io::Cursor::new(data)).context("invalid downloaded ZIP")?;
        ensure!(zip.len() <= 4096, "downloaded archive entry limit exceeded");
        if zip.file_names().any(|name| name == "word/document.xml") {
            return Ok("docx".into());
        }
        if zip
            .file_names()
            .any(|name| name.starts_with("xl/worksheets/"))
        {
            return Ok("xlsx".into());
        }
    }
    let format = match mime.unwrap_or("") {
        "application/pdf" => Some("pdf"),
        "text/html" | "application/xhtml+xml" => Some("html"),
        "text/markdown" => Some("md"),
        "text/plain" => Some("txt"),
        "application/yaml" | "text/yaml" => Some("yaml"),
        "image/svg+xml" => Some("svg"),
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "audio/mpeg" => Some("mp3"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/ogg" => Some("ogg"),
        "audio/flac" => Some("flac"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        _ => None,
    };
    if let Some(format) = format {
        return Ok(format.into());
    }
    let prefix = String::from_utf8_lossy(&data[..data.len().min(256)])
        .trim_start()
        .to_ascii_lowercase();
    if ["<!doctype html", "<html", "<head", "<body"]
        .iter()
        .any(|tag| prefix.starts_with(tag))
    {
        return Ok("html".into());
    }
    for path in [url.path(), relative] {
        if supports(Path::new(path)) {
            return Ok(extension(Path::new(path)));
        }
    }
    Ok("txt".into())
}

fn tweet_id(url: &reqwest::Url) -> Option<&str> {
    if !matches!(
        url.host_str(),
        Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
    ) {
        return None;
    }
    let parts: Vec<_> = url.path_segments()?.collect();
    let id = *parts.get(2)?;
    (parts.get(1) == Some(&"status") && !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        .then_some(id)
}

fn arxiv_id(url: &reqwest::Url) -> Option<String> {
    if !matches!(
        url.host_str(),
        Some("arxiv.org" | "www.arxiv.org" | "export.arxiv.org")
    ) {
        return None;
    }
    let id = url.path().strip_prefix("/abs/")?;
    (!id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '-')))
    .then(|| id.to_owned())
}

fn arxiv_metadata(data: &[u8], expected: &str) -> Result<Value> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_reader(data);
    let mut depth = 0usize;
    let mut entry = None;
    let mut entries = 0usize;
    let mut capture: Option<(String, usize, String)> = None;
    let mut fields = std::collections::BTreeMap::new();
    let mut authors = vec![];
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                depth += 1;
                ensure!(depth <= 128, "arXiv XML nesting exceeds limit");
                if e.local_name().as_ref() == b"entry" {
                    entries += 1;
                    ensure!(
                        entries == 1,
                        "arXiv response must contain exactly one entry"
                    );
                    entry = Some(depth);
                }
                if entry.is_some()
                    && matches!(
                        e.local_name().as_ref(),
                        b"id" | b"title" | b"summary" | b"published" | b"updated" | b"name"
                    )
                {
                    capture = Some((
                        String::from_utf8(e.local_name().as_ref().to_vec())?,
                        depth,
                        String::new(),
                    ));
                }
            }
            Event::Text(e) => {
                if let Some((_, _, text)) = &mut capture {
                    text.push_str(&e.decode()?);
                }
            }
            Event::CData(e) => {
                if let Some((_, _, text)) = &mut capture {
                    text.push_str(&e.decode()?);
                }
            }
            Event::GeneralRef(e) => {
                if let Some((_, _, text)) = &mut capture {
                    text.push_str(&quick_xml::escape::unescape(&format!("&{};", e.decode()?))?);
                }
            }
            Event::End(_) => {
                if capture.as_ref().is_some_and(|(_, d, _)| *d == depth) {
                    let (name, _, text) = capture.take().unwrap();
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if name == "name" {
                        authors.push(text);
                        ensure!(authors.len() <= 256, "arXiv author limit exceeded");
                    } else {
                        fields.insert(name, text);
                    }
                }
                if entry == Some(depth) {
                    entry = None;
                }
                depth = depth.saturating_sub(1);
            }
            Event::DocType(_) => anyhow::bail!("arXiv XML DTDs are prohibited"),
            Event::Empty(e) if e.local_name().as_ref() == b"entry" => {
                anyhow::bail!("arXiv response contains an empty entry")
            }
            Event::Eof => {
                ensure!(depth == 0, "incomplete arXiv XML");
                break;
            }
            _ => {}
        }
    }
    ensure!(
        entries == 1,
        "arXiv response must contain exactly one entry"
    );
    let id = fields.get("id").context("arXiv response has no entry ID")?;
    let returned = arxiv_id(&safe_url(id, false)?).context("invalid arXiv response ID")?;
    // API may supply a version for an unversioned requested identifier.
    ensure!(
        returned == expected
            || returned.strip_prefix(expected).is_some_and(|s| s
                .strip_prefix('v')
                .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))),
        "arXiv response ID does not match request"
    );
    let title = fields
        .get("title")
        .filter(|s| !s.is_empty())
        .context("arXiv response has no title")?;
    let summary = fields
        .get("summary")
        .filter(|s| !s.is_empty())
        .context("arXiv response has no abstract")?;
    Ok(
        json!({"arxiv_id":returned,"title":title,"abstract":summary,"authors":authors,"published":fields.get("published"),"updated":fields.get("updated")}),
    )
}

fn url_addresses(
    url: &reqwest::Url,
    allow_private: bool,
    deadline: Instant,
    resolver: Option<TokioResolver>,
) -> Result<Vec<SocketAddr>> {
    fn allowed(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => {
                let [a, b, c, _] = ip.octets();
                !ip.is_private()
                    && !ip.is_loopback()
                    && !ip.is_link_local()
                    && !ip.is_broadcast()
                    && !ip.is_documentation()
                    && !ip.is_unspecified()
                    && !ip.is_multicast()
                    && a != 0
                    && a < 240
                    && !(a == 100 && (64..=127).contains(&b))
                    && !(a == 198 && (b == 18 || b == 19))
                    && !(a == 192 && b == 0 && c == 0)
            }
            IpAddr::V6(ip) => ip
                .to_ipv4_mapped()
                .map(|v| allowed(IpAddr::V4(v)))
                .unwrap_or_else(|| {
                    !ip.is_loopback()
                        && !ip.is_unspecified()
                        && !ip.is_multicast()
                        && (ip.segments()[0] & 0xfe00) != 0xfc00
                        && (ip.segments()[0] & 0xffc0) != 0xfe80
                        && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0xdb8)
                }),
        }
    }
    remaining(deadline)?;
    let host = url
        .host_str()
        .context("URL host missing")?
        .trim_matches(['[', ']']);
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<_> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        // A scoped async resolver avoids uncancellable getaddrinfo threads.
        // Read system DNS/hosts configuration without modifying it. Dropping
        // this runtime cancels the resolver's I/O tasks, including on timeout.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let resolver = match resolver {
                Some(resolver) => resolver,
                None => TokioResolver::builder_tokio()
                    .map_err(|_| anyhow::anyhow!("cannot read DNS configuration"))?
                    .build()
                    .map_err(|_| anyhow::anyhow!("cannot initialize DNS resolver"))?,
            };
            remaining(deadline)?;
            let response = tokio::time::timeout_at(deadline.into(), resolver.lookup_ip(host))
                .await
                .map_err(|_| anyhow::anyhow!("URL DNS resolution timed out"))?
                .map_err(|_| anyhow::anyhow!("cannot resolve URL host"))?;
            Ok::<_, anyhow::Error>(
                response
                    .iter()
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect(),
            )
        })?
    };
    remaining(deadline)?;
    ensure!(!addresses.is_empty(), "URL host has no addresses");
    ensure!(
        allow_private || addresses.iter().all(|a| allowed(a.ip())),
        "URL resolves to private/reserved addresses; set allow_private_urls only for an explicitly trusted local source"
    );
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::{
        config::{
            ConnectionConfig, LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig,
        },
        net::runtime::TokioRuntimeProvider,
        proto::{
            op::Message,
            rr::{RData, Record, rdata::A},
        },
    };
    use std::{
        io::Write,
        net::{Ipv4Addr, TcpListener, UdpSocket},
        thread,
    };

    fn resolver_at(address: SocketAddr) -> TokioResolver {
        // Fixtures use .test: Hickory returns NXDOMAIN for .invalid locally,
        // before contacting even an explicitly configured mock name server.
        let mut connection = ConnectionConfig::udp();
        connection.port = address.port();
        let config = ResolverConfig::from_parts(
            None,
            vec![],
            vec![NameServerConfig::new(address.ip(), true, vec![connection])],
        );
        let mut builder =
            TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
        builder.options_mut().use_hosts_file = ResolveHosts::Never;
        builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4Only;
        builder.options_mut().attempts = 1;
        builder.options_mut().timeout = Duration::from_secs(5);
        builder.build().unwrap()
    }

    #[test]
    fn redirect_targets_keep_scheme_credentials_and_private_address_checks() {
        let source = safe_url("https://public.example/start", false).unwrap();
        assert_eq!(
            redirect_target(&source, "/next").unwrap().as_str(),
            "https://public.example/next"
        );
        let credential_url = ["https://user:SYNTHETIC_SECRET", "@public.example/"].concat();
        for location in [
            "http://public.example/next",
            "file:///tmp/source",
            credential_url.as_str(),
        ] {
            let error = redirect_target(&source, location).unwrap_err();
            assert!(!format!("{error:#}").contains("SYNTHETIC_SECRET"));
        }
        let next = redirect_target(&source, "https://127.0.0.1/private").unwrap();
        assert!(
            url_addresses(&next, false, Instant::now() + Duration::from_secs(1), None)
                .unwrap_err()
                .to_string()
                .contains("private/reserved")
        );
        assert_eq!(
            url_addresses(&next, true, Instant::now() + Duration::from_secs(1), None).unwrap(),
            ["127.0.0.1:443".parse::<SocketAddr>().unwrap()]
        );
    }

    fn mock_dns(ips: Vec<Ipv4Addr>, delay: Duration) -> (TokioResolver, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let resolver = resolver_at(socket.local_addr().unwrap());
        let task = thread::spawn(move || {
            let mut packet = [0; 4096];
            let (size, client) = socket.recv_from(&mut packet).unwrap();
            let request = Message::from_vec(&packet[..size]).unwrap();
            let query = request.queries[0].clone();
            let mut response = Message::response(request.metadata.id, request.metadata.op_code);
            response.metadata.recursion_desired = true;
            response.metadata.recursion_available = true;
            response.add_query(query.clone());
            for ip in ips {
                response.add_answer(Record::from_rdata(
                    query.name().clone(),
                    60,
                    RData::A(A(ip)),
                ));
            }
            thread::sleep(delay);
            socket.send_to(&response.to_vec().unwrap(), client).unwrap();
        });
        (resolver, task)
    }

    fn mock_http(listener: TcpListener, delay: Duration) -> thread::JoinHandle<String> {
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "mock HTTP was not reached");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = vec![];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            thread::sleep(delay);
            // A deadline test deliberately closes the connection first.
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: 8\r\nConnection: close\r\n\r\n# Pinned");
            String::from_utf8(request).unwrap()
        })
    }

    #[cfg(unix)]
    #[test]
    fn stalled_dns_times_out_before_http_or_downloader_starts() {
        let temp = tempfile::tempdir().unwrap();
        for downloader in [false, true] {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let marker = temp.path().join("downloader-started");
            let mut options = IngestOptions {
                timeout_secs: 1,
                allow_private_urls: true,
                ..IngestOptions::default()
            };
            if downloader {
                options.converters.insert(
                    "url".into(),
                    CommandAdapter {
                        program: "python3".into(),
                        args: vec![
                            "-c".into(),
                            "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('started')"
                                .into(),
                            marker.to_string_lossy().into_owned(),
                        ],
                        output_file: false,
                    },
                );
            }
            let url = format!(
                "http://dns-stall.test:{}/source",
                listener.local_addr().unwrap().port()
            );
            let start = Instant::now();
            let error = extract_url_with_resolver(
                &url,
                "source.md",
                &options,
                Some(resolver_at(socket.local_addr().unwrap())),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("DNS resolution timed out"),
                "unexpected lookup result: {error:#}"
            );
            assert!(start.elapsed() < Duration::from_secs(3));
            assert!(
                socket.recv_from(&mut [0; 4096]).is_ok(),
                "fixture must actually receive a DNS query"
            );
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            assert!(!marker.exists());
        }
    }

    #[test]
    fn dns_addresses_are_checked_and_allowed_results_are_pinned() {
        let url = safe_url("http://pinned.test:1234/source", false).unwrap();
        let public = Ipv4Addr::new(93, 184, 215, 14);
        for ips in [
            vec![public],
            vec![Ipv4Addr::LOCALHOST],
            vec![public, Ipv4Addr::LOCALHOST],
        ] {
            let is_public = ips == [public];
            let (resolver, dns) = mock_dns(ips, Duration::ZERO);
            let result = url_addresses(
                &url,
                false,
                Instant::now() + Duration::from_secs(2),
                Some(resolver),
            );
            dns.join().unwrap_or_else(|_| {
                panic!(
                    "DNS fixture failed; lookup error: {:?}",
                    result.as_ref().err()
                )
            });
            if is_public {
                assert_eq!(result.unwrap(), [SocketAddr::new(public.into(), 1234)]);
            } else {
                assert!(result.unwrap_err().to_string().contains("private/reserved"));
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let http = mock_http(listener, Duration::ZERO);
        let (resolver, dns) = mock_dns(vec![Ipv4Addr::LOCALHOST], Duration::ZERO);
        let options = IngestOptions {
            allow_private_urls: true,
            timeout_secs: 2,
            ..IngestOptions::default()
        };
        let facts = extract_url_with_resolver(
            &format!("http://pinned.test:{port}/source"),
            "source.md",
            &options,
            Some(resolver),
        )
        .unwrap();
        dns.join().unwrap();
        assert!(
            http.join()
                .unwrap()
                .to_lowercase()
                .contains(&format!("host: pinned.test:{port}"))
        );
        assert!(
            facts
                .nodes
                .iter()
                .any(|n| n.kind == "heading" && n.label == "Pinned")
        );
    }

    #[cfg(unix)]
    #[test]
    fn dns_http_and_downloader_share_the_original_retrieval_deadline() {
        for downloader in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut options = IngestOptions {
                allow_private_urls: true,
                timeout_secs: 1,
                ..IngestOptions::default()
            };
            let http = if downloader {
                options.converters.insert(
                    "url".into(),
                    CommandAdapter {
                        program: "python3".into(),
                        args: vec![
                            "-c".into(),
                            "import time; time.sleep(0.7); print('# Too late')".into(),
                        ],
                        output_file: false,
                    },
                );
                None
            } else {
                Some(mock_http(listener, Duration::from_millis(700)))
            };
            let (resolver, dns) = mock_dns(vec![Ipv4Addr::LOCALHOST], Duration::from_millis(500));
            let start = Instant::now();
            let result = extract_url_with_resolver(
                &format!("http://delayed.test:{port}/source"),
                "source.md",
                &options,
                Some(resolver),
            );
            dns.join().unwrap_or_else(|_| {
                panic!(
                    "DNS fixture failed; retrieval error: {:?}",
                    result.as_ref().err()
                )
            });
            if let Some(http) = http {
                http.join().unwrap_or_else(|_| {
                    panic!(
                        "HTTP fixture failed; retrieval error: {:?}",
                        result.as_ref().err()
                    )
                });
            }
            assert!(
                result.is_err(),
                "DNS must not grant the next stage a fresh one-second budget"
            );
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }
}
