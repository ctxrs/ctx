#[tokio::test]
async fn ensure_avf_guest_gateway_proxy_forwards_to_loopback_backend() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind backend listener");
    let backend_addr = backend.local_addr().expect("backend local addr");
    let proxy_port = available_proxy_port();
    let proxy_addr = format!("127.0.0.1:{proxy_port}");
    let backend_addr_str = backend_addr.to_string();

    let backend_task = tokio::spawn(async move {
        let (mut socket, _) = backend.accept().await.expect("accept backend connection");
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 4];
        socket
            .read_exact(&mut buf)
            .await
            .expect("read backend bytes");
        assert_eq!(&buf, b"ping");
        socket
            .write_all(b"pong")
            .await
            .expect("write backend reply");
    });

    super::ensure_avf_guest_gateway_proxy_for_test(&proxy_addr, &backend_addr_str, proxy_port)
        .await
        .expect("start gateway proxy");

    let mut client = tokio::net::TcpStream::connect(&proxy_addr)
        .await
        .expect("connect proxy");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    client.write_all(b"ping").await.expect("write proxy bytes");
    let mut buf = [0u8; 4];
    client.read_exact(&mut buf).await.expect("read proxy bytes");
    assert_eq!(&buf, b"pong");

    backend_task.await.expect("backend task");
}

#[tokio::test]
async fn ensure_avf_guest_gateway_proxy_replaces_stale_backend_for_reused_port() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let proxy_port = available_proxy_port();
    let proxy_addr = format!("127.0.0.1:{proxy_port}");

    let backend_one = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind first backend listener");
    let backend_one_addr = backend_one.local_addr().expect("first backend local addr");
    let backend_one_task = tokio::spawn(async move {
        let (mut socket, _) = backend_one
            .accept()
            .await
            .expect("accept first backend connection");
        let mut buf = [0u8; 4];
        socket
            .read_exact(&mut buf)
            .await
            .expect("read first backend bytes");
        assert_eq!(&buf, b"ping");
        socket
            .write_all(b"one!")
            .await
            .expect("write first backend reply");
    });

    super::ensure_avf_guest_gateway_proxy_for_test(
        &proxy_addr,
        &backend_one_addr.to_string(),
        proxy_port,
    )
    .await
    .expect("start first gateway proxy");

    let mut first_client = tokio::net::TcpStream::connect(&proxy_addr)
        .await
        .expect("connect first proxy");
    first_client
        .write_all(b"ping")
        .await
        .expect("write first proxy bytes");
    let mut first_buf = [0u8; 4];
    first_client
        .read_exact(&mut first_buf)
        .await
        .expect("read first proxy bytes");
    assert_eq!(&first_buf, b"one!");
    backend_one_task.await.expect("first backend task");

    let backend_two = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind second backend listener");
    let backend_two_addr = backend_two.local_addr().expect("second backend local addr");
    let backend_two_task = tokio::spawn(async move {
        let (mut socket, _) = backend_two
            .accept()
            .await
            .expect("accept second backend connection");
        let mut buf = [0u8; 4];
        socket
            .read_exact(&mut buf)
            .await
            .expect("read second backend bytes");
        assert_eq!(&buf, b"ping");
        socket
            .write_all(b"two!")
            .await
            .expect("write second backend reply");
    });

    super::ensure_avf_guest_gateway_proxy_for_test(
        &proxy_addr,
        &backend_two_addr.to_string(),
        proxy_port,
    )
    .await
    .expect("replace gateway proxy backend");

    let mut second_client = tokio::net::TcpStream::connect(&proxy_addr)
        .await
        .expect("connect second proxy");
    second_client
        .write_all(b"ping")
        .await
        .expect("write second proxy bytes");
    let mut second_buf = [0u8; 4];
    second_client
        .read_exact(&mut second_buf)
        .await
        .expect("read second proxy bytes");
    assert_eq!(&second_buf, b"two!");
    backend_two_task.await.expect("second backend task");
}

fn available_proxy_port() -> u16 {
    let temp = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy port probe");
    temp.local_addr().expect("proxy port local addr").port()
}
