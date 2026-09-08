use std::io::Cursor;

use super::*;

const CUDA_LIBRARY: &str = "lib/libonnxruntime.so";

fn payload(relative: &str) -> Vec<u8> {
    format!("ctx-sidecar-fixture::{relative}").into_bytes()
}

fn contract_entries(flavor: OnnxRuntimeFlavor) -> Vec<(String, Vec<u8>)> {
    expected_runtime_files(flavor)
        .into_iter()
        .map(|relative| (relative.to_owned(), payload(relative)))
        .collect()
}

fn tar_header(name: &str, entry_type: tar::EntryType, size: u64) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(entry_type);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header.set_path(name).unwrap();
    header.set_cksum();
    header
}

/// Writes the archive name straight into the raw header so the fixture can
/// carry names `tar::Header::set_path` refuses to produce, such as absolute
/// paths and `..` traversal.
fn raw_named_tar_header(raw_name: &[u8], entry_type: tar::EntryType, size: u64) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(entry_type);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header.as_mut_bytes()[..raw_name.len()].copy_from_slice(raw_name);
    header.set_cksum();
    header
}

fn write_tar_zstd(path: &Path, headers: Vec<(tar::Header, Vec<u8>)>) -> String {
    let file = fs::File::create(path).unwrap();
    let encoder = zstd::stream::write::Encoder::new(file, 1).unwrap();
    let mut builder = tar::Builder::new(encoder);
    for (header, body) in headers {
        builder.append(&header, Cursor::new(body)).unwrap();
    }
    builder
        .into_inner()
        .unwrap()
        .finish()
        .unwrap()
        .sync_all()
        .unwrap();
    sha256_runtime_file(path).unwrap()
}

fn contract_headers(flavor: OnnxRuntimeFlavor) -> Vec<(tar::Header, Vec<u8>)> {
    let mut headers = vec![(tar_header("lib/", tar::EntryType::Directory, 0), Vec::new())];
    for (name, body) in contract_entries(flavor) {
        headers.push((
            tar_header(&name, tar::EntryType::Regular, body.len() as u64),
            body,
        ));
    }
    headers
}

struct Fixture {
    _temporary: tempfile::TempDir,
    runtime_root: PathBuf,
    archive: PathBuf,
    digest: String,
}

impl Fixture {
    fn contract(flavor: OnnxRuntimeFlavor) -> Self {
        Self::from_headers(contract_headers(flavor))
    }

    fn from_headers(headers: Vec<(tar::Header, Vec<u8>)>) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let runtime_root = temporary.path().join("runtime");
        let archive = temporary.path().join("ctx-onnxruntime-sidecar.tar.zst");
        let digest = write_tar_zstd(&archive, headers);
        Self {
            _temporary: temporary,
            runtime_root,
            archive,
            digest,
        }
    }

    fn request_asserting(&self, backend: SemanticRuntimeBackend) -> SemanticRuntimeInstall<'_> {
        SemanticRuntimeInstall {
            archive: &self.archive,
            expected_archive_sha256: &self.digest,
            runtime_root: &self.runtime_root,
            backend: Some(backend),
            replace_existing: false,
        }
    }

    /// The archive decides, which is how every real install is driven.
    fn request(&self) -> SemanticRuntimeInstall<'_> {
        SemanticRuntimeInstall {
            archive: &self.archive,
            expected_archive_sha256: &self.digest,
            runtime_root: &self.runtime_root,
            backend: None,
            replace_existing: false,
        }
    }

    fn installed_root(&self) -> PathBuf {
        self.runtime_root
            .join(RUNTIME_LAYOUT_DIR)
            .join(OnnxRuntimeFlavor::Cuda.version())
            .join("linux-x64-cuda12")
    }

    fn version_dir(&self) -> PathBuf {
        self.runtime_root
            .join(RUNTIME_LAYOUT_DIR)
            .join(OnnxRuntimeFlavor::Cuda.version())
    }
}

