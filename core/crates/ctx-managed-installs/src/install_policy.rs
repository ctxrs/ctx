use anyhow::{Context, Result};

pub(crate) const CTX_HOST_EXECUTION_POLICY_ENV: &str = "CTX_HOST_EXECUTION_POLICY";
pub(crate) const CTX_MANAGED_NPM_REGISTRY_ENV: &str = "CTX_MANAGED_NPM_REGISTRY";
pub(crate) const CTX_MANAGED_PIP_INDEX_URL_ENV: &str = "CTX_MANAGED_PIP_INDEX_URL";

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct PipInstallPolicy {
    pub(crate) index_url: Option<String>,
}

pub(crate) fn sandbox_only_execution_policy_enabled() -> Result<bool> {
    let value = std::env::var(CTX_HOST_EXECUTION_POLICY_ENV).unwrap_or_default();
    match value.trim() {
        "" | "allow_host" => Ok(false),
        "sandbox_only" => Ok(true),
        other => anyhow::bail!(
            "unsupported {CTX_HOST_EXECUTION_POLICY_ENV}: {other} (expected allow_host|sandbox_only)"
        ),
    }
}

pub(crate) fn managed_https_url_env(env_key: &'static str) -> Result<Option<String>> {
    let value = std::env::var(env_key).unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = url::Url::parse(value).with_context(|| format!("invalid {env_key}: {value}"))?;
    if parsed.scheme() != "https" {
        anyhow::bail!("{env_key} must use an https URL");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("{env_key} must not include credentials");
    }
    Ok(Some(value.trim_end_matches('/').to_string()))
}

pub(crate) fn validate_npm_install_policy(package_spec: &str) -> Result<Option<String>> {
    validate_managed_package_spec_argv_safe("npm", package_spec)?;
    let registry = managed_https_url_env(CTX_MANAGED_NPM_REGISTRY_ENV)?;
    let sandbox_only = sandbox_only_execution_policy_enabled()?;
    if sandbox_only && registry.is_none() {
        anyhow::bail!(
            "live npm registry installs are disabled when {CTX_HOST_EXECUTION_POLICY_ENV}=sandbox_only; configure {CTX_MANAGED_NPM_REGISTRY_ENV} to an approved HTTPS mirror"
        );
    }
    if sandbox_only && is_direct_npm_package_spec(package_spec) {
        anyhow::bail!(
            "direct npm package specs are disabled when {CTX_HOST_EXECUTION_POLICY_ENV}=sandbox_only; publish the package to the configured {CTX_MANAGED_NPM_REGISTRY_ENV} mirror"
        );
    }
    Ok(registry)
}

fn validate_managed_package_spec_argv_safe(manager: &str, package_spec: &str) -> Result<()> {
    let spec = package_spec.trim();
    if spec.starts_with('-') {
        anyhow::bail!(
            "{manager} package spec must not start with '-' because package managers parse it as installer options"
        );
    }
    Ok(())
}

fn is_direct_npm_package_spec(package_spec: &str) -> bool {
    let spec = package_spec.trim();
    if has_direct_npm_spec_prefix(spec) {
        return true;
    }
    if let Some(source_spec) = npm_package_source_spec(spec) {
        return is_direct_npm_source_spec(source_spec);
    }

    let lower = spec.to_ascii_lowercase();
    spec.contains('/') && !spec.starts_with('@')
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.gz")
}

fn npm_package_source_spec(package_spec: &str) -> Option<&str> {
    if package_spec.starts_with('@') {
        let scoped_name = package_spec.strip_prefix('@')?;
        let slash_index = scoped_name.find('/')?;
        let package_name = scoped_name.get(slash_index + 1..)?;
        let version_separator = package_name.find('@')?;
        return package_name.get(version_separator + 1..);
    }

    let version_separator = package_spec.find('@')?;
    if version_separator == 0 {
        return None;
    }
    package_spec.get(version_separator + 1..)
}

fn is_direct_npm_source_spec(source_spec: &str) -> bool {
    let source = source_spec.trim();
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("npm:") {
        return is_direct_npm_package_spec(&source[4..]);
    }
    has_direct_npm_spec_prefix(source) || source.contains('/')
}

