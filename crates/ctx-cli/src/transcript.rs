use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{anyhow, Result};
use clap::ValueEnum;

mod artifact;
use artifact::{atomic_write_output, AtomicOutputFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TranscriptMode {
    Full,
    Lite,
    Log,
}

impl TranscriptMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lite => "lite",
            Self::Log => "log",
        }
    }
}

pub(crate) fn write_output(body: String, out: Option<PathBuf>) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        atomic_write_output(&out, body.as_bytes())?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub(crate) struct TranscriptOutput<'a> {
    destination: TranscriptDestination<'a>,
    bytes_written: usize,
}

enum TranscriptDestination<'a> {
    Stdout(&'a mut (dyn Write + Send)),
    Staged(AtomicOutputFile),
}

impl<'a> TranscriptOutput<'a> {
    pub(crate) fn create(out: Option<PathBuf>, stdout: &'a mut (dyn Write + Send)) -> Result<Self> {
        let destination = if let Some(out) = out {
            if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            TranscriptDestination::Staged(AtomicOutputFile::create(&out)?)
        } else {
            TranscriptDestination::Stdout(stdout)
        };
        Ok(Self {
            destination,
            bytes_written: 0,
        })
    }

    pub(crate) fn finish(mut self) -> Result<usize> {
        self.flush()?;
        let bytes_written = self.bytes_written;
        if let TranscriptDestination::Staged(output) = self.destination {
            output.commit()?;
        }
        Ok(bytes_written)
    }
}

impl Write for TranscriptOutput<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = match &mut self.destination {
            TranscriptDestination::Stdout(writer) => writer.write(buffer)?,
            TranscriptDestination::Staged(writer) => writer.write(buffer)?,
        };
        self.bytes_written = self.bytes_written.saturating_add(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.destination {
            TranscriptDestination::Stdout(writer) => writer.flush(),
            TranscriptDestination::Staged(writer) => writer.flush(),
        }
    }
}

pub(crate) fn normalize_uuid_prefix(value: &str, kind: &str) -> Result<String> {
    let prefix = value.trim();
    if prefix.len() < 8 {
        return Err(anyhow!(
            "{kind} id prefix must be at least 8 hex characters, or pass a full ctx UUID"
        ));
    }
    if prefix.contains('-') || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        ));
    }
    Ok(prefix.to_ascii_lowercase())
}

pub(crate) fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}
