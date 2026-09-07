use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use tempfile::NamedTempFile;

use super::super::support::terminate_and_reap_test_child;

// Bound snapshots and diagnostics, not instantaneous disk usage by an unpolled child.
const MAX_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) struct CapturedChild {
    child: Option<Child>,
    stdout: NamedTempFile,
    stderr: NamedTempFile,
}

impl CapturedChild {
    pub(super) fn spawn(command: &mut Command, root: &Path) -> io::Result<Self> {
        let stdout = NamedTempFile::new_in(root)?;
        let stderr = NamedTempFile::new_in(root)?;
        // Capture starts before spawn, including children collected in a later phase.
        command.stdout(stdout.reopen()?).stderr(stderr.reopen()?);
        Ok(Self {
            child: Some(command.spawn()?),
            stdout,
            stderr,
        })
    }

    pub(super) fn id(&self) -> u32 {
        self.child.as_ref().expect("owned capture child").id()
    }

    pub(super) fn write_input(&mut self, input: &[u8]) -> io::Result<()> {
        self.child
            .as_mut()
            .expect("owned capture child")
            .stdin
            .take()
            .expect("piped capture stdin")
            .write_all(input)
    }

    pub(super) fn terminate(&mut self) -> Result<(), String> {
        terminate_and_reap_test_child(&mut self.child, "captured lifecycle command").map(|_| ())
    }

    pub(super) fn output(mut self, timeout: Duration) -> Result<Output, String> {
        // Shutdown callers deliberately begin their deadline after a later signal.
        let deadline = Instant::now() + timeout;
        loop {
            capture_len(&self.stdout).map_err(|error| format!("stdout capture: {error}"))?;
            capture_len(&self.stderr).map_err(|error| format!("stderr capture: {error}"))?;
            match self
                .child
                .as_mut()
                .expect("owned capture child")
                .try_wait()
                .map_err(|error| format!("poll captured child: {error}"))?
            {
                Some(status) => {
                    self.child.take(); // try_wait reaped this exact child's exit status.
                    let (stdout, stderr) = self.snapshots()?;
                    return Ok(Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                None => {
                    let pid = self.id();
                    self.terminate()?;
                    let (stdout, stderr) = self.snapshots()?;
                    return Err(format!(
                        "pid {pid} exceeded {timeout:?}:\nstdout:\n{}\nstderr:\n{}",
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr)
                    ));
                }
            }
        }
    }

    fn snapshots(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        let stdout = snapshot(&self.stdout).map_err(|error| format!("stdout capture: {error}"))?;
        let stderr = snapshot(&self.stderr).map_err(|error| format!("stderr capture: {error}"))?;
        Ok((stdout, stderr))
    }
}

impl Drop for CapturedChild {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            if thread::panicking() {
                eprintln!("capture teardown also failed: {error}");
            } else {
                panic!("capture teardown failed: {error}");
            }
        }
    }
}

fn capture_len(file: &NamedTempFile) -> io::Result<u64> {
    let len = file.as_file().metadata()?.len();
    if len > MAX_CAPTURE_BYTES {
        return Err(io::Error::other(format!(
            "capture exceeds {MAX_CAPTURE_BYTES} bytes"
        )));
    }
    Ok(len)
}

fn snapshot(file: &NamedTempFile) -> io::Result<Vec<u8>> {
    let len = capture_len(file)?;
    // Reopen independently: a seek on a cloned writer could change its offset.
    read_snapshot(file.reopen()?, len)
}

fn read_snapshot(reader: File, len: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(len).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "capture shrank while reading",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
