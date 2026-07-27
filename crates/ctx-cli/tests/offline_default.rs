mod support;

use support::*;

use std::{
    io::{Read, Write},
    net::TcpListener,
};

fn write_network_endpoints(data_root: &Path, endpoint: &str, analytics_enabled: Option<bool>) {
    fs::create_dir_all(data_root).unwrap();
    let enabled = analytics_enabled
        .map(|enabled| format!("enabled = {enabled}\n"))
        .unwrap_or_default();
    fs::write(
        data_root.join("config.toml"),
        format!(
            "[analytics]\n{enabled}endpoint = \"{endpoint}\"\n\
             [upgrade]\nfunctions_base = \"{endpoint}\"\n"
        ),
    )
    .unwrap();
}

fn local_command(temp: &TempDir, data_root: &Path) -> Command {
    let mut command = ctx(temp);
    command
        .env("CTX_DATA_ROOT", data_root)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env_remove("CTX_ANALYTICS_ENDPOINT")
        .env_remove("CTX_UPGRADE_AUTO")
        // Stale variables from the deleted history-upload prototype must be
        // inert and must not make a local command network-capable.
        .env("CTX_CLOUD_MODE", "local_and_cloud")
        .env("CTX_CLOUD_TOKEN", "stale-token")
        .env("CTX_CLOUD_API_BASE", "http://127.0.0.1:9");
    command
}

#[test]
fn local_import_status_and_daemon_are_network_inert_when_analytics_are_disabled() {
    let temp = tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let fixture_source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/codex-history.jsonl");
    let fixture = temp.path().join("codex-history.jsonl");
    fs::copy(fixture_source, &fixture).unwrap();

    let import_root = temp.path().join("import");
    write_network_endpoints(&import_root, &endpoint, Some(false));
    local_command(&temp, &import_root)
        .env("CTX_CLOUD_API_BASE", &endpoint)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            fixture.to_str().unwrap(),
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success();
    assert!(
        listener.accept().is_err(),
        "ctx import attempted a connection"
    );

    let pro_root = temp.path().join("pro");
    write_network_endpoints(&pro_root, &endpoint, Some(false));
    local_command(&temp, &pro_root)
        .env("CTX_CLOUD_API_BASE", &endpoint)
        .args(["status", "--format=json"])
        .assert()
        .success();
    assert!(
        listener.accept().is_err(),
        "ctx status Pro inspection attempted a connection"
    );

    let daemon_root = temp.path().join("daemon");
    write_network_endpoints(&daemon_root, &endpoint, Some(false));
    local_command(&temp, &daemon_root)
        .env("CTX_CLOUD_API_BASE", &endpoint)
        .args(["daemon", "disable", "--format=json"])
        .assert()
        .success();
    assert!(
        listener.accept().is_err(),
        "ctx daemon attempted a connection"
    );
}

#[test]
fn explicit_analytics_opt_in_connects_to_configured_endpoint() {
    let temp = tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let data_root = temp.path().join("opted-in");
    write_network_endpoints(&data_root, &endpoint, Some(true));

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0_u8; 4096];
        let size = stream.read(&mut request).unwrap();
        assert!(size > 0, "analytics connection sent no request");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });

    local_command(&temp, &data_root)
        .args(["doctor", "--format=json"])
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();
    server.join().unwrap();
}
