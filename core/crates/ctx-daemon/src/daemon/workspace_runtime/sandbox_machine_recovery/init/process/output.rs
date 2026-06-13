use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;

pub(super) async fn read_child_pipe<R>(mut reader: R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await?;
    Ok(buf)
}

pub(super) async fn collect_child_output(
    status: std::process::ExitStatus,
    stdout_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<std::process::Output> {
    let stdout = stdout_task
        .await
        .context("joining sandbox machine init stdout capture")??;
    let stderr = stderr_task
        .await
        .context("joining sandbox machine init stderr capture")??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}
