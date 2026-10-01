use super::*;

fn registered(db: PathBuf, args: &ServeArgs) -> Result<Graf> {
    ensure!(
        (1..=256 * 1024 * 1024).contains(&args.snapshot_max_bytes),
        "snapshot-max-bytes must be between 1 and 268435456"
    );
    if let Some(repo) = &args.github_repo {
        prs::validate_repo(repo)?;
    }
    ensure!(
        args.project.len() < MAX_PROJECTS,
        "at most 32 databases including default may be registered"
    );
    let mut paths = BTreeMap::from([("default".to_owned(), db)]);
    for entry in &args.project {
        let (name, path) = entry.split_once('=').context("project must be NAME=DB")?;
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "project names must be 1..64 ASCII letters, digits, underscores or hyphens"
        );
        ensure!(!path.is_empty(), "project database path must not be empty");
        ensure!(
            !paths.contains_key(name),
            "duplicate or reserved project name: {name}"
        );
        paths.insert(name.to_owned(), PathBuf::from(path));
    }
    let mut projects = BTreeMap::new();
    for (name, db) in paths {
        let db = db
            .canonicalize()
            .with_context(|| format!("cannot open registered project {name}"))?;
        drop(Store::open_read_only(&db)?);
        let memory_dir = (name == "default")
            .then(|| args.memory_dir.clone())
            .flatten();
        projects.insert(
            name,
            Arc::new(Project {
                db,
                memory_dir,
                snapshot_max_bytes: args.snapshot_max_bytes,
                cache: Mutex::new(None),
            }),
        );
    }
    let mut tool_router = Graf::tool_router();
    if args.github_repo.is_none() {
        for name in ["list_prs", "get_pr_impact", "triage_prs"] {
            tool_router.remove_route(name);
        }
    }
    Ok(Graf {
        projects: Arc::new(projects),
        github_repo: args.github_repo.clone(),
        workers: Arc::new(Semaphore::new(4)),
        tool_router,
        observation: None,
    })
}

async fn authorize(
    expected: Option<blake3::Hash>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    if let Some(expected) = expected {
        let provided = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
            .map(|(_, token)| blake3::hash(token.as_bytes()));
        // blake3::Hash equality compares in constant time.
        if provided != Some(expected) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(request).await)
}

pub async fn serve_observed(
    db: PathBuf,
    args: ServeArgs,
    facts: &mut crate::GraphObservation,
    observer: Option<crate::GraphObserver>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let result = serve_inner(db, args, facts, observer).await;
    facts.duration = Some(started.elapsed());
    if let Err(error) = &result {
        if error.is::<crate::output::OutputFailure>() {
            facts.output_served = Some(false);
            facts.output_failure = Some(crate::observation::failure_kind(error));
        } else {
            crate::observation::failed(facts, error);
        }
    }
    facts.lifecycle = Some(if result.is_err() && facts.lifecycle.is_none() {
        crate::observation::GraphLifecycle::StartFailed
    } else {
        crate::observation::GraphLifecycle::Stopped
    });
    result
}

async fn serve_inner(
    db: PathBuf,
    args: ServeArgs,
    facts: &mut crate::GraphObservation,
    observer: Option<crate::GraphObserver>,
) -> Result<()> {
    let startup = std::time::Instant::now();
    facts.phase = crate::observation::GraphPhase::Registration;
    let server = registered(db, &args)?;
    if args.transport == Transport::Stdio {
        ensure!(
            args.bearer_token_env.is_none() && args.allowed_host.is_empty(),
            "HTTP authentication/host options require --transport http"
        );
        // Use the SDK codec so oversized lines terminate the transport without unbounded buffering.
        let requests = observation::StdioObservations::default();
        let input_requests = requests.clone();
        let input_observer = observer.clone();
        let observer_for_input = observer.clone();
        let input = FramedRead::new(
            tokio::io::stdin(),
            JsonRpcMessageCodec::<ClientJsonRpcMessage>::new_with_max_length(MAX_MESSAGE),
        )
        .inspect(move |result| {
            if result.is_err() {
                observation::protocol_failure(&input_observer);
            }
        })
        .take_while(|result| std::future::ready(result.is_ok()))
        .map(move |result| {
            let mut message = result.expect("codec errors terminate input");
            input_requests.input(&mut message, &observer_for_input);
            message
        });
        let output = FramedWrite::new(
            tokio::io::stdout(),
            JsonRpcMessageCodec::<ServerJsonRpcMessage>::default(),
        );
        facts.phase = crate::observation::GraphPhase::Protocol;
        let output = observation::ObservedSink::new(output, requests);
        let running = server.serve((output, input)).await?;
        facts.lifecycle = Some(crate::observation::GraphLifecycle::Ready);
        facts.duration = Some(startup.elapsed());
        crate::observation::emit(&observer, *facts);
        facts.phase = crate::observation::GraphPhase::Shutdown;
        running.waiting().await?;
        return Ok(());
    }
    ensure!(
        args.path.starts_with('/')
            && !args.path.contains(['{', '}', '*', '?', '#'])
            && args.path.len() <= 256,
        "HTTP path must be an absolute literal path without query or fragment"
    );
    ensure!(
        args.host.is_loopback() || args.bearer_token_env.is_some(),
        "nonloopback HTTP requires --bearer-token-env NAME"
    );
    let bearer = args.bearer_token_env.as_ref().map(|name| -> Result<_> {
        ensure!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "invalid bearer environment variable name");
        let token = std::env::var(name).map_err(|_| anyhow::anyhow!("bearer environment variable is missing or not Unicode"))?;
        ensure!(!token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b)),
            "bearer environment variable must contain a nonempty ASCII bearer token of at most 4096 bytes");
        Ok(blake3::hash(token.as_bytes()))
    }).transpose()?;
    let mut config = StreamableHttpServerConfig::default().enforce_origin_validation();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.max_request_body_bytes = MAX_MESSAGE;
    if !args.host.is_unspecified() {
        config.allowed_hosts.push(args.host.to_string());
    }
    for host in args.allowed_host {
        ensure!(
            !host.is_empty()
                && !host.contains(['*', '/', '@'])
                && !host.chars().any(char::is_whitespace),
            "allowed-host must be an exact HTTP authority"
        );
        config.allowed_hosts.push(host);
    }
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let http_observer = observer.clone();
    let app = axum::Router::new()
        .route_service(&args.path, service)
        .layer(axum::middleware::from_fn(move |request, next| {
            authorize(bearer, request, next)
        }))
        .layer(axum::middleware::from_fn(move |request, next| {
            observation::http(http_observer.clone(), request, next)
        }));
    facts.phase = crate::observation::GraphPhase::Bind;
    let listener = tokio::net::TcpListener::bind((args.host, args.port)).await?;
    crate::output::stderr(format_args!(
        "ctx graph MCP listening on http://{}{}",
        listener.local_addr()?,
        args.path
    ))?;
    facts.lifecycle = Some(crate::observation::GraphLifecycle::Ready);
    facts.duration = Some(startup.elapsed());
    crate::observation::emit(&observer, *facts);
    facts.phase = crate::observation::GraphPhase::Shutdown;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
