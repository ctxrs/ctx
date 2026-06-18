use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;

mod dev_instance_identity {
    include!("../../../build-support/dev_instance_identity.rs");
}

fn main() {
    let build_identity = emit_build_identity();
    ensure_desktop_bundle_metadata(&build_identity);
    emit_embedded_updater_pubkey();
    if env::var_os("CARGO_FEATURE_STT").is_some() {
        ensure_vosk_runtime();
    }

    println!("cargo:rerun-if-env-changed=CTX_DESKTOP_SKIP_TAURI_BUILD");
    let skip_tauri_build = env::var_os("CTX_DESKTOP_SKIP_TAURI_BUILD")
        .map(|value| value != "0")
        .unwrap_or(false);
    if skip_tauri_build {
        println!("cargo:warning=skipping tauri-build because CTX_DESKTOP_SKIP_TAURI_BUILD is set");
        return;
    }

    tauri_build::build()
}

struct BuildIdentity {
    exact_version: String,
    build_id: String,
    compatibility_token: String,
}

fn emit_build_identity() -> BuildIdentity {
    println!("cargo:rerun-if-env-changed=CTX_BUILD_ID");
    println!("cargo:rerun-if-env-changed=CTX_COMPATIBILITY_TOKEN");
    println!("cargo:rerun-if-env-changed=CTX_DEV_INSTANCE_ID");
    println!("cargo:rerun-if-env-changed=CTX_DEV_INSTANCE_ROOT");
    println!("cargo:rerun-if-env-changed=CTX_RELEASE_EFFECTIVE_VERSION");
    println!("cargo:rerun-if-env-changed=RELEASE_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    emit_git_rerun_hints(&manifest_dir);

    let exact_version = explicit_env_value("CTX_RELEASE_EFFECTIVE_VERSION")
        .or_else(|| explicit_env_value("RELEASE_VERSION"))
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string()));
    println!("cargo:rustc-env=CTX_RELEASE_EFFECTIVE_VERSION={exact_version}");

    let compatibility_token = explicit_env_value("CTX_COMPATIBILITY_TOKEN")
        .or_else(|| explicit_env_value("CTX_DEV_INSTANCE_ID"))
        .unwrap_or_else(|| dev_instance_identity::resolve_dev_instance_id(&manifest_dir));
    println!("cargo:rustc-env=CTX_COMPATIBILITY_TOKEN={compatibility_token}");
    println!("cargo:rustc-env=CTX_DEV_INSTANCE_ID={compatibility_token}");

    let build_id = env::var("CTX_BUILD_ID")
        .ok()
        .map(|explicit| explicit.trim().to_string())
        .filter(|trimmed| !trimmed.is_empty())
        .unwrap_or_else(|| {
            git_head_build_id(&manifest_dir).unwrap_or_else(|| {
                env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string())
            })
        });
    println!("cargo:rustc-env=CTX_BUILD_ID={build_id}");

    BuildIdentity {
        exact_version,
        build_id,
        compatibility_token,
    }
}

fn explicit_env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn ensure_desktop_bundle_metadata(identity: &BuildIdentity) {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let bundles_dir = manifest_dir.join("bundles");
    fs::create_dir_all(&bundles_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create desktop bundle metadata dir '{}': {err}",
            bundles_dir.display()
        )
    });

    let provider_matrix_src = manifest_dir
        .join("../../..")
        .join("crates/ctx-provider-accounts/src/provider_matrix.json");
    println!("cargo:rerun-if-changed={}", provider_matrix_src.display());
    let provider_matrix_raw = fs::read(&provider_matrix_src).unwrap_or_else(|err| {
        panic!(
            "failed to read canonical provider matrix '{}': {err}",
            provider_matrix_src.display()
        )
    });
    write_if_different(
        &bundles_dir.join("provider_matrix.json"),
        &provider_matrix_raw,
    )
    .unwrap_or_else(|err| panic!("failed to write bundled provider matrix: {err}"));

    let artifact_identity = format!(
        concat!(
            "{{\n",
            "  \"schemaVersion\": 1,\n",
            "  \"exactVersion\": \"{}\",\n",
            "  \"buildId\": \"{}\",\n",
            "  \"compatibilityToken\": \"{}\"\n",
            "}}\n"
        ),
        json_escape(&identity.exact_version),
        json_escape(&identity.build_id),
        json_escape(&identity.compatibility_token)
    );
    write_if_different(
        &bundles_dir.join("artifact_identity.json"),
        artifact_identity.as_bytes(),
    )
    .unwrap_or_else(|err| panic!("failed to write desktop artifact identity: {err}"));
}