fn has_direct_npm_spec_prefix(package_spec: &str) -> bool {
    let spec = package_spec.trim();
    let lower = spec.to_ascii_lowercase();
    if [
        "http://",
        "https://",
        "git://",
        "git+http://",
        "git+https://",
        "git+ssh://",
        "ssh://",
        "file:",
        "link:",
        "workspace:",
        "github:",
        "gitlab:",
        "bitbucket:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    lower.starts_with("git@")
        || spec.starts_with('/')
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with("~/")
        || spec.contains('\\')
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.gz")
        || looks_like_windows_absolute_path(spec)
}

fn looks_like_windows_absolute_path(package_spec: &str) -> bool {
    let bytes = package_spec.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

pub(crate) fn validate_pip_install_policy(package_spec: &str) -> Result<PipInstallPolicy> {
    validate_managed_package_spec_argv_safe("pip", package_spec)?;
    let index_url = managed_https_url_env(CTX_MANAGED_PIP_INDEX_URL_ENV)?;
    if !sandbox_only_execution_policy_enabled()? {
        return Ok(PipInstallPolicy { index_url });
    }

    if index_url.is_none() {
        anyhow::bail!(
            "live pip index installs are disabled when {CTX_HOST_EXECUTION_POLICY_ENV}=sandbox_only; configure {CTX_MANAGED_PIP_INDEX_URL_ENV} to an approved HTTPS mirror"
        );
    }
    if is_direct_pip_package_spec(package_spec) {
        anyhow::bail!(
            "direct pip package specs are disabled when {CTX_HOST_EXECUTION_POLICY_ENV}=sandbox_only; publish the package to the configured {CTX_MANAGED_PIP_INDEX_URL_ENV} mirror"
        );
    }

    Ok(PipInstallPolicy { index_url })
}

fn is_direct_pip_package_spec(package_spec: &str) -> bool {
    let spec = package_spec.trim();
    if has_direct_pip_spec_prefix(spec) {
        return true;
    }
    let lower = spec.to_ascii_lowercase();
    if lower.contains(" @ ") {
        let Some((_, source)) = lower.split_once(" @ ") else {
            return false;
        };
        return has_direct_pip_spec_prefix(source.trim());
    }
    if let Some((_, source)) = lower.split_once('@') {
        if has_direct_pip_spec_prefix(source.trim()) {
            return true;
        }
    }
    false
}

fn has_direct_pip_spec_prefix(package_spec: &str) -> bool {
    let spec = package_spec.trim();
    let lower = spec.to_ascii_lowercase();
    [
        "http://",
        "https://",
        "git://",
        "git+http://",
        "git+https://",
        "git+ssh://",
        "git+file://",
        "ssh://",
        "file://",
        "file:",
        "hg+",
        "svn+",
        "bzr+",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        || spec.starts_with('/')
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with("~/")
        || spec.contains('\\')
        || lower.ends_with(".whl")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".zip")
        || looks_like_windows_absolute_path(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        old_value: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            // SAFETY: Guarded by ENV_LOCK so tests mutate process env serially.
            unsafe { std::env::set_var(key, value) };
            Self { key, old_value }
        }

        fn remove(key: &'static str) -> Self {
            let old_value = std::env::var(key).ok();
            // SAFETY: Guarded by ENV_LOCK so tests mutate process env serially.
            unsafe { std::env::remove_var(key) };
            Self { key, old_value }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.old_value.as_ref() {
                Some(value) => {
                    // SAFETY: Guarded by ENV_LOCK so tests mutate process env serially.
                    unsafe { std::env::set_var(self.key, value) };
                }
                None => {
                    // SAFETY: Guarded by ENV_LOCK so tests mutate process env serially.
                    unsafe { std::env::remove_var(self.key) };
                }
            }
        }
    }

    #[test]
    fn npm_install_policy_rejects_sandbox_only_without_managed_registry() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _registry = EnvGuard::remove(CTX_MANAGED_NPM_REGISTRY_ENV);

        let err = validate_npm_install_policy("provider@1.0.0")
            .expect_err("sandbox-only without mirror should fail");
        assert!(
            err.to_string().contains(CTX_MANAGED_NPM_REGISTRY_ENV),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn npm_install_policy_accepts_sandbox_only_with_https_managed_registry() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _registry = EnvGuard::set(CTX_MANAGED_NPM_REGISTRY_ENV, "https://npm.corp.example/");

        let registry =
            validate_npm_install_policy("provider@1.0.0").expect("sandbox-only mirror should pass");
        assert_eq!(registry.as_deref(), Some("https://npm.corp.example"));
    }

    #[test]
    fn npm_install_policy_rejects_sandbox_only_direct_specs_even_with_managed_registry() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _registry = EnvGuard::set(CTX_MANAGED_NPM_REGISTRY_ENV, "https://npm.corp.example/");

        for package_spec in [
            "https://registry.npmjs.org/provider/-/provider-1.0.0.tgz",
            "git+ssh://git@github.com/company/provider.git",
            "github:company/provider",
            "company/provider",
            "@company/provider@https://registry.npmjs.org/provider/-/provider-1.0.0.tgz",
            "@company/provider@git+ssh://git@github.com/company/provider.git",
            "@company/provider@github:company/provider",
            "@company/provider@company/provider",
            "@company/provider@file:../provider",
            "@company/provider@../provider",
            "@company/provider@./provider",
            "@company/provider@/tmp/provider",
            "@company/provider@C:\\tmp\\provider",
            "@company/provider@provider.tgz",
            "file:../provider",
            "../provider",
            "./provider",
            "/tmp/provider",
            "C:\\tmp\\provider",
            "provider.tgz",
        ] {
            let err = validate_npm_install_policy(package_spec)
                .expect_err("direct npm spec should fail under sandbox-only");
            assert!(
                err.to_string().contains("direct npm package specs"),
                "unexpected error for {package_spec}: {err:#}"
            );
        }
    }

    #[test]
    fn npm_install_policy_rejects_option_like_specs() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _registry = EnvGuard::set(CTX_MANAGED_NPM_REGISTRY_ENV, "https://npm.corp.example/");

        for package_spec in [
            "--registry=https://registry.npmjs.org/provider",
            "--//registry.npmjs.org/:_authToken=secret",
            "-g",
        ] {
            let err = validate_npm_install_policy(package_spec)
                .expect_err("option-like npm spec should fail");
            assert!(
                err.to_string().contains("installer options"),
                "unexpected error for {package_spec}: {err:#}"
            );
        }
    }

    #[test]
    fn npm_install_policy_accepts_registry_aliases_with_https_managed_registry() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _registry = EnvGuard::set(CTX_MANAGED_NPM_REGISTRY_ENV, "https://npm.corp.example/");

        for package_spec in [
            "provider",
            "provider@1.0.0",
            "@company/provider@^1.2.3",
            "@company/provider",
            "provider-alias@npm:@company/provider@1.2.3",
        ] {
            validate_npm_install_policy(package_spec).unwrap_or_else(|err| {
                panic!("registry package should pass for {package_spec}: {err:#}")
            });
        }
    }

    #[test]
    fn npm_install_policy_rejects_non_https_managed_registry() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _registry = EnvGuard::set(CTX_MANAGED_NPM_REGISTRY_ENV, "http://npm.corp.example/");

        let err =
            validate_npm_install_policy("provider@1.0.0").expect_err("http mirror should fail");
        assert!(
            err.to_string().contains("https"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn npm_install_policy_allows_default_policy_without_managed_registry() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::remove(CTX_HOST_EXECUTION_POLICY_ENV);
        let _registry = EnvGuard::remove(CTX_MANAGED_NPM_REGISTRY_ENV);

        let registry =
            validate_npm_install_policy("https://registry.npmjs.org/provider/-/provider-1.0.0.tgz")
                .expect("default policy should pass");
        assert_eq!(registry, None);
    }

    #[test]
    fn pip_install_policy_rejects_sandbox_only_without_managed_index() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _index = EnvGuard::remove(CTX_MANAGED_PIP_INDEX_URL_ENV);

        let err = validate_pip_install_policy("provider==1.0.0")
            .expect_err("sandbox-only without mirror should fail");
        assert!(
            err.to_string().contains(CTX_MANAGED_PIP_INDEX_URL_ENV),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn pip_install_policy_rejects_sandbox_only_direct_specs_even_with_managed_index() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _index = EnvGuard::set(
            CTX_MANAGED_PIP_INDEX_URL_ENV,
            "https://pip.corp.example/simple",
        );

        for package_spec in [
            "https://files.pythonhosted.org/pkg.whl",
            "git+https://github.com/company/provider.git#egg=provider",
            "git+ssh://git@github.com/company/provider.git#egg=provider",
            "file:///tmp/provider.whl",
            "provider @ https://files.pythonhosted.org/pkg.whl",
            "provider @ git+https://github.com/company/provider.git",
            "provider@git+https://github.com/company/provider.git",
            "../provider",
            "./provider",
            "/tmp/provider",
            "C:\\tmp\\provider",
            "provider.whl",
        ] {
            let err = validate_pip_install_policy(package_spec)
                .expect_err("direct pip spec should fail under sandbox-only");
            assert!(
                err.to_string().contains("direct pip package specs"),
                "unexpected error for {package_spec}: {err:#}"
            );
        }
    }

    #[test]
    fn pip_install_policy_rejects_option_like_specs() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _index = EnvGuard::set(
            CTX_MANAGED_PIP_INDEX_URL_ENV,
            "https://pip.corp.example/simple",
        );

        for package_spec in [
            "--index-url=https://files.pythonhosted.org/simple",
            "--extra-index-url=https://files.pythonhosted.org/simple",
            "-r requirements.txt",
        ] {
            let err = validate_pip_install_policy(package_spec)
                .expect_err("option-like pip spec should fail");
            assert!(
                err.to_string().contains("installer options"),
                "unexpected error for {package_spec}: {err:#}"
            );
        }
    }

    #[test]
    fn pip_install_policy_accepts_sandbox_only_with_https_managed_index() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let _policy = EnvGuard::set(CTX_HOST_EXECUTION_POLICY_ENV, "sandbox_only");
        let _index = EnvGuard::set(
            CTX_MANAGED_PIP_INDEX_URL_ENV,
            "https://pip.corp.example/simple/",
        );

        let policy = validate_pip_install_policy("provider==1.0.0")
            .expect("sandbox-only mirror should pass");
        assert_eq!(
            policy.index_url.as_deref(),
            Some("https://pip.corp.example/simple")
        );
    }
}
