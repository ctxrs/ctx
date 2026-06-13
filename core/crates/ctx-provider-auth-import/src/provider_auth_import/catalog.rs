use super::*;

pub async fn list_provider_auth_import_candidates() -> Result<Vec<ProviderAuthImportCandidate>> {
    let scanner = AuthImportScanner::discover()?;
    let candidates = scanner.scan().into_iter().map(|c| c.candidate).collect();
    Ok(candidates)
}

#[cfg(test)]
pub(super) fn scan_with_roots(roots: &HostRoots) -> Vec<CandidateMaterial> {
    AuthImportScanner {
        roots: roots.clone(),
    }
    .scan()
}

pub(super) fn host_roots() -> Result<HostRoots> {
    let base = directories::BaseDirs::new().context("missing home directory")?;
    let home = optional_path_env(CTX_PROVIDER_AUTH_IMPORT_HOME_ENV)
        .unwrap_or_else(|| base.home_dir().to_path_buf());
    let xdg_config = optional_path_env(CTX_PROVIDER_AUTH_IMPORT_XDG_CONFIG_HOME_ENV)
        .or_else(|| optional_path_env("XDG_CONFIG_HOME"))
        .unwrap_or_else(|| home.join(".config"));
    let xdg_data = optional_path_env(CTX_PROVIDER_AUTH_IMPORT_XDG_DATA_HOME_ENV)
        .or_else(|| optional_path_env("XDG_DATA_HOME"))
        .unwrap_or_else(|| home.join(".local").join("share"));
    let codex_home = optional_path_env(CTX_PROVIDER_AUTH_IMPORT_CODEX_HOME_ENV)
        .or_else(|| optional_path_env("CODEX_HOME"))
        .unwrap_or_else(|| home.join(".codex"));
    Ok(HostRoots {
        home,
        xdg_config,
        xdg_data,
        codex_home,
    })
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

impl AuthImportScanner {
    pub(super) fn discover() -> Result<Self> {
        Ok(Self {
            roots: host_roots()?,
        })
    }

    pub(super) fn scan(&self) -> Vec<CandidateMaterial> {
        let mut out = Vec::new();
        let mut seen: HashMap<String, ()> = HashMap::new();
        for spec in build_catalog(&self.roots) {
            if let Some(candidate) = candidate_from_spec(&spec) {
                if seen.insert(candidate.candidate.id.clone(), ()).is_none() {
                    out.push(candidate);
                }
            }
        }
        out.sort_by(|a, b| {
            a.candidate
                .provider_label
                .cmp(&b.candidate.provider_label)
                .then_with(|| a.candidate.path.cmp(&b.candidate.path))
        });
        out
    }
}

fn build_catalog(roots: &HostRoots) -> Vec<PathSpec> {
    let mut out = vec![
        PathSpec {
            provider_id: "codex",
            provider_label: "Codex",
            kind: "auth_file",
            signal_strength: "strong",
            confidence: "high",
            importable: true,
            unsupported_reason: None,
            path: roots.codex_home.join("auth.json"),
        },
        PathSpec {
            provider_id: "amp",
            provider_label: "Amp",
            kind: "config_file",
            signal_strength: "weak",
            confidence: "low-medium",
            importable: false,
            unsupported_reason: Some(
                "Amp auth is often keychain-backed; no canonical importable auth file was found.",
            ),
            path: roots.xdg_config.join("amp").join("settings.json"),
        },
        PathSpec {
            provider_id: "copilot",
            provider_label: "Copilot",
            kind: "config_file",
            signal_strength: "weak",
            confidence: "low-medium",
            importable: false,
            unsupported_reason: Some(
                "Copilot auth storage is not a stable canonical file path in available docs.",
            ),
            path: roots.home.join(".copilot").join("lsp-config.json"),
        },
        PathSpec {
            provider_id: "cursor",
            provider_label: "Cursor",
            kind: "config_file",
            signal_strength: "weak",
            confidence: "low-medium",
            importable: false,
            unsupported_reason: Some(
                "Cursor auth storage is not a stable canonical file path in available docs.",
            ),
            path: roots.home.join(".cursor").join("cli-config.json"),
        },
        PathSpec {
            provider_id: "droid",
            provider_label: "Droid",
            kind: "config_file",
            signal_strength: "weak",
            confidence: "low-medium",
            importable: false,
            unsupported_reason: Some(
                "Droid account auth is documented as encrypted/keychain-backed storage.",
            ),
            path: roots.home.join(".factory").join("settings.json"),
        },
        PathSpec {
            provider_id: "gemini",
            provider_label: "Gemini",
            kind: "env_file",
            signal_strength: "strong",
            confidence: "medium",
            importable: true,
            unsupported_reason: None,
            path: roots.home.join(".gemini").join(".env"),
        },
        PathSpec {
            provider_id: "gemini",
            provider_label: "Gemini",
            kind: "auth_file",
            signal_strength: "weak",
            confidence: "medium",
            importable: true,
            unsupported_reason: None,
            path: roots.home.join(".gemini").join("oauth_creds.json"),
        },
        PathSpec {
            provider_id: "opencode",
            provider_label: "OpenCode",
            kind: "auth_file",
            signal_strength: "strong",
            confidence: "high",
            importable: true,
            unsupported_reason: None,
            path: roots.xdg_data.join("opencode").join("auth.json"),
        },
        PathSpec {
            provider_id: "qwen",
            provider_label: "Qwen",
            kind: "env_file",
            signal_strength: "strong",
            confidence: "medium",
            importable: true,
            unsupported_reason: None,
            path: roots.home.join(".qwen").join(".env"),
        },
    ];

    let amp_oauth = roots.home.join(".amp").join("oauth");
    if let Ok(entries) = std::fs::read_dir(&amp_oauth) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("json")) {
                out.push(PathSpec {
                    provider_id: "amp",
                    provider_label: "Amp",
                    kind: "auth_file",
                    signal_strength: "weak",
                    confidence: "low-medium",
                    importable: true,
                    unsupported_reason: None,
                    path,
                });
            }
        }
    }

    out
}