fn assert_nothing_published(fixture: &Fixture) {
    assert!(
        !fixture.installed_root().exists(),
        "a failed install published {}",
        fixture.installed_root().display()
    );
    if let Ok(entries) = fs::read_dir(fixture.version_dir()) {
        let leftovers = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "a failed install left scratch directories behind: {leftovers:?}"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn operator_install_publishes_a_runtime_the_loader_accepts() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let report = install_operator_runtime(&fixture.request()).unwrap();

    assert_eq!(report.backend, SemanticRuntimeBackend::Cuda);
    assert_eq!(report.platform, "linux-x64-cuda12");
    assert_eq!(report.version, "1.27.0");
    assert_eq!(report.root, fixture.installed_root());
    assert_eq!(report.library, fixture.installed_root().join(CUDA_LIBRARY));
    assert_eq!(report.archive_sha256, fixture.digest);
    assert_eq!(report.manager, "ctx-local-operator");
    assert_eq!(report.metadata_trust, "operator-pinned-digest");
    assert_eq!(
        report.files,
        expected_runtime_files(OnnxRuntimeFlavor::Cuda).len()
    );

    let identity = validate_runtime_candidate(&report.library, OnnxRuntimeFlavor::Cuda).unwrap();
    assert_eq!(identity, report.identity);
    assert!(
        identity.contains("manager=ctx-local-operator|metadata_trust=operator-pinned-digest"),
        "identity does not carry the operator trust tier: {identity}"
    );
    assert!(identity.contains(&format!("sha256={}", fixture.digest)));

    let reported = installed_runtime_report(&fixture.runtime_root, SemanticRuntimeBackend::Cuda)
        .unwrap()
        .expect("installed runtime is reportable");
    assert_eq!(reported, report);

    let body = fs::read_to_string(report.root.join(RUNTIME_INSTALL_MANIFEST)).unwrap();
    ctx_history_platform::platform_security::verify_private_directory(&report.root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::symlink_metadata(report.root.join("lib"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::symlink_metadata(&report.library)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }
    let manifest: serde_json::Value = serde_json::from_str(&body).unwrap();
    let object = manifest.as_object().unwrap();
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "artifact_url",
            "files",
            "installed_at",
            "manager",
            "metadata_trust",
            "platform",
            "runtime",
            "schema_version",
            "sha256",
            "version",
        ]
    );
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["manager"], "ctx-local-operator");
    assert_eq!(manifest["metadata_trust"], "operator-pinned-digest");
    assert_eq!(manifest["runtime"], "onnxruntime");
    assert_eq!(manifest["platform"], "linux-x64-cuda12");
    assert_eq!(manifest["version"], "1.27.0");
    assert_eq!(manifest["sha256"], fixture.digest);
    assert_eq!(
        manifest["artifact_url"],
        url::Url::from_file_path(&fixture.archive).unwrap().as_str()
    );
    let installed_at = manifest["installed_at"].as_str().unwrap();
    assert_eq!(installed_at.len(), 20, "{installed_at}");
    assert!(installed_at.ends_with('Z'), "{installed_at}");
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files.len(), report.files);
    for record in files {
        let relative = record["path"].as_str().unwrap();
        let body = payload(relative);
        assert_eq!(record["size"], body.len());
        assert_eq!(
            record["sha256"],
            format!("{:x}", Sha256::digest(&body)),
            "{relative}"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn installed_report_is_none_before_any_install() {
    let temporary = tempfile::tempdir().unwrap();
    assert!(installed_runtime_report(temporary.path(), SemanticRuntimeBackend::Cuda)
        .unwrap()
        .is_none());
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_a_mismatched_archive_digest_before_extracting() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let wrong = "0".repeat(64);
    let mut request = fixture.request();
    request.expected_archive_sha256 = &wrong;
    let error = install_operator_runtime(&request).unwrap_err().to_string();
    assert!(error.contains("not the pinned"), "{error}");
    assert!(
        !fixture.version_dir().exists(),
        "digest rejection created runtime directories"
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_a_digest_that_is_not_lowercase_hex() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    for pin in [
        fixture.digest.to_ascii_uppercase(),
        fixture.digest[..63].to_owned(),
        format!("{}z", &fixture.digest[..63]),
    ] {
        let mut request = fixture.request();
        request.expected_archive_sha256 = &pin;
        let error = install_operator_runtime(&request).unwrap_err().to_string();
        assert!(
            error.contains("64 lowercase hexadecimal characters"),
            "{pin} was not rejected: {error}"
        );
    }
    assert_nothing_published(&fixture);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_unsafe_archive_entries() {
    for (label, expected_error, header, body) in [
        (
            "symlink",
            "link, device, or other non-regular entry",
            {
                let mut header = tar_header("GIT_COMMIT_ID", tar::EntryType::Symlink, 0);
                header.set_link_name("/etc/passwd").unwrap();
                header.set_cksum();
                header
            },
            Vec::new(),
        ),
        (
            "hardlink",
            "link, device, or other non-regular entry",
            {
                let mut header = tar_header("GIT_COMMIT_ID", tar::EntryType::Link, 0);
                header.set_link_name("LICENSE").unwrap();
                header.set_cksum();
                header
            },
            Vec::new(),
        ),
        (
            "absolute",
            "unsafe sidecar archive entry \"/etc/ctx-escape\"",
            raw_named_tar_header(b"/etc/ctx-escape", tar::EntryType::Regular, 6),
            b"escape".to_vec(),
        ),
        (
            "traversal",
            "unsafe sidecar archive entry \"lib/../../ctx-escape\"",
            raw_named_tar_header(b"lib/../../ctx-escape", tar::EntryType::Regular, 6),
            b"escape".to_vec(),
        ),
        (
            "device",
            "link, device, or other non-regular entry",
            tar_header("GIT_COMMIT_ID", tar::EntryType::Char, 0),
            Vec::new(),
        ),
        (
            "fifo",
            "link, device, or other non-regular entry",
            tar_header("GIT_COMMIT_ID", tar::EntryType::Fifo, 0),
            Vec::new(),
        ),
    ] {
        let mut headers = vec![(tar_header("lib/", tar::EntryType::Directory, 0), Vec::new())];
        for (name, contents) in contract_entries(OnnxRuntimeFlavor::Cuda) {
            if name == "GIT_COMMIT_ID" && body.is_empty() {
                continue;
            }
            headers.push((
                tar_header(&name, tar::EntryType::Regular, contents.len() as u64),
                contents,
            ));
        }
        headers.push((header, body));
        let fixture = Fixture::from_headers(headers);
        let error = install_operator_runtime(&fixture.request())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected_error),
            "{label} entry produced an unexpected error: {error}"
        );
        assert_nothing_published(&fixture);
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_a_missing_contract_file() {
    let mut headers = vec![(tar_header("lib/", tar::EntryType::Directory, 0), Vec::new())];
    for (name, body) in contract_entries(OnnxRuntimeFlavor::Cuda) {
        if name == "lib/libonnxruntime_providers_cuda.so" {
            continue;
        }
        headers.push((
            tar_header(&name, tar::EntryType::Regular, body.len() as u64),
            body,
        ));
    }
    let fixture = Fixture::from_headers(headers);
    let error = install_operator_runtime(&fixture.request())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("missing [lib/libonnxruntime_providers_cuda.so]"),
        "{error}"
    );
    assert_nothing_published(&fixture);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_an_extra_archive_file() {
    let mut headers = vec![(tar_header("lib/", tar::EntryType::Directory, 0), Vec::new())];
    for (name, body) in contract_entries(OnnxRuntimeFlavor::Cuda) {
        headers.push((
            tar_header(&name, tar::EntryType::Regular, body.len() as u64),
            body,
        ));
    }
    headers.push((
        tar_header("lib/libextra.so", tar::EntryType::Regular, 5),
        b"extra".to_vec(),
    ));
    let fixture = Fixture::from_headers(headers);
    let error = install_operator_runtime(&fixture.request())
        .unwrap_err()
        .to_string();
    assert!(error.contains("has [lib/libextra.so] unexpected"), "{error}");
    assert_nothing_published(&fixture);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_a_stray_archive_directory() {
    let mut headers = vec![
        (tar_header("lib/", tar::EntryType::Directory, 0), Vec::new()),
        (
            tar_header("plugins/", tar::EntryType::Directory, 0),
            Vec::new(),
        ),
    ];
    for (name, body) in contract_entries(OnnxRuntimeFlavor::Cuda) {
        headers.push((
            tar_header(&name, tar::EntryType::Regular, body.len() as u64),
            body,
        ));
    }
    let fixture = Fixture::from_headers(headers);
    let error = install_operator_runtime(&fixture.request())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unexpected sidecar archive directory \"plugins\""),
        "{error}"
    );
    assert_nothing_published(&fixture);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_refuses_to_overwrite_without_the_replace_option() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let first = install_operator_runtime(&fixture.request()).unwrap();

    let error = install_operator_runtime(&fixture.request())
        .unwrap_err()
        .to_string();
    assert!(error.contains("already installed at"), "{error}");
    assert_eq!(
        validate_runtime_candidate(&first.library, OnnxRuntimeFlavor::Cuda).unwrap(),
        first.identity,
        "the refused install disturbed the existing runtime"
    );

    let mut replacement = fixture.request();
    replacement.replace_existing = true;
    let second = install_operator_runtime(&replacement).unwrap();
    assert_eq!(second.root, first.root);
    assert_eq!(
        validate_runtime_candidate(&second.library, OnnxRuntimeFlavor::Cuda).unwrap(),
        second.identity
    );
    let leftovers = fs::read_dir(fixture.version_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "linux-x64-cuda12")
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "replacing left scratch trees behind: {leftovers:?}"
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn loader_rejects_a_mixed_manager_and_metadata_trust_pair() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let report = install_operator_runtime(&fixture.request()).unwrap();
    let manifest_path = report.root.join(RUNTIME_INSTALL_MANIFEST);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();

    for (manager, metadata_trust) in [
        ("ctx-local-operator", "signed-release-metadata"),
        ("ctx-hosted-installer", "operator-pinned-digest"),
        ("ctx-local-operator", "ctx-local-operator"),
    ] {
        manifest["manager"] = serde_json::json!(manager);
        manifest["metadata_trust"] = serde_json::json!(metadata_trust);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = validate_runtime_candidate(&report.library, OnnxRuntimeFlavor::Cuda)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("pairs manager"),
            "{manager}/{metadata_trust} was not rejected as a pair: {error}"
        );
        assert!(installed_runtime_report(&fixture.runtime_root, SemanticRuntimeBackend::Cuda)
            .is_err());
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_an_archive_that_is_not_a_supported_container() {
    let temporary = tempfile::tempdir().unwrap();
    let archive = temporary.path().join("sidecar.bin");
    fs::write(&archive, b"not-an-archive-at-all").unwrap();
    let digest = sha256_runtime_file(&archive).unwrap();
    let runtime_root = temporary.path().join("runtime");
    let error = install_operator_runtime(&SemanticRuntimeInstall {
        archive: &archive,
        expected_archive_sha256: &digest,
        runtime_root: &runtime_root,
        backend: None,
        replace_existing: false,
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("unsupported sidecar archive format"), "{error}");
    assert!(!runtime_root.join(RUNTIME_LAYOUT_DIR).exists());
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_a_relative_runtime_root() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let relative = PathBuf::from("relative-runtime-root");
    let mut request = fixture.request();
    request.runtime_root = &relative;
    let error = install_operator_runtime(&request).unwrap_err().to_string();
    assert!(error.contains("is not absolute"), "{error}");
    assert!(!relative.exists());
}

#[test]
fn installed_report_requires_a_platform_the_backend_supports() {
    let temporary = tempfile::tempdir().unwrap();
    let unsupported = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        SemanticRuntimeBackend::Cuda
    } else {
        SemanticRuntimeBackend::WindowsMl
    };
    let error = installed_runtime_report(temporary.path(), unsupported)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("has no sidecar for"),
        "unsupported backend error is not specific: {error}"
    );
}

#[test]
fn contract_directories_cover_every_parent() {
    let files = contract_files(OnnxRuntimeFlavor::Cuda);
    assert_eq!(
        contract_directories(&files),
        BTreeSet::from(["lib".to_owned()])
    );
    let windows = contract_files(OnnxRuntimeFlavor::WindowsMl);
    assert_eq!(
        contract_directories(&windows),
        BTreeSet::from(["lib".to_owned()])
    );
    let cpu = contract_files(OnnxRuntimeFlavor::Cpu);
    assert_eq!(
        contract_directories(&cpu),
        BTreeSet::from(["lib".to_owned()])
    );
}

#[test]
fn utc_stamps_are_rfc3339_seconds() {
    assert_eq!(format_rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(format_rfc3339_utc(1_780_000_000), "2026-05-28T20:26:40Z");
    assert_eq!(format_rfc3339_utc(1_767_225_599), "2025-12-31T23:59:59Z");
    let now = rfc3339_utc_now();
    assert_eq!(now.len(), 20, "{now}");
    assert!(now.ends_with('Z'), "{now}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn operator_install_publishes_a_cpu_runtime_the_loader_accepts() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cpu);
    let report = install_operator_runtime(&fixture.request()).unwrap();

    assert_eq!(report.backend, SemanticRuntimeBackend::Cpu);
    assert_eq!(report.platform, "linux-x64");
    assert_eq!(report.version, "1.27.0");
    // The published CPU sidecar carries four documents and one library.
    assert_eq!(report.files, 5);
    let installed_root = fixture
        .runtime_root
        .join(RUNTIME_LAYOUT_DIR)
        .join(OnnxRuntimeFlavor::Cpu.version())
        .join("linux-x64");
    assert_eq!(report.root, installed_root);
    assert_eq!(
        report.library,
        installed_root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB)
    );
    assert_eq!(report.archive_sha256, fixture.digest);
    assert_eq!(report.manager, "ctx-local-operator");
    assert_eq!(report.metadata_trust, "operator-pinned-digest");

    let identity = validate_runtime_candidate(&report.library, OnnxRuntimeFlavor::Cpu).unwrap();
    assert_eq!(identity, report.identity);
    assert!(
        identity.contains("manager=ctx-local-operator|metadata_trust=operator-pinned-digest"),
        "identity does not carry the operator trust tier: {identity}"
    );
    assert!(identity.contains(&format!("sha256={}", fixture.digest)));

    let reported = installed_runtime_report(&fixture.runtime_root, SemanticRuntimeBackend::Cpu)
        .unwrap()
        .expect("the installed CPU runtime is reportable");
    assert_eq!(reported, report);
    // Each backend owns its own platform directory, so provisioning the CPU
    // runtime must not make the accelerator look installed.
    assert!(
        installed_runtime_report(&fixture.runtime_root, SemanticRuntimeBackend::Cuda)
            .unwrap()
            .is_none(),
        "the CPU install published a CUDA runtime"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(report.root.join(RUNTIME_INSTALL_MANIFEST)).unwrap())
            .unwrap();
    assert_eq!(manifest["runtime"], "onnxruntime");
    assert_eq!(manifest["platform"], "linux-x64");
    assert_eq!(manifest["manager"], "ctx-local-operator");
    assert_eq!(manifest["metadata_trust"], "operator-pinned-digest");
    assert_eq!(manifest["files"].as_array().unwrap().len(), 5);
}

#[test]
fn local_install_refuses_the_zip_only_runtimes() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cpu);
    let error =
        install_operator_runtime(&fixture.request_asserting(SemanticRuntimeBackend::WindowsMl))
            .unwrap_err()
            .to_string();
    assert!(error.contains("Windows ML"), "{error}");
    assert!(error.contains("hosted installer"), "{error}");
    if cfg!(target_os = "windows") {
        let error =
            install_operator_runtime(&fixture.request_asserting(SemanticRuntimeBackend::Cpu))
                .unwrap_err()
                .to_string();
        assert!(error.contains("hosted installer"), "{error}");
    }
}

#[test]
fn supported_local_backends_lead_with_the_cpu_runtime() {
    let backends = crate::supported_local_runtime_backends();
    if cfg!(target_os = "windows") {
        assert!(backends.is_empty(), "{backends:?}");
        return;
    }
    assert_eq!(backends.first(), Some(&SemanticRuntimeBackend::Cpu));
    assert!(!backends.contains(&SemanticRuntimeBackend::WindowsMl));
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(
            backends,
            [SemanticRuntimeBackend::Cpu, SemanticRuntimeBackend::Cuda]
        );
    }
    // Advertising a backend this platform has no sidecar for would send an
    // operator after an install that cannot resolve a platform directory.
    for backend in backends {
        assert!(
            backend.flavor().platform_dir().is_ok(),
            "{backend:?} is advertised without a sidecar platform"
        );
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn identify_reads_the_backend_out_of_the_archive_contents() {
    let cuda = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let cpu = Fixture::contract(OnnxRuntimeFlavor::Cpu);

    assert_eq!(
        identify_runtime_archive(&cuda.archive).unwrap(),
        SemanticRuntimeBackend::Cuda
    );
    assert_eq!(
        identify_runtime_archive(&cpu.archive).unwrap(),
        SemanticRuntimeBackend::Cpu
    );
    // Identification is a read: it must not create or touch a runtime root.
    assert!(!cuda.runtime_root.exists());
    assert!(!cpu.runtime_root.exists());
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn identify_rejects_an_archive_matching_no_contract() {
    let mut truncated = vec![(tar_header("lib/", tar::EntryType::Directory, 0), Vec::new())];
    for (name, body) in contract_entries(OnnxRuntimeFlavor::Cuda) {
        if name == "lib/libonnxruntime_providers_cuda.so" {
            continue;
        }
        truncated.push((
            tar_header(&name, tar::EntryType::Regular, body.len() as u64),
            body,
        ));
    }
    let truncated = Fixture::from_headers(truncated);
    // The CLI prints the whole context chain, so that is what is asserted here.
    let error = format!(
        "{:#}",
        identify_runtime_archive(&truncated.archive).unwrap_err()
    );
    assert!(
        error.contains("match no semantic runtime contract"),
        "{error}"
    );
    assert!(
        error.contains(truncated.archive.to_str().unwrap()),
        "the error does not name the archive: {error}"
    );

    let mut extra = contract_headers(OnnxRuntimeFlavor::Cpu);
    extra.push((
        tar_header("lib/libextra.so", tar::EntryType::Regular, 5),
        b"extra".to_vec(),
    ));
    let extra = Fixture::from_headers(extra);
    let error = format!("{:#}", identify_runtime_archive(&extra.archive).unwrap_err());
    assert!(
        error.contains("match no semantic runtime contract"),
        "{error}"
    );
    assert!(error.contains("lib/libextra.so"), "{error}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn identify_reports_the_closest_contract_and_what_differs() {
    let mut headers = vec![(tar_header("lib/", tar::EntryType::Directory, 0), Vec::new())];
    for (name, body) in contract_entries(OnnxRuntimeFlavor::Cpu) {
        if name == "VERSION_NUMBER" {
            continue;
        }
        headers.push((
            tar_header(&name, tar::EntryType::Regular, body.len() as u64),
            body,
        ));
    }
    let fixture = Fixture::from_headers(headers);

    let error = format!("{:#}", identify_runtime_archive(&fixture.archive).unwrap_err());

    assert!(error.contains("closest contract (cpu)"), "{error}");
    assert!(error.contains("missing [VERSION_NUMBER]"), "{error}");
    assert!(error.contains("has nothing unexpected"), "{error}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_infers_each_backend_from_its_own_archive() {
    let cuda = Fixture::contract(OnnxRuntimeFlavor::Cuda);
    let report = install_operator_runtime(&cuda.request()).unwrap();
    assert_eq!(report.backend, SemanticRuntimeBackend::Cuda);
    assert_eq!(report.platform, "linux-x64-cuda12");
    assert_eq!(
        report.files,
        expected_runtime_files(OnnxRuntimeFlavor::Cuda).len()
    );

    let cpu = Fixture::contract(OnnxRuntimeFlavor::Cpu);
    let report = install_operator_runtime(&cpu.request()).unwrap();
    assert_eq!(report.backend, SemanticRuntimeBackend::Cpu);
    assert_eq!(report.platform, "linux-x64");
    assert_eq!(report.files, 5);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn install_rejects_a_backend_the_archive_contradicts() {
    let fixture = Fixture::contract(OnnxRuntimeFlavor::Cuda);

    let error = install_operator_runtime(&fixture.request_asserting(SemanticRuntimeBackend::Cpu))
        .unwrap_err()
        .to_string();

    // Both sides have to be named: the operator retyped one of them.
    assert!(error.contains("requested cpu runtime"), "{error}");
    assert!(error.contains("carries the cuda runtime"), "{error}");
    assert_nothing_published(&fixture);
    assert!(
        install_operator_runtime(&fixture.request_asserting(SemanticRuntimeBackend::Cuda)).is_ok(),
        "the matching assertion must still install"
    );
}

#[test]
fn detected_accelerator_names_only_an_installable_sidecar_backend() {
    if let Some(backend) = crate::detected_accelerator_backend() {
        assert_ne!(
            backend,
            SemanticRuntimeBackend::Cpu,
            "the CPU runtime is not an accelerator"
        );
        // Guidance derived from this must never point at a backend with no
        // sidecar for the running platform.
        backend
            .flavor()
            .platform_dir()
            .expect("detected accelerator has no sidecar platform");
    }
    // Core ML is an execution provider of the OS-supplied CPU runtime, not an
    // installable sidecar, so macOS has no accelerator to report.
    if cfg!(target_os = "macos") {
        assert_eq!(crate::detected_accelerator_backend(), None);
    }
}
