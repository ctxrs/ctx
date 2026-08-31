use std::{
    fmt, io,
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
};

use anyhow::{anyhow, Result};

use super::{CONNECT_TIMEOUT, DNS_RESOLVE_TIMEOUT, EXECUTION_BUDGET};

pub(super) const RESOLVER_THREADS: usize = 2;
pub(super) const RESOLVER_QUEUE_CAPACITY: usize = 16;
const MAX_RESOLVED_ADDRESSES: usize = 16;

pub(super) fn build_http_agent(
    root_certs: ureq_semantic::tls::RootCerts,
) -> Result<ureq_semantic::Agent> {
    let config = ureq_semantic::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .tls_config(
            ureq_semantic::tls::TlsConfig::builder()
                .root_certs(root_certs)
                .build(),
        )
        .timeout_global(Some(EXECUTION_BUDGET))
        .timeout_resolve(Some(DNS_RESOLVE_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .build();
    let resolver = BoundedResolver::shared()?;
    Ok(ureq_semantic::Agent::with_parts(
        config,
        ureq_semantic::unversioned::transport::DefaultConnector::default(),
        resolver,
    ))
}

pub(super) type ResolverLookup =
    dyn Fn(String, ureq_semantic::config::IpFamily) -> io::Result<Vec<SocketAddr>> + Send + Sync;

static SHARED_RESOLVER_RUNTIME: OnceLock<std::result::Result<ResolverRuntime, String>> =
    OnceLock::new();

struct ResolveJob {
    address: String,
    ip_family: ureq_semantic::config::IpFamily,
    cancelled: Arc<AtomicBool>,
    result: SyncSender<io::Result<Vec<SocketAddr>>>,
}

pub(super) struct ResolverRuntime {
    sender: Option<SyncSender<ResolveJob>>,
    workers: Vec<JoinHandle<()>>,
}

impl ResolverRuntime {
    pub(super) fn spawn(lookup: Arc<ResolverLookup>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(RESOLVER_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(RESOLVER_THREADS);
        for index in 0..RESOLVER_THREADS {
            let receiver = Arc::clone(&receiver);
            let lookup = Arc::clone(&lookup);
            match thread::Builder::new()
                .name(format!("ctx-semantic-resolver-{index}"))
                .spawn(move || resolver_worker(receiver, lookup))
            {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    drop(sender);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            sender: Some(sender),
            workers,
        })
    }

    pub(super) fn resolver(&self) -> Result<BoundedResolver> {
        Ok(BoundedResolver {
            sender: self
                .sender
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedding resolver is unavailable"))?
                .clone(),
        })
    }
}

impl Drop for ResolverRuntime {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn resolver_worker(receiver: Arc<Mutex<mpsc::Receiver<ResolveJob>>>, lookup: Arc<ResolverLookup>) {
    loop {
        let job = {
            let Ok(receiver) = receiver.lock() else {
                return;
            };
            receiver.recv()
        };
        let Ok(job) = job else {
            return;
        };
        if job.cancelled.load(Ordering::Acquire) {
            continue;
        }
        let result = lookup(job.address, job.ip_family);
        if !job.cancelled.load(Ordering::Acquire) {
            let _ = job.result.try_send(result);
        }
    }
}

fn system_resolve(
    address: String,
    ip_family: ureq_semantic::config::IpFamily,
) -> io::Result<Vec<SocketAddr>> {
    Ok(ip_family
        .keep_wanted(address.to_socket_addrs()?)
        .take(MAX_RESOLVED_ADDRESSES)
        .collect())
}

#[derive(Clone)]
pub(super) struct BoundedResolver {
    sender: SyncSender<ResolveJob>,
}

impl BoundedResolver {
    fn shared() -> Result<Self> {
        let runtime = SHARED_RESOLVER_RUNTIME.get_or_init(|| {
            ResolverRuntime::spawn(Arc::new(system_resolve))
                .map_err(|error| format!("semantic embedding resolver could not start: {error}"))
        });
        match runtime {
            Ok(runtime) => runtime.resolver(),
            Err(error) => Err(anyhow!(error.clone())),
        }
    }
}

impl fmt::Debug for BoundedResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedResolver")
            .field("workers", &RESOLVER_THREADS)
            .field("queue_capacity", &RESOLVER_QUEUE_CAPACITY)
            .finish()
    }
}

impl ureq_semantic::unversioned::resolver::Resolver for BoundedResolver {
    fn resolve(
        &self,
        uri: &ureq_semantic::http::Uri,
        config: &ureq_semantic::config::Config,
        timeout: ureq_semantic::unversioned::transport::NextTimeout,
    ) -> std::result::Result<
        ureq_semantic::unversioned::resolver::ResolvedSocketAddrs,
        ureq_semantic::Error,
    > {
        let scheme = uri.scheme().ok_or_else(|| {
            ureq_semantic::Error::BadUri("semantic embedding URI has no scheme".to_owned())
        })?;
        let authority = uri.authority().ok_or_else(|| {
            ureq_semantic::Error::BadUri("semantic embedding URI has no authority".to_owned())
        })?;
        let address =
            ureq_semantic::unversioned::resolver::DefaultResolver::host_and_port(scheme, authority)
                .ok_or_else(|| {
                    ureq_semantic::Error::BadUri(
                        "semantic embedding URI has no supported port".to_owned(),
                    )
                })?;
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let job = ResolveJob {
            address,
            ip_family: config.ip_family(),
            cancelled: Arc::clone(&cancelled),
            result: result_sender,
        };
        match self.sender.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(ureq_semantic::Error::Timeout(timeout.reason));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(ureq_semantic::Error::Io(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "semantic embedding resolver is unavailable",
                )));
            }
        }

        let result = match timeout.not_zero() {
            Some(wait) => match result_receiver.recv_timeout(*wait) {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    cancelled.store(true, Ordering::Release);
                    return Err(ureq_semantic::Error::Timeout(timeout.reason));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ureq_semantic::Error::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "semantic embedding resolver worker stopped",
                    )));
                }
            },
            None => result_receiver.recv().map_err(|_| {
                ureq_semantic::Error::Io(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "semantic embedding resolver worker stopped",
                ))
            })?,
        };
        let addresses = result.map_err(ureq_semantic::Error::Io)?;
        let mut resolved = self.empty();
        for address in addresses.into_iter().take(MAX_RESOLVED_ADDRESSES) {
            resolved.push(address);
        }
        if resolved.is_empty() {
            Err(ureq_semantic::Error::HostNotFound)
        } else {
            Ok(resolved)
        }
    }
}
