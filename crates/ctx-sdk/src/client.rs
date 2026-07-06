use ctx_protocol::{
    AgentHistoryEnvelope, AgentHistoryErrorBody, AgentHistoryErrorCode, AgentHistoryOperation,
    BackendInfo, JsonObject,
};
use serde_json::json;

use crate::{
    local_cli::run_ctx_json, normalize::normalize, AgentHistoryBackend, AgentHistoryError,
    HostedBackendConfig, ImportOptions, InitOptions, LocalBackendConfig, SearchOptions,
    ShowEventOptions, ShowSessionOptions,
};

#[derive(Debug, Clone)]
pub struct AgentHistoryClient {
    backend: AgentHistoryBackend,
}

impl AgentHistoryClient {
    pub fn local(config: LocalBackendConfig) -> Self {
        Self {
            backend: AgentHistoryBackend::Local(config),
        }
    }

    pub fn hosted(config: HostedBackendConfig) -> Self {
        Self {
            backend: AgentHistoryBackend::Hosted(config),
        }
    }

    pub fn backend_info(&self) -> BackendInfo {
        match &self.backend {
            AgentHistoryBackend::Local(config) => BackendInfo::local(
                config
                    .data_root
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            AgentHistoryBackend::Hosted(config) => {
                BackendInfo::hosted(Some(config.base_url.clone()))
            }
        }
    }

    pub fn status(&self) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json(AgentHistoryOperation::Status, &["status", "--json"])
    }

    pub fn init(&self, options: InitOptions) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut args = vec!["setup", "--json", "--progress", "none"];
        if options.catalog_only {
            args.push("--catalog-only");
        }
        self.local_json(AgentHistoryOperation::Init, &args)
    }

    pub fn sources(&self) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json(AgentHistoryOperation::Sources, &["sources", "--json"])
    }

    pub fn import_history(
        &self,
        options: ImportOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.import_or_sync(AgentHistoryOperation::Import, options)
    }

    pub fn sync(&self, options: ImportOptions) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.import_or_sync(AgentHistoryOperation::Sync, options)
    }

    pub fn search(
        &self,
        options: SearchOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        if !options.has_intent() {
            return Err(AgentHistoryError::new(
                AgentHistoryErrorCode::InvalidRequest,
                "search requires a query, term, or file option",
                false,
            ));
        }
        let mut owned = Vec::<String>::new();
        owned.push("search".to_owned());
        if let Some(query) = options.query {
            owned.push(query);
        }
        for term in options.terms {
            owned.push("--term".to_owned());
            owned.push(term);
        }
        owned.extend(["--limit".to_owned(), options.limit.to_string()]);
        push_opt(&mut owned, "--provider", options.provider);
        push_opt(&mut owned, "--workspace", options.workspace);
        push_opt(&mut owned, "--since", options.since);
        if let Some(file) = options.file {
            push_opt(
                &mut owned,
                "--file",
                Some(file.to_string_lossy().into_owned()),
            );
        }
        push_opt(&mut owned, "--session", options.session);
        if options.events {
            owned.push("--events".to_owned());
        }
        owned.extend(["--refresh".to_owned(), options.refresh.as_arg().to_owned()]);
        if options.include_current_session {
            owned.push("--include-current-session".to_owned());
        }
        owned.push("--json".to_owned());
        self.local_json_owned(AgentHistoryOperation::Search, owned)
    }

    pub fn show_event(
        &self,
        id: impl AsRef<str>,
        options: ShowEventOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut owned = vec![
            "show".to_owned(),
            "event".to_owned(),
            id.as_ref().to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ];
        if options.before > 0 {
            owned.extend(["--before".to_owned(), options.before.to_string()]);
        }
        if options.after > 0 {
            owned.extend(["--after".to_owned(), options.after.to_string()]);
        }
        if let Some(window) = options.window {
            owned.extend(["--window".to_owned(), window.to_string()]);
        }
        self.local_json_owned(AgentHistoryOperation::ShowEvent, owned)
    }

    pub fn show_session(
        &self,
        id: impl AsRef<str>,
        options: ShowSessionOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            AgentHistoryOperation::ShowSession,
            vec![
                "show".to_owned(),
                "session".to_owned(),
                id.as_ref().to_owned(),
                "--mode".to_owned(),
                options.mode,
                "--format".to_owned(),
                "json".to_owned(),
            ],
        )
    }

    pub fn locate_event(
        &self,
        id: impl AsRef<str>,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            AgentHistoryOperation::LocateEvent,
            vec![
                "locate".to_owned(),
                "event".to_owned(),
                id.as_ref().to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
        )
    }

    pub fn locate_session(
        &self,
        id: impl AsRef<str>,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            AgentHistoryOperation::LocateSession,
            vec![
                "locate".to_owned(),
                "session".to_owned(),
                id.as_ref().to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
            ],
        )
    }

    fn import_or_sync(
        &self,
        operation: AgentHistoryOperation,
        options: ImportOptions,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let mut owned = vec![
            "import".to_owned(),
            "--json".to_owned(),
            "--progress".to_owned(),
            "none".to_owned(),
        ];
        push_opt(&mut owned, "--provider", options.provider);
        if let Some(path) = options.path {
            push_opt(
                &mut owned,
                "--path",
                Some(path.to_string_lossy().into_owned()),
            );
        }
        if options.all {
            owned.push("--all".to_owned());
        }
        if options.resume {
            owned.push("--resume".to_owned());
        }
        self.local_json_owned(operation, owned)
    }

    fn local_json(
        &self,
        operation: AgentHistoryOperation,
        args: &[&str],
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        self.local_json_owned(
            operation,
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        )
    }

    fn local_json_owned(
        &self,
        operation: AgentHistoryOperation,
        args: Vec<String>,
    ) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
        let config = match &self.backend {
            AgentHistoryBackend::Local(config) => config,
            AgentHistoryBackend::Hosted(config) => {
                let mut details = JsonObject::new();
                details.insert("backend".to_owned(), json!("hosted"));
                return Err(AgentHistoryError {
                    body: AgentHistoryErrorBody {
                        details: Some(details),
                        ..AgentHistoryErrorBody::new(
                            AgentHistoryErrorCode::NotSupported,
                            "hosted ctx agent history backend is not available in this in-repo SDK",
                            false,
                        )
                    },
                }
                .with_cause(config.base_url.clone()));
            }
        };

        let raw = run_ctx_json(config, &args)?;
        normalize(operation, self.backend_info(), raw)
    }
}

fn push_opt(args: &mut Vec<String>, name: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(name.to_owned());
        args.push(value);
    }
}
