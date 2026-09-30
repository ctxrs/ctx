#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

pub struct Sandbox {
    directory: TempDir,
}

pub struct RunningServer<'a> {
    _sandbox: &'a Sandbox,
    child: std::process::Child,
    pub endpoint: String,
}

impl Drop for RunningServer<'_> {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sandbox {
    pub fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("ctx-hosted-cli-")
            .tempdir()
            .unwrap();
        for child in ["home", "config", "cache", "state", "runtime", "tmp"] {
            fs::create_dir(directory.path().join(child)).unwrap();
        }
        Self { directory }
    }

    pub fn root(&self) -> &Path {
        self.directory.path()
    }
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub fn command(&self) -> Command {
        let mut command = Command::from_std(self.std_command());
        command.timeout(Duration::from_secs(30));
        command
    }

    pub fn std_command(&self) -> std::process::Command {
        let program = PathBuf::from(Command::cargo_bin("ctx").unwrap().get_program());
        let program = std::path::absolute(program).unwrap();
        let mut command = std::process::Command::new(program);
        command.env_clear().current_dir(self.root());
        // Windows needs its system directory to load ordinary runtime libraries.
        #[cfg(windows)]
        if let Some(system) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system);
        }
        for (name, child) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "state"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("CTX_RUNTIME_DIR", "runtime"),
            ("APPDATA", "config"),
            ("LOCALAPPDATA", "state"),
            ("TMPDIR", "tmp"),
            ("TEMP", "tmp"),
            ("TMP", "tmp"),
            ("CTX_DATA_ROOT", "history"),
        ] {
            command.env(name, self.path(child));
        }
        command
            .env("CTX_ANALYTICS_ENABLED", "false")
            .env("CTX_LOCAL_USAGE_ENABLED", "false")
            .env("CTX_UPGRADE_AUTO", "off")
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("TZ", "UTC")
            .env("NO_COLOR", "1");
        command
    }

    pub fn private_file(&self, name: &str, contents: &[u8]) -> PathBuf {
        use std::io::Write;
        let path = self.path(name);
        let mut file =
            ctx_history_platform::platform_security::create_private_file_new(&path).unwrap();
        file.write_all(contents).unwrap();
        file.sync_all().unwrap();
        path
    }

    pub fn server(&self) -> Command {
        let mut command = self.command();
        command.arg("server").arg("--root").arg(self.path("server"));
        command
    }

    pub fn init(&self) -> Value {
        success(
            self.server()
                .args(["init", "test-team", "--credentials-out"])
                .arg(self.path("operator.json"))
                .arg("--format=json"),
        )
    }

    pub fn assert_no_local_index(&self) {
        assert!(!self.path("history/search").exists());
        assert!(!self.path("history/daemon").exists());
        assert!(!self.path("home/.ctx").exists());
    }

    pub fn start_server(&self) -> RunningServer<'_> {
        self.start_server_at(&self.path("server"), &self.path("operator.json"))
    }

    pub fn start_server_at(&self, root: &Path, _credentials: &Path) -> RunningServer<'_> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        self.start_server_bound(root, address)
    }

    pub fn start_server_ephemeral(&self) -> RunningServer<'_> {
        self.start_server_bound(&self.path("server"), "127.0.0.1:0".parse().unwrap())
    }

    fn start_server_bound(&self, root: &Path, address: std::net::SocketAddr) -> RunningServer<'_> {
        use std::{
            net::{SocketAddr, TcpStream},
            process::Stdio,
            time::Instant,
        };
        let readiness = tempfile::NamedTempFile::new_in(self.path("tmp")).unwrap();
        let mut command = self.std_command();
        command.arg("server").arg("--root").arg(root);
        command.args(["run", "--bind", &address.to_string()]);
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(readiness.reopen().unwrap()))
            .spawn()
            .unwrap();
        let mut server = RunningServer {
            _sandbox: self,
            child,
            endpoint: format!("http://{address}"),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "server stopped before readiness"
            );
            let log = fs::read_to_string(readiness.path()).unwrap();
            if let Some(bound) = log
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .find_map(|line| {
                    line.trim_end()
                        .strip_prefix("ctx history server listening on ")
                })
            {
                let bound: SocketAddr = bound.parse().unwrap();
                let settings: Value =
                    serde_json::from_slice(&fs::read(root.join("admin/settings.json")).unwrap())
                        .unwrap();
                let endpoint = settings["connection"]["endpoint"].as_str().unwrap();
                let saved: SocketAddr = endpoint.strip_prefix("http://").unwrap().parse().unwrap();
                assert_eq!(saved.port(), bound.port());
                assert_ne!(bound.port(), 0);
                if TcpStream::connect_timeout(&saved, Duration::from_millis(100)).is_ok() {
                    server.endpoint = endpoint.to_owned();
                    return server;
                }
            }
            assert!(Instant::now() < deadline, "server did not become ready");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

pub fn success(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().clone();
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid JSON: {error}; stdout={:?}", output.stdout))
}

pub fn failure(command: &mut Command) -> Value {
    let output = command.assert().failure().get_output().clone();
    assert!(
        output.stdout.is_empty(),
        "failure should not emit a success result"
    );
    serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|error| panic!("invalid error JSON: {error}; stderr={:?}", output.stderr))
}

/// A committed synthetic Core generation, with no native provider or daemon.
pub fn seed_history(sandbox: &Sandbox) -> ctx_history_core::CoreRecord {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
        NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
        SourceKey, SourceObservation, TypedKey,
    };
    use ctx_history_index::{GenerationWriter, WriterOptions};

    let source = SourceKey::derive(
        "custom",
        "synthetic_hosted_cli",
        "session",
        1,
        SourceAnchor::CatalogLineage([19; 32]),
    )
    .unwrap();
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("synthetic-session").unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let event_key =
        NativeItemKey::native_id("message", TypedKey::utf8("synthetic-event").unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &event_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "synthetic-hosted-cli-v1",
        "portable meadow observatory fixture",
    )
    .unwrap();
    record.role = Some("user".to_owned());
    record.content.structured_content = Some(serde_json::json!({"synthetic": ["complete", 7]}));
    record.validate_contract().unwrap();
    let root = sandbox.path("history");
    ctx_history_platform::platform_security::create_private_directory_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[sources]\nautomatic = false\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let mut writer = GenerationWriter::open(
        root.join("search/lexical"),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    let observation = SourceObservation::new(source, "synthetic-v1", vec![1]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "synthetic-hosted-cli-v1",
                [7; 32],
                ScannedSourceCounts {
                    complete_records: 1,
                    retained_records: 1,
                    indexed_documents: 1,
                    certified_bytes: serde_json::to_vec(&record).unwrap().len() as u64,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    record
}
