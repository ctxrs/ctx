use super::*;

#[derive(Debug)]
struct GatewayCertVerifier {
    inner: Arc<WebPkiServerVerifier>,
    server_name: ServerName<'static>,
    pinned_der: Option<Vec<u8>>,
}

impl ServerCertVerifier for GatewayCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(pinned) = &self.pinned_der {
            if end_entity.as_ref() == pinned.as_slice() {
                return Ok(ServerCertVerified::assertion());
            }
        }
        self.inner.verify_server_cert(
            end_entity,
            intermediates,
            &self.server_name,
            ocsp_response,
            now,
        )
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn gateway_ws_connector(pem: &str) -> Result<Connector> {
    let mut roots = RootCertStore::empty();
    let mut pinned_der: Option<Vec<u8>> = None;
    for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = cert.context("parsing gateway CA")?;
        if pinned_der.is_none() {
            pinned_der = Some(cert.as_ref().to_vec());
        }
        roots.add(cert).context("adding gateway CA")?;
    }
    let verifier = WebPkiServerVerifier::builder(Arc::new(roots.clone()))
        .build()
        .context("building gateway verifier")?;
    let server_name =
        ServerName::try_from("ctx-gateway").context("building gateway server name")?;
    let verifier = GatewayCertVerifier {
        inner: verifier,
        server_name,
        pinned_der,
    };
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(verifier));
    Ok(Connector::Rustls(Arc::new(config)))
}

pub(super) async fn connect_terminal_gateway(
    remote: &RemoteTerminalRequest,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let mut base = remote.gateway_url.trim_end_matches('/').to_string();
    if base.starts_with("https://") {
        base = base.replacen("https://", "wss://", 1);
    } else if base.starts_with("http://") {
        base = base.replacen("http://", "ws://", 1);
    } else if !base.starts_with("ws://") && !base.starts_with("wss://") {
        base = format!("ws://{base}");
    }
    let url = format!(
        "{base}/workers/{}/terminals/{}/daemon",
        remote.worker_id, remote.terminal_id.0
    );
    let mut req = url
        .as_str()
        .into_client_request()
        .context("building terminal relay request")?;
    if let Some(token) = remote.token.as_deref() {
        req.headers_mut().insert(
            "x-ctx-gateway-token",
            token.parse().context("parsing gateway token")?,
        );
    }
    let (ws_stream, _) = if let Some(pem) = remote.gateway_ca_pem.as_deref() {
        let connector = gateway_ws_connector(pem)?;
        connect_async_tls_with_config(req, None, false, Some(connector))
            .await
            .with_context(|| format!("connecting to gateway terminal relay at {url}"))?
    } else {
        connect_async(req)
            .await
            .with_context(|| format!("connecting to gateway terminal relay at {url}"))?
    };
    Ok(ws_stream)
}
