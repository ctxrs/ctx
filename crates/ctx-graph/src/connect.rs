//! Explicit database operations. Read commands never call these adapters.
use std::{
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use ctx_graph_core::{export, languages::configs, sources, store::Store};
use serde_json::json;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Explicitly fetch database schema facts into the local source cache.
    Introspect {
        #[command(subcommand)]
        backend: Introspect,
    },
    /// Explicitly write the stored graph to a remote database.
    Push {
        #[command(subcommand)]
        backend: Push,
    },
}

#[derive(Debug, Subcommand)]
pub enum Introspect {
    /// Requires pg_dump on PATH; index the saved facts separately afterward.
    Postgres(PostgresArgs),
}

#[derive(Debug, Subcommand)]
pub enum Push {
    /// Requires cypher-shell on PATH; writes one explicit transaction.
    Neo4j(Neo4jArgs),
    /// Requires redis-cli on PATH; each statement commits separately.
    Falkordb(FalkorArgs),
}

#[derive(Debug, Args)]
pub struct Bounds {
    /// Total client deadline, including all statements (seconds).
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_secs: u64,
    /// Maximum client stdout/stderr bytes; PostgreSQL SQL parser limit is 2 MiB.
    #[arg(long, default_value_t = 2_097_152, value_parser = clap::value_parser!(u64).range(1..=2_097_152))]
    max_output_bytes: u64,
}

#[derive(Debug, Args)]
pub struct PostgresArgs {
    /// Stable nickname, used as postgres:NAME; never use a connection string.
    #[arg(long)]
    name: String,
    /// Environment variable containing a single-host PostgreSQL URL. Omit to use PG* variables.
    #[arg(long)]
    dsn_env: Option<String>,
    #[arg(long, default_value = ".")]
    project: PathBuf,
    #[command(flatten)]
    bounds: Bounds,
}

#[derive(Debug, Args)]
pub struct Neo4jArgs {
    /// Environment variable containing a Bolt/Neo4j URI without credentials.
    #[arg(long)]
    uri_env: String,
    #[arg(long, default_value = "NEO4J_USERNAME")]
    user_env: String,
    #[arg(long, default_value = "NEO4J_PASSWORD")]
    password_env: String,
    /// Explicit destination database.
    #[arg(long)]
    database: String,
    #[command(flatten)]
    bounds: Bounds,
}

#[derive(Debug, Args)]
pub struct FalkorArgs {
    /// Environment variable containing redis://, rediss:// or falkordb:// host:port, without credentials.
    #[arg(long)]
    uri_env: String,
    /// Password environment variable for the default Redis user; omitted means anonymous.
    #[arg(long)]
    password_env: Option<String>,
    #[arg(long)]
    graph: String,
    #[command(flatten)]
    bounds: Bounds,
}

pub fn run(command: &Command, db: Option<&Path>, json_output: bool) -> Result<()> {
    let report = match command {
        Command::Introspect {
            backend: Introspect::Postgres(args),
        } => introspect(args)?,
        Command::Push { backend } => push(backend, db)?,
    };
    if json_output {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", report["message"].as_str().unwrap_or("Completed"));
    }
    Ok(())
}

fn env_value(name: &str) -> Result<String> {
    ensure!(
        !name.is_empty()
            && name.bytes().enumerate().all(|(i, c)| c == b'_'
                || c.is_ascii_alphabetic()
                || (i > 0 && c.is_ascii_digit())),
        "expected an environment variable name, not a credential value"
    );
    let value = std::env::var(name)
        .ok()
        .filter(|s| !s.is_empty() && !s.contains('\0'));
    value.context("required connection environment variable is missing, empty, or invalid")
}

fn endpoint(value: &str, schemes: &[&str]) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)
        .ok()
        .context("invalid connection URL")?;
    ensure!(
        schemes.contains(&url.scheme()) && url.host_str().is_some(),
        "unsupported connection URL; include the scheme and host"
    );
    Ok(url)
}

// URI userinfo/path percent decoding, without form decoding's '+' -> space rule.
fn uri_component(value: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.as_bytes().iter().copied();
    while let Some(c) = input.next() {
        if c == b'%' {
            let high = input.next().and_then(|c| (c as char).to_digit(16));
            let low = input.next().and_then(|c| (c as char).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                bail!("invalid percent encoding in PostgreSQL URL")
            };
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(c);
        }
    }
    let result = String::from_utf8(bytes)
        .ok()
        .context("PostgreSQL URL fields must be UTF-8")?;
    ensure!(!result.contains('\0'), "PostgreSQL URL contains a NUL byte");
    Ok(result)
}