fn candidate_from_spec(spec: &PathSpec) -> Option<CandidateMaterial> {
    if !spec.path.exists() {
        return None;
    }

    let id = build_candidate_id(spec.provider_id, spec.kind, &spec.path);
    let mut candidate = ProviderAuthImportCandidate {
        id,
        provider_id: spec.provider_id.to_string(),
        provider_label: spec.provider_label.to_string(),
        kind: spec.kind.to_string(),
        path: spec.path.to_string_lossy().to_string(),
        signal_strength: spec.signal_strength.to_string(),
        confidence: spec.confidence.to_string(),
        parse_status: "detected".to_string(),
        unsupported_reason: spec.unsupported_reason.map(|s| s.to_string()),
        summary: None,
        account_identity: None,
        endpoint: None,
        auth_type: None,
        fingerprint: None,
        last_modified: file_mtime(&spec.path),
    };

    if !spec.importable {
        candidate.parse_status = "unsupported".to_string();
        return Some(CandidateMaterial {
            candidate,
            importable: false,
            secret_bytes: None,
            label: None,
        });
    }

    let bytes = match std::fs::read(&spec.path) {
        Ok(bytes) => bytes,
        Err(err) => {
            candidate.parse_status = "parse_error".to_string();
            candidate.unsupported_reason = Some(format!("failed to read file: {err}"));
            return Some(CandidateMaterial {
                candidate,
                importable: false,
                secret_bytes: None,
                label: None,
            });
        }
    };

    if bytes.is_empty() {
        candidate.parse_status = "parse_error".to_string();
        candidate.unsupported_reason = Some("file is empty".to_string());
        return Some(CandidateMaterial {
            candidate,
            importable: false,
            secret_bytes: None,
            label: None,
        });
    }

    let fingerprint = sha256_hex(&bytes);
    candidate.fingerprint = Some(fingerprint);

    if spec.provider_id == "gemini" && spec.kind == "auth_file" {
        let file_name = spec
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !file_name.eq_ignore_ascii_case("oauth_creds.json") {
            return None;
        }

        let oauth_raw = match String::from_utf8(bytes.clone()) {
            Ok(raw) => raw,
            Err(_) => {
                candidate.parse_status = "parse_error".to_string();
                candidate.unsupported_reason =
                    Some("gemini oauth_creds.json must be UTF-8 JSON".to_string());
                return Some(CandidateMaterial {
                    candidate,
                    importable: false,
                    secret_bytes: None,
                    label: None,
                });
            }
        };

        let oauth_value = match serde_json::from_str::<serde_json::Value>(&oauth_raw) {
            Ok(value) => value,
            Err(err) => {
                candidate.parse_status = "parse_error".to_string();
                candidate.unsupported_reason =
                    Some(format!("gemini oauth_creds.json must be valid JSON: {err}"));
                return Some(CandidateMaterial {
                    candidate,
                    importable: false,
                    secret_bytes: None,
                    label: None,
                });
            }
        };
        if !oauth_value.is_object() {
            candidate.parse_status = "parse_error".to_string();
            candidate.unsupported_reason =
                Some("gemini oauth_creds.json must be a JSON object".to_string());
            return Some(CandidateMaterial {
                candidate,
                importable: false,
                secret_bytes: None,
                label: None,
            });
        }

        let google_accounts_path = spec
            .path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("google_accounts.json");
        if google_accounts_path.exists() {
            let google_raw = match std::fs::read_to_string(&google_accounts_path) {
                Ok(contents) => contents,
                Err(err) => {
                    candidate.parse_status = "parse_error".to_string();
                    candidate.unsupported_reason =
                        Some(format!("failed to read google_accounts.json: {err}"));
                    return Some(CandidateMaterial {
                        candidate,
                        importable: false,
                        secret_bytes: None,
                        label: None,
                    });
                }
            };
            if let Some(raw) = parsers::trim_to_option(&google_raw) {
                if let Err(err) = serde_json::from_str::<serde_json::Value>(&raw) {
                    candidate.parse_status = "parse_error".to_string();
                    candidate.unsupported_reason = Some(format!(
                        "gemini google_accounts.json must be valid JSON: {err}"
                    ));
                    return Some(CandidateMaterial {
                        candidate,
                        importable: false,
                        secret_bytes: None,
                        label: None,
                    });
                }
            }
        }

        candidate.summary = summarize_json_candidate(spec.provider_id, &oauth_value);
        candidate.auth_type = Some("subscription".to_string());
        candidate.parse_status = "parsed".to_string();
        return Some(CandidateMaterial {
            candidate,
            importable: true,
            secret_bytes: Some(bytes),
            label: Some(format!("Imported {} profile", spec.provider_label)),
        });
    }

    let label = match spec.kind {
        "env_file" => {
            let raw = String::from_utf8_lossy(&bytes);
            let env_map = parsers::parse_env_file(&raw);
            let has_secret = env_map
                .keys()
                .any(|k| k.contains("API_KEY") || k.contains("TOKEN"));
            if !has_secret {
                candidate.parse_status = "unsupported".to_string();
                candidate.unsupported_reason =
                    Some("No API key/token variable found in env file.".to_string());
                return Some(CandidateMaterial {
                    candidate,
                    importable: false,
                    secret_bytes: None,
                    label: None,
                });
            }
            let (summary, endpoint) = summarize_env(spec.provider_id, &env_map);
            candidate.summary = summary;
            candidate.endpoint = endpoint;
            candidate.auth_type = Some("api_key".to_string());
            candidate.parse_status = "parsed".to_string();
            Some(format!("{} API key", spec.provider_label))
        }
        _ => {
            let summary = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(value) => summarize_json_candidate(spec.provider_id, &value),
                Err(_) => None,
            };
            candidate.summary = summary;
            candidate.auth_type = Some(if spec.provider_id == "codex" {
                "subscription".to_string()
            } else {
                "file_auth".to_string()
            });
            candidate.parse_status = "parsed".to_string();
            Some(format!("Imported {} profile", spec.provider_label))
        }
    };

    Some(CandidateMaterial {
        candidate,
        importable: true,
        secret_bytes: Some(bytes),
        label,
    })
}

