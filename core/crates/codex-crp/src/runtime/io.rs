use super::RuntimeCommand;
use crate::protocol::{CrpChannel, CrpCommandEnvelope, CrpEvent, CrpEventEnvelope, CRP_VERSION};
use anyhow::Result;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tracing::warn;

const CRP_EVENT_DUMP_ENV: &str = "CODEX_CRP_DUMP_CRP_EVENTS_PATH";

static CRP_EVENT_DUMP: OnceLock<Option<Mutex<std::io::BufWriter<std::fs::File>>>> = OnceLock::new();

pub(super) struct CrpWriter {
    seq: u64,
    out: BufWriter<tokio::io::Stdout>,
}

impl CrpWriter {
    pub(super) fn new() -> Self {
        Self {
            seq: 0,
            out: BufWriter::new(tokio::io::stdout()),
        }
    }

    async fn send(&mut self, channel: CrpChannel, event: CrpEvent) -> Result<()> {
        let envelope = CrpEventEnvelope {
            v: Some(CRP_VERSION),
            seq: self.next_seq(),
            channel,
            event,
        };
        maybe_dump_crp_event(&envelope);
        let bytes = serde_json::to_vec(&envelope)?;
        self.out.write_all(&bytes).await?;
        self.out.write_all(b"\n").await?;
        self.out.flush().await?;
        Ok(())
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

pub(super) struct CrpEventRouter {
    control_tx: mpsc::UnboundedSender<CrpEvent>,
    data_tx: mpsc::Sender<CrpEvent>,
}

impl CrpEventRouter {
    pub(super) fn new(
        control_tx: mpsc::UnboundedSender<CrpEvent>,
        data_tx: mpsc::Sender<CrpEvent>,
    ) -> Self {
        Self {
            control_tx,
            data_tx,
        }
    }

    pub(super) fn send_control(&self, event: CrpEvent) -> Result<(), ()> {
        self.control_tx.send(event).map_err(|_| ())
    }

    pub(super) fn send_data(&self, event: CrpEvent) -> Result<(), Option<String>> {
        let session_id = event_session_id(&event).map(ToString::to_string);
        match self.data_tx.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(event))
            | Err(mpsc::error::TrySendError::Closed(event)) => {
                let _ = event;
                Err(session_id)
            }
        }
    }
}

pub(super) fn dispatch_event(router: &CrpEventRouter, channel: CrpChannel, event: CrpEvent) {
    match channel {
        CrpChannel::Control => {
            if router.send_control(event).is_err() {
                warn!("failed to dispatch control event");
            }
        }
        CrpChannel::Data => {
            if let Err(Some(session_id)) = router.send_data(event) {
                let _ = router.send_control(CrpEvent::SessionGap {
                    session_id,
                    turn_id: None,
                    reason: Some("data_plane_overflow".to_string()),
                });
            }
        }
    }
}

pub(super) async fn read_commands(tx: mpsc::UnboundedSender<RuntimeCommand>) {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let command = match serde_json::from_str::<CrpCommandEnvelope>(trimmed) {
            Ok(envelope) => RuntimeCommand::Parsed(Box::new(envelope.command)),
            Err(err) => RuntimeCommand::ParseError {
                message: err.to_string(),
            },
        };
        if tx.send(command).is_err() {
            break;
        }
    }
}

pub(super) async fn run_writer(
    mut writer: CrpWriter,
    mut control_rx: mpsc::UnboundedReceiver<CrpEvent>,
    mut data_rx: mpsc::Receiver<CrpEvent>,
) -> Result<()> {
    loop {
        tokio::select! {
            biased;
            Some(event) = control_rx.recv() => writer.send(CrpChannel::Control, event).await?,
            Some(event) = data_rx.recv() => writer.send(CrpChannel::Data, event).await?,
            else => break,
        }
    }
    Ok(())
}

fn event_session_id(event: &CrpEvent) -> Option<&str> {
    match event {
        CrpEvent::MessageDelta { session_id, .. }
        | CrpEvent::ReasoningTrace { session_id, .. }
        | CrpEvent::ReasoningTraceFinal { session_id, .. }
        | CrpEvent::ToolOutputDelta { session_id, .. } => Some(session_id),
        _ => None,
    }
}

fn maybe_dump_crp_event(envelope: &CrpEventEnvelope) {
    let Ok(path) = std::env::var(CRP_EVENT_DUMP_ENV) else {
        return;
    };

    let Some(writer) =
        CRP_EVENT_DUMP.get_or_init(|| open_dump_writer(&path, CRP_EVENT_DUMP_ENV).map(Mutex::new))
    else {
        return;
    };

    let Ok(mut writer) = writer.lock() else {
        return;
    };
    if serde_json::to_writer(&mut *writer, envelope).is_ok() {
        let _ = writer.write_all(b"\n");
        let _ = writer.flush();
    }
}

fn open_dump_writer(path: &str, env_name: &str) -> Option<std::io::BufWriter<std::fs::File>> {
    if let Some(parent) = Path::new(path).parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            warn!(
                path = %path,
                parent = %parent.display(),
                error = %err,
                "failed to create {env_name} parent; disabling CRP event dumps"
            );
            return None;
        }
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => Some(std::io::BufWriter::new(file)),
        Err(err) => {
            warn!(
                path = %path,
                error = %err,
                "failed to open {env_name}; disabling CRP event dumps"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{open_dump_writer, CRP_EVENT_DUMP_ENV};

    #[test]
    fn crp_dump_writer_creates_parent_dirs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("nested").join("crp-events.jsonl");
        let writer = open_dump_writer(
            path.to_str().expect("test path should be utf-8"),
            CRP_EVENT_DUMP_ENV,
        )
        .expect("writer should open");
        drop(writer);
        assert!(path.exists());
    }
}
