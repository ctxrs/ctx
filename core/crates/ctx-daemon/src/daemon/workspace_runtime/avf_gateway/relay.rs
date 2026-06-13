pub(super) fn spawn_avf_daemon_gateway_proxy(
    listener: tokio::net::TcpListener,
    gateway_addr: String,
    backend_addr: String,
) -> tokio::task::JoinHandle<()> {
    let gateway_addr_for_task = gateway_addr.clone();
    let backend_addr_for_task = backend_addr.clone();
    tokio::spawn(async move {
        loop {
            let (mut inbound, peer_addr) = match listener.accept().await {
                Ok(parts) => parts,
                Err(err) => {
                    tracing::warn!(
                        gateway_addr = gateway_addr_for_task,
                        backend_addr = backend_addr_for_task,
                        "AVF daemon gateway proxy accept failed: {err}"
                    );
                    break;
                }
            };
            let backend_addr = backend_addr_for_task.clone();
            let gateway_addr = gateway_addr_for_task.clone();
            tokio::spawn(async move {
                match tokio::net::TcpStream::connect(&backend_addr).await {
                    Ok(mut outbound) => {
                        if let Err(err) =
                            tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await
                        {
                            tracing::debug!(
                                gateway_addr,
                                backend_addr,
                                %peer_addr,
                                "AVF daemon gateway proxy relay closed with error: {err}"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            gateway_addr,
                            backend_addr,
                            %peer_addr,
                            "AVF daemon gateway proxy could not connect to backend: {err}"
                        );
                    }
                }
            });
        }
    })
}
