use super::*;
use hickory_resolver::{
    config::{ConnectionConfig, LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig},
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
    let mut builder = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
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