fn postgres_environment(value: &str, command: &mut Process) -> Result<()> {
    let url = endpoint(value, &["postgres", "postgresql"])?;
    ensure!(
        url.fragment().is_none(),
        "PostgreSQL URL fragments are unsupported"
    );
    ensure!(
        !url.host_str().unwrap().contains([',', '%']),
        "PostgreSQL URL must use one hostname or IP address; use PG* environment configuration for socket or multi-host connections"
    );
    let database = uri_component(url.path().strip_prefix('/').unwrap_or(url.path()))?;
    ensure!(
        !database.is_empty(),
        "PostgreSQL URL requires a database name"
    );
    command.env("PGHOST", url.host_str().unwrap().trim_matches(['[', ']']));
    command.env("PGPORT", url.port().unwrap_or(5432).to_string());
    command.env("PGDATABASE", database);
    if !url.username().is_empty() {
        command.env("PGUSER", uri_component(url.username())?);
    }
    if let Some(password) = url.password() {
        command.env("PGPASSWORD", uri_component(password)?);
    }
    // libpq owns TLS and authentication. Unsupported options fail before spawn.
    for (key, value) in url.query_pairs() {
        let name = match key.as_ref() {
            "sslmode" => "PGSSLMODE",
            "sslcert" => "PGSSLCERT",
            "sslkey" => "PGSSLKEY",
            "sslrootcert" => "PGSSLROOTCERT",
            "sslcrl" => "PGSSLCRL",
            "connect_timeout" => "PGCONNECT_TIMEOUT",
            "application_name" => "PGAPPNAME",
            "options" => "PGOPTIONS",
            _ => bail!(
                "unsupported PostgreSQL URL option; use standard PG* environment configuration instead"
            ),
        };
        ensure!(!value.contains('\0'), "PostgreSQL URL contains a NUL byte");
        command.env(name, value.as_ref());
    }
    // An inherited service or hostaddr must not redirect the explicitly selected URL.
    command.env_remove("PGSERVICE").env_remove("PGHOSTADDR");
    Ok(())
}

fn introspect(args: &PostgresArgs) -> Result<serde_json::Value> {
    ensure!(
        !args.name.is_empty()
            && args.name.len() <= 128
            && args
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "PostgreSQL nickname must contain only letters, digits, '_' or '-' (1..128 characters)"
    );
    ensure!(
        args.project.is_dir(),
        "project must be an existing directory"
    );
    let source = format!("postgres:{}", args.name);
    let relative = sources::relative_path(&source, "schema.sql")?;
    let mut command = Process::new("pg_dump");
    command.args([
        "--schema-only",
        "--serializable-deferrable",
        "--no-password",
        "--no-owner",
        "--no-privileges",
        "--no-comments",
        "--no-security-labels",
        "--no-publications",
        "--no-subscriptions",
        "--encoding=UTF8",
    ]);
    if let Some(name) = &args.dsn_env {
        postgres_environment(&env_value(name)?, &mut command)?;
    }
    let bytes = execute(
        &mut command,
        b"",
        &args.bounds,
        deadline(&args.bounds),
        "pg_dump",
    )?;
    let sql = String::from_utf8(bytes)
        .ok()
        .context("pg_dump schema is not UTF-8")?;
    let hash = blake3::hash(sql.as_bytes()).to_hex().to_string();
    let facts = configs::parse(&relative, &sql, &hash)
        .map_err(|_| anyhow::anyhow!("could not parse PostgreSQL schema"))?
        .context("PostgreSQL schema was not recognized as SQL")?;
    ensure!(
        facts.diagnostics.is_empty(),
        "PostgreSQL schema extraction reported diagnostics; cached facts were not replaced"
    );
    let record = sources::save(&args.project, &source, facts)?;
    Ok(
        json!({"source": source, "path": record.facts.path, "nodes": record.facts.nodes.len(), "references": record.facts.references.len(), "indexed": false, "message": "PostgreSQL schema facts saved. Run ctx graph index on the project explicitly to update its graph."}),
    )
}

fn database(db: Option<&Path>) -> Result<PathBuf> {
    crate::discover_database(db)
}

fn destination_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "destination database/graph name must be nonempty, at most 256 bytes, and contain no controls"
    );
    Ok(())
}

