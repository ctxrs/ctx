//! Subprocess driver for the library entry point; no additional product binary.
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, Write};
use std::process::{Command, Output};

const MARKER: &[u8] = b"CTX-OUTPUT-TEST-ENTRY\n";
const ARGS: &str = "CTX_OUTPUT_TEST_ARGV";

pub(crate) fn command<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args.into_iter().map(|s| s.as_ref().to_owned()).collect();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "test_support::entry", "--nocapture"])
        .env(ARGS, encode(&args))
        .env_remove("CTX_OUTPUT_CONFIG_DIR")
        .env_remove("CTX_OUTPUT_STATE_DIR")
        .env_remove("TYPESAFE_API_KEY");
    command
}

pub(crate) fn clean(mut output: Output) -> Output {
    if let Some(index) = output
        .stdout
        .windows(MARKER.len())
        .position(|s| s == MARKER)
    {
        output.stdout.drain(..index + MARKER.len());
    }
    output
}

pub(crate) fn skip_header(reader: &mut impl BufRead) {
    let mut line = Vec::new();
    loop {
        line.clear();
        assert_ne!(
            reader.read_until(b'\n', &mut line).unwrap(),
            0,
            "missing driver marker"
        );
        if line.ends_with(MARKER) {
            break;
        }
    }
}

pub(crate) fn close_stdout(child: &mut std::process::Child) {
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    skip_header(&mut reader);
    drop(reader);
}

#[cfg(unix)]
fn encode(args: &[OsString]) -> String {
    use std::os::unix::ffi::OsStrExt;
    serde_json::to_string(&args.iter().map(|a| a.as_bytes()).collect::<Vec<_>>()).unwrap()
}

#[cfg(unix)]
fn decode(encoded: &str) -> Vec<OsString> {
    use std::os::unix::ffi::OsStringExt;
    serde_json::from_str::<Vec<Vec<u8>>>(encoded)
        .unwrap()
        .into_iter()
        .map(OsString::from_vec)
        .collect()
}

#[cfg(windows)]
fn encode(args: &[OsString]) -> String {
    use std::os::windows::ffi::OsStrExt;
    serde_json::to_string(
        &args
            .iter()
            .map(|a| a.encode_wide().collect::<Vec<_>>())
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[cfg(windows)]
fn decode(encoded: &str) -> Vec<OsString> {
    use std::os::windows::ffi::OsStringExt;
    serde_json::from_str::<Vec<Vec<u16>>>(encoded)
        .unwrap()
        .into_iter()
        .map(|a| OsString::from_wide(&a))
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn encode(args: &[OsString]) -> String {
    serde_json::to_string(&args.iter().map(|a| a.to_str().unwrap()).collect::<Vec<_>>()).unwrap()
}

#[cfg(not(any(unix, windows)))]
fn decode(encoded: &str) -> Vec<OsString> {
    serde_json::from_str::<Vec<String>>(encoded)
        .unwrap()
        .into_iter()
        .map(Into::into)
        .collect()
}

#[test]
fn entry() {
    let Ok(args) = std::env::var(ARGS) else {
        return;
    };
    // Ignore a closed consumer here: run owns the command's broken-pipe policy.
    let _ = std::io::stdout().write_all(MARKER);
    let _ = std::io::stdout().flush();
    std::process::exit(crate::run(decode(&args)));
}