fn write_if_different(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    if fs::read(path)
        .map(|existing| existing == contents)
        .unwrap_or(false)
    {
        return Ok(());
    }
    fs::write(path, contents)
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn emit_embedded_updater_pubkey() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let pubkey_path = manifest_dir.join("config").join("updater_pubkey.txt");
    println!("cargo:rerun-if-changed={}", pubkey_path.display());
    let raw = fs::read_to_string(&pubkey_path).unwrap_or_else(|err| {
        panic!(
            "failed to read embedded updater pubkey '{}': {err}",
            pubkey_path.display()
        )
    });
    let normalized = normalize_minisign_pubkey_text(&raw).unwrap_or_else(|| {
        panic!(
            "embedded updater pubkey '{}' is invalid; expected normalized minisign public key text",
            pubkey_path.display()
        )
    });
    let encoded = BASE64_STANDARD.encode(normalized.as_bytes());
    println!("cargo:rustc-env=CTX_DESKTOP_EMBEDDED_UPDATER_PUBKEY_B64={encoded}");
}

fn normalize_minisign_pubkey_text(raw: &str) -> Option<String> {
    let normalized = raw.replace("\r\n", "\n");
    let mut lines = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let header = lines.next()?;
    if !header.starts_with("untrusted comment: minisign public key:") {
        return None;
    }
    let key_line = lines.next()?;
    if key_line.is_empty() || lines.next().is_some() {
        return None;
    }
    Some(format!("{header}\n{key_line}\n"))
}

fn git_head_build_id(cwd: &Path) -> Option<String> {
    let head = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !head.status.success() {
        return None;
    }
    let mut id = String::from_utf8(head.stdout).ok()?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let dirty = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|out| out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty())
        .unwrap_or(false);
    if dirty {
        id.push_str("-dirty");
    }
    Some(id)
}

fn emit_git_rerun_hints(cwd: &Path) {
    let git_dir_out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output();
    let Ok(git_dir_out) = git_dir_out else {
        return;
    };
    if !git_dir_out.status.success() {
        return;
    }
    let git_dir_raw = String::from_utf8_lossy(&git_dir_out.stdout)
        .trim()
        .to_string();
    if git_dir_raw.is_empty() {
        return;
    }
    let git_dir = {
        let p = PathBuf::from(&git_dir_raw);
        if p.is_absolute() {
            p
        } else {
            cwd.join(p)
        }
    };
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
}

fn ensure_vosk_runtime() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    println!("cargo:rerun-if-env-changed=CTX_VOSK_LIBVOSK_PATH");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    let override_libvosk = env::var_os("CTX_VOSK_LIBVOSK_PATH").map(PathBuf::from);
    let runtime = if let Some(override_path) = override_libvosk {
        runtime_from_override(&target_os, &override_path)
    } else {
        fetch_vosk_runtime(&manifest_dir, &target_os, &target_arch, &target_env)
    };

    // Linker: make Vosk discoverable at build time.
    println!(
        "cargo:rustc-link-search=native={}",
        runtime.link_dir.display()
    );

    match target_os.as_str() {
        "linux" => {
            // Runtime: prefer local copies (dev) and bundled copies (Tauri Linux bundles).
            // - `$ORIGIN` covers local builds where we copy libvosk next to the binary.
            // - `$ORIGIN/../lib/ctx/bin` covers typical Tauri bundle layouts where `bin/*` resources
            //   are staged under the app's lib directory.
            // - `$ORIGIN/../lib/ctx` is a fallback for bundles that place resources one level higher.
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/ctx/bin");
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/ctx");
        }
        "macos" => {
            // Tauri bundles resources under:
            //   ctx.app/Contents/Resources/...
            // The main binary lives under:
            //   ctx.app/Contents/MacOS/ctx
            // Prefer local dev copies (next to the binary) and bundled copies (Resources/bin).
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
            println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Resources/bin");
        }
        "windows" => {
            // On Windows, we delay-load libvosk.dll so the app can configure DLL search paths
            // before the first STT call, and so that bundling layouts can vary safely.
            if target_env != "msvc" {
                panic!(
                    "STT runtime bundling currently supports Windows MSVC targets only (got target env: {target_env}). \
Set CTX_VOSK_LIBVOSK_PATH to a compatible Vosk runtime to override."
                );
            }
            let import_lib = runtime.link_dir.join("libvosk.lib");
            if !import_lib.exists() {
                panic!(
                    "missing Windows import library {} (required to link STT; provide it next to libvosk.dll)",
                    import_lib.display()
                );
            }
            println!("cargo:rustc-link-arg=/DELAYLOAD:libvosk.dll");
            println!("cargo:rustc-link-lib=delayimp");
        }
        _ => {
            // Non-desktop or unsupported targets.
            return;
        }
    }

    // Bundle resources: ensure runtime libs are staged under src-tauri/bin so Tauri can include them.
    let bundle_bin_dir = manifest_dir.join("bin");
    fs::create_dir_all(&bundle_bin_dir).expect("create src-tauri/bin");
    for runtime_file in &runtime.runtime_files {
        let src = runtime.link_dir.join(runtime_file);
        if !src.exists() {
            panic!("missing Vosk runtime file at {}", src.display());
        }
        let dst = bundle_bin_dir.join(runtime_file);
        copy_if_different(&src, &dst)
            .unwrap_or_else(|e| panic!("failed to copy {} into src-tauri/bin: {e}", src.display()));
    }

    // Local dev runs (e.g. tauri-driver pointing at target/debug/ctx) expect the runtime library
    // to exist next to the binary when `stt` is enabled.
    let local_target_dir = manifest_dir.join("target").join(&profile);
    fs::create_dir_all(&local_target_dir).expect("create src-tauri/target/<profile>");
    for runtime_file in &runtime.runtime_files {
        let src = runtime.link_dir.join(runtime_file);
        let dst = local_target_dir.join(runtime_file);
        copy_if_different(&src, &dst).unwrap_or_else(|e| {
            panic!(
                "failed to copy {} next to target binary: {e}",
                src.display()
            )
        });
    }
}