fn push(backend: &Push, db: Option<&Path>) -> Result<serde_json::Value> {
    let snapshot = Store::open_read_only(&database(db)?)?.snapshot()?;
    let statements = export::cypher_statements(&snapshot)?;
    ensure!(
        statements
            .iter()
            .try_fold(0usize, |sum, s| sum.checked_add(s.len() + 2))
            .is_some_and(|size| size <= 64 * 1024 * 1024),
        "push payload exceeds 64 MiB; no statements sent"
    );
    let (backend_name, atomic) = match backend {
        Push::Neo4j(args) => {
            destination_name(&args.database)?;
            let uri = env_value(&args.uri_env)?;
            let url = endpoint(
                &uri,
                &[
                    "neo4j",
                    "neo4j+s",
                    "neo4j+ssc",
                    "bolt",
                    "bolt+s",
                    "bolt+ssc",
                ],
            )?;
            ensure!(
                url.username().is_empty() && url.password().is_none(),
                "connection URL must not contain credentials; use credential environment options"
            );
            let mut command = Process::new("cypher-shell");
            command
                .args(["--non-interactive", "--fail-fast", "--format", "plain"])
                .env("NEO4J_ADDRESS", &uri)
                .env_remove("NEO4J_URI")
                .env("NEO4J_USERNAME", env_value(&args.user_env)?)
                .env("NEO4J_PASSWORD", env_value(&args.password_env)?)
                .env("NEO4J_DATABASE", &args.database);
            let mut script = String::from(":begin\n");
            for statement in &statements {
                script.push_str(statement);
                script.push_str(";\n");
            }
            script.push_str(":commit\n");
            execute(&mut command, script.as_bytes(), &args.bounds, deadline(&args.bounds), "cypher-shell")
                .context("Neo4j push failed. The explicit transaction prevents partial graph writes, but commit acknowledgment may be lost; destination may contain the whole snapshot. No automatic retry was attempted")?;
            ("neo4j", true)
        }
        Push::Falkordb(args) => {
            destination_name(&args.graph)?;
            let url = endpoint(&env_value(&args.uri_env)?, &["redis", "rediss", "falkordb"])?;
            ensure!(
                url.username().is_empty() && url.password().is_none(),
                "connection URL must not contain credentials; use --password-env"
            );
            ensure!(
                matches!(url.path(), "" | "/") && url.query().is_none() && url.fragment().is_none(),
                "FalkorDB URL must contain only scheme, host and port; select the graph with --graph"
            );
            let password = args.password_env.as_deref().map(env_value).transpose()?;
            let end = deadline(&args.bounds);
            for (completed, statement) in statements.iter().enumerate() {
                let mut command = Process::new("redis-cli");
                command.args([
                    "-e",
                    "--raw",
                    "-h",
                    url.host_str().unwrap().trim_matches(['[', ']']),
                    "-p",
                    &url.port().unwrap_or(6379).to_string(),
                ]);
                if url.scheme() == "rediss" {
                    command.arg("--tls");
                }
                command.env_remove("REDISCLI_AUTH");
                if let Some(password) = &password {
                    command.env("REDISCLI_AUTH", password);
                }
                // -x appends stdin as ONE last argument, preserving embedded semicolons.
                command.args(["-x", "GRAPH.QUERY", &args.graph]);
                execute(&mut command, statement.as_bytes(), &args.bounds, end, "redis-cli")
                    .with_context(|| format!("FalkorDB push failed after {completed} confirmed statements of {}. Earlier statements remain committed; the current statement may also have committed or still be running. No automatic retry was attempted", statements.len()))?;
            }
            ("falkordb", false)
        }
    };
    Ok(
        json!({"backend": backend_name, "generation": snapshot.generation, "submitted_nodes": snapshot.nodes.len(), "submitted_edges": snapshot.edges.len(), "confirmed_statements": statements.len(), "atomic": atomic, "message": format!("{backend_name} push acknowledged {} statements ({} nodes, {} edges submitted).", statements.len(), snapshot.nodes.len(), snapshot.edges.len())}),
    )
}

fn deadline(bounds: &Bounds) -> Instant {
    Instant::now() + Duration::from_secs(bounds.timeout_secs)
}

/// Temporary files avoid blocked pipes, including grandchildren inheriting stdout.
/// Client output is never included in errors: drivers often echo credentials.
fn execute(
    command: &mut Process,
    input: &[u8],
    bounds: &Bounds,
    end: Instant,
    tool: &str,
) -> Result<Vec<u8>> {
    ensure!(
        Instant::now() < end,
        "{tool} total deadline exceeded before starting the next statement"
    );
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    stdin.rewind()?;
    let mut stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| {
        anyhow::anyhow!("cannot start {tool}; install the client and ensure it is on PATH")
    })?;
    let result = (|| -> Result<()> {
        loop {
            ensure!(Instant::now() < end, "{tool} total deadline exceeded");
            ensure!(
                stdout.metadata()?.len() <= bounds.max_output_bytes
                    && stderr.metadata()?.len() <= bounds.max_output_bytes,
                "{tool} output exceeds byte limit"
            );
            if let Some(status) = child.try_wait()? {
                ensure!(
                    status.success(),
                    "{tool} exited unsuccessfully; verify connection, permissions and client version (client output omitted to protect credentials)"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    // Stop inherited writers before consuming output, even if the leader exited.
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result?;
    ensure!(
        stdout.metadata()?.len() <= bounds.max_output_bytes
            && stderr.metadata()?.len() <= bounds.max_output_bytes,
        "{tool} output exceeds byte limit"
    );
    stdout.rewind()?;
    let mut output = Vec::new();
    stdout
        .take(bounds.max_output_bytes + 1)
        .read_to_end(&mut output)?;
    ensure!(
        output.len() as u64 <= bounds.max_output_bytes,
        "{tool} output exceeds byte limit"
    );
    Ok(output)
}
