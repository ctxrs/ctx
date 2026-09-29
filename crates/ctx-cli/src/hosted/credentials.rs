use std::{
    fs::File,
    io::{IsTerminal, Read, Write},
    path::Path,
};

use anyhow::{bail, Context, Result};
use ctx_history_platform::platform_security::{
    create_private_file_new, verify_private_file_handle,
};

const MAX_CREDENTIAL_BYTES: u64 = 16 * 1024;

/// A dash explicitly selects piped stdin. File credentials must be owner-private.
pub(super) fn read(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            bail!("pipe the credential to stdin or use an owner-private credential file");
        }
        stdin
            .lock()
            .take(MAX_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("read credential from stdin")?;
    } else {
        open(path)?
            .take(MAX_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("read credential file")?;
    }
    if bytes.len() > MAX_CREDENTIAL_BYTES as usize {
        bail!("credential exceeds the supported size");
    }
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("credential must be UTF-8"))?;
    let token = if text.trim_start().starts_with('{') {
        // Read the server's bootstrap, enrollment, or issued-secret envelope.
        // Never include its JSON or decoder errors in diagnostics.
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("invalid credential file"))?;
        value
            .get("credential")
            .or_else(|| value.get("enrollment"))
            .unwrap_or(&value)
            .get("secret")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("credential file contains no secret"))?
            .to_owned()
    } else {
        text.trim_end_matches(['\r', '\n']).to_owned()
    };
    if token.is_empty()
        || token
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        bail!("credential must be one nonempty token");
    }
    Ok(token)
}

fn open(path: &Path) -> Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .context("open credential file")?
    };
    #[cfg(windows)]
    let file = ctx_history_platform::platform_security::open_verified_private_file(path)
        .context("open owner-private credential file")?;
    #[cfg(not(any(unix, windows)))]
    let file = File::open(path).context("open credential file")?;
    verify_private_file_handle(&file).context("credential file must be owner-private")?;
    Ok(file)
}

/// Reserve a new private output before issuing a credential. Never overwrite one.
pub(super) fn create(path: &Path) -> Result<File> {
    if path == Path::new("-") {
        bail!("issued credentials require an owner-private output file, not stdout");
    }
    create_private_file_new(path).context("create new owner-private credential file")
}

pub(super) fn write(file: &mut File, secret: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(&mut *file, secret).context("encode credential file")?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .context("save credential to its owner-private file")
}