struct VoskRuntime {
    link_dir: PathBuf,
    runtime_files: Vec<String>,
}

fn runtime_from_override(target_os: &str, override_path: &Path) -> VoskRuntime {
    let link_dir = override_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut runtime_files = Vec::new();
    if let Some(name) = override_path.file_name().and_then(|s| s.to_str()) {
        if !name.is_empty() {
            runtime_files.push(name.to_string());
        }
    }

    // When overriding on Windows, also bundle the known runtime deps if present.
    if target_os == "windows" {
        for dep in [
            "libstdc++-6.dll",
            "libwinpthread-1.dll",
            "libgcc_s_seh-1.dll",
        ] {
            if link_dir.join(dep).exists() {
                runtime_files.push(dep.to_string());
            }
        }
    }

    VoskRuntime {
        link_dir,
        runtime_files,
    }
}

fn fetch_vosk_runtime(
    manifest_dir: &Path,
    target_os: &str,
    target_arch: &str,
    target_env: &str,
) -> VoskRuntime {
    const VOSK_VERSION: &str = "0.3.42";

    let (zip_name, url, expected_sha256, extract_files, runtime_files) = match target_os {
        "linux" => match target_arch {
            "x86_64" => (
                "vosk-linux-x86_64-0.3.42.zip",
                "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/vosk/0.3.42/linux/x86_64/sha256/70480495011a29f957c1194cd460449ef7de8c17ea000e387ddb13fd7f844d42/vosk-linux-x86_64-0.3.42.zip",
                "70480495011a29f957c1194cd460449ef7de8c17ea000e387ddb13fd7f844d42",
                vec!["libvosk.so"],
                vec!["libvosk.so"],
            ),
            "aarch64" => (
                "vosk-linux-aarch64-0.3.42.zip",
                "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/vosk/0.3.42/linux/aarch64/sha256/e53fea373d591722b40c31449994854d54792457ad0ec7807428dca60549dbac/vosk-linux-aarch64-0.3.42.zip",
                "e53fea373d591722b40c31449994854d54792457ad0ec7807428dca60549dbac",
                vec!["libvosk.so"],
                vec!["libvosk.so"],
            ),
            other => {
                panic!(
                    "unsupported Linux arch for bundled Vosk runtime: {other} (set CTX_VOSK_LIBVOSK_PATH to override)"
                );
            }
        },
        "macos" => (
            "vosk-osx-0.3.42.zip",
            "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/vosk/0.3.42/macos/universal/sha256/65395f196c9d0583d79949142b25560acaf9c295f36284e18433097f3adb0ea1/vosk-osx-0.3.42.zip",
            "65395f196c9d0583d79949142b25560acaf9c295f36284e18433097f3adb0ea1",
            vec!["libvosk.dylib"],
            vec!["libvosk.dylib"],
        ),
        "windows" => {
            if target_arch != "x86_64" || target_env != "msvc" {
                panic!(
                    "unsupported Windows target for bundled Vosk runtime: arch={target_arch} env={target_env} \
(set CTX_VOSK_LIBVOSK_PATH to override)"
                );
            }
            (
                "vosk-win64-0.3.42.zip",
                "https://api.ctx.rs/storage/v1/object/public/releases/runtimes/vosk/0.3.42/windows/x86_64/sha256/9a63e42bd970343041d19e784e545228d3f4703ccec9f2eb1ccc6d5e96c170c3/vosk-win64-0.3.42.zip",
                "9a63e42bd970343041d19e784e545228d3f4703ccec9f2eb1ccc6d5e96c170c3",
                vec![
                    "libvosk.dll",
                    "libstdc++-6.dll",
                    "libwinpthread-1.dll",
                    "libgcc_s_seh-1.dll",
                    "libvosk.lib",
                ],
                vec![
                    "libvosk.dll",
                    "libstdc++-6.dll",
                    "libwinpthread-1.dll",
                    "libgcc_s_seh-1.dll",
                ],
            )
        }
        other => {
            panic!(
                "unsupported target OS for bundled Vosk runtime: {other} (set CTX_VOSK_LIBVOSK_PATH to override)"
            );
        }
    };

    // Cache under src-tauri/target so it stays out of git and is easy to clean.
    let cache_dir = manifest_dir
        .join("target")
        .join("vendor")
        .join("vosk")
        .join(VOSK_VERSION)
        .join(target_os)
        .join(target_arch);
    fs::create_dir_all(&cache_dir).expect("create Vosk cache dir");

    let zip_path = cache_dir.join(zip_name);
    if !zip_path.exists() {
        download_to_file(url, &zip_path);
    }

    let actual_sha256 = sha256_file_hex(&zip_path).expect("sha256 zip");
    if actual_sha256 != expected_sha256 {
        let _ = fs::remove_file(&zip_path);
        panic!("unexpected sha256 for {zip_name}: expected {expected_sha256}, got {actual_sha256}");
    }

    for file in &extract_files {
        let out_path = cache_dir.join(file);
        if out_path.exists() {
            continue;
        }
        extract_zip_member_ending_with(&zip_path, file, &out_path).unwrap_or_else(|e| {
            panic!("failed extracting {file} from {zip_name}: {e:?}");
        });
    }

    // Ensure the dylib has a stable install name for @rpath-based lookup.
    if target_os == "macos" {
        let dylib = cache_dir.join("libvosk.dylib");
        if dylib.exists() {
            // build.rs runs on the host; don't assume install_name_tool exists for cross builds.
            if cfg!(target_os = "macos") {
                let status = std::process::Command::new("install_name_tool")
                    .args(["-id", "@rpath/libvosk.dylib"])
                    .arg(&dylib)
                    .status()
                    .expect("failed to run install_name_tool (install Xcode command line tools)");
                if !status.success() {
                    panic!("install_name_tool failed for {}", dylib.display());
                }
            }
        }
    }

    VoskRuntime {
        link_dir: cache_dir,
        runtime_files: runtime_files.into_iter().map(|s| s.to_string()).collect(),
    }
}