pub(super) fn summarize_env(
    provider_id: &str,
    env_map: &BTreeMap<String, String>,
) -> (Option<String>, Option<String>) {
    let endpoint = [
        "OPENAI_BASE_URL",
        "BASE_URL",
        "CTX_GATEWAY_BASE_URL",
        "ANTHROPIC_BASE_URL",
    ]
    .into_iter()
    .find_map(|k| env_map.get(k).cloned().filter(|s| !s.trim().is_empty()));

    let key_name = env_map
        .keys()
        .find(|k| k.contains("API_KEY") || k.contains("TOKEN"))
        .cloned();
    let summary = match (provider_id, key_name) {
        ("gemini", Some(name)) => Some(format!("Gemini key from {name}")),
        ("qwen", Some(name)) => Some(format!("Qwen/OpenAI-compatible key from {name}")),
        (_, Some(name)) => Some(format!("Credential from {name}")),
        _ => None,
    };

    (summary, endpoint)
}

pub(super) fn summarize_json_candidate(
    provider_id: &str,
    value: &serde_json::Value,
) -> Option<String> {
    let find_string = |keys: &[&str]| -> Option<String> {
        for key in keys {
            if let Some(v) = value.get(key).and_then(|v| v.as_str()) {
                let t = v.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    };
    if provider_id == "codex" {
        if let Some(email) = find_string(&["email", "user_email"]) {
            return Some(format!("Codex account {email}"));
        }
        return None;
    }
    if let Some(email) = find_string(&["email", "user_email", "username"]) {
        return Some(email);
    }
    None
}

fn optional_path_env(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn file_mtime(path: &Path) -> Option<DateTime<Utc>> {
    let md = std::fs::metadata(path).ok()?;
    let t = md.modified().ok()?;
    Some(DateTime::<Utc>::from(t))
}

fn build_candidate_id(provider_id: &str, kind: &str, path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider_id.as_bytes());
    hasher.update(b"|");
    hasher.update(kind.as_bytes());
    hasher.update(b"|");
    hasher.update(path.to_string_lossy().as_bytes());
    hex::encode(hasher.finalize())
}
