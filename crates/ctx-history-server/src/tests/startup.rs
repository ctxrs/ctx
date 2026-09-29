use super::*;
use std::net::TcpListener;

#[test]
fn failed_bind_never_invokes_ready_callback() {
    let root = tempfile::tempdir().unwrap();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = ServerConfig::new(root.path().join("server"));
    config.bind = occupied.local_addr().unwrap();
    let server = Arc::new(HistoryServer::open(config).unwrap());
    let mut called = false;
    let result = serve_blocking_with_ready(server, |_| {
        called = true;
        Ok(())
    });
    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AddrInUse)
    );
    assert!(!called);
}

#[test]
fn ready_gets_actual_bound_address_and_failure_closes_listener() {
    let root = tempfile::tempdir().unwrap();
    let mut config = ServerConfig::new(root.path().join("server"));
    config.bind.set_port(0);
    let server = Arc::new(HistoryServer::open(config).unwrap());
    let mut address = None;
    let result = serve_blocking_with_ready(server, |bound| {
        assert!(bound.ip().is_loopback());
        assert_ne!(bound.port(), 0);
        // The supplied address belongs to an already bound socket.
        assert!(TcpListener::bind(bound).is_err());
        address = Some(bound);
        Err(Error::Invalid("operator endpoint save failed"))
    });
    assert!(matches!(
        result,
        Err(Error::Invalid("operator endpoint save failed"))
    ));
    // Callback failure did not leak a listener or start a serving task.
    let rebound = TcpListener::bind(address.unwrap()).unwrap();
    assert_eq!(Some(rebound.local_addr().unwrap()), address);
}