fn download_to_file(url: &str, path: &Path) {
    eprintln!("downloading {url} → {}", path.display());

    let response = ureq::get(url)
        .call()
        .unwrap_or_else(|e| panic!("failed to download {url}: {e}"));

    let mut reader = response.into_reader();
    let mut out = fs::File::create(path)
        .unwrap_or_else(|e| panic!("failed to create {} for download: {e}", path.display()));

    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).unwrap_or_else(|e| {
            panic!("failed reading download stream for {url}: {e}");
        });
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .unwrap_or_else(|e| panic!("failed writing {}: {e}", path.display()));
    }
}

fn extract_zip_member_ending_with(
    zip_path: &Path,
    suffix: &str,
    out_path: &Path,
) -> zip::result::ZipResult<()> {
    let f = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(f)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        if name.ends_with(suffix) {
            let mut out = fs::File::create(out_path)?;
            std::io::copy(&mut file, &mut out)?;
            return Ok(());
        }
    }

    Err(zip::result::ZipError::FileNotFound)
}

fn sha256_file_hex(path: &Path) -> Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};

    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    Ok(out)
}

fn copy_if_different(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    let src_meta = fs::metadata(src)?;
    if let Ok(dst_meta) = fs::metadata(dst) {
        if src_meta.len() == dst_meta.len() {
            // Avoid re-copying large libs when unchanged.
            return Ok(());
        }
    }
    fs::copy(src, dst)?;
    Ok(())
}
