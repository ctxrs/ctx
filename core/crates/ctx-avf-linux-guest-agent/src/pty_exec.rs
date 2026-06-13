use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use portable_pty::{NativePtySystem, PtySize, PtySystem};

use crate::process_setup::{build_pty_command, PreparedExec};
use crate::protocol::{read_exec_frame, AvfLinuxExecExit, AvfLinuxExecFrame};
use crate::{
    emit_exec_stream_frames, write_stream_frame, AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD,
    DEFAULT_PTY_COLS, DEFAULT_PTY_ROWS,
};

fn relay_pty_output(reader: &mut impl Read, writer: &Arc<Mutex<File>>) -> Result<()> {
    let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                emit_exec_stream_frames(&buf[..n], true, |frame| write_stream_frame(writer, frame))?
            }
            Err(err) => return Err(err).context("reading PTY output"),
        }
    }
}

pub(crate) fn handle_pty_connection(stream: File, prepared: PreparedExec) -> Result<()> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: DEFAULT_PTY_ROWS,
            cols: DEFAULT_PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("opening guest PTY")?;
    let cmd = build_pty_command(&prepared)?;
    let mut child = pair.slave.spawn_command(cmd).with_context(|| {
        format!(
            "spawning guest PTY command `{}` in {}",
            prepared.command,
            prepared.cwd.display()
        )
    })?;
    drop(pair.slave);

    let mut pty_reader = pair
        .master
        .try_clone_reader()
        .context("cloning guest PTY reader")?;
    let mut pty_writer = pair
        .master
        .take_writer()
        .context("taking guest PTY writer")?;
    let resize_master = Arc::new(Mutex::new(pair.master));
    let writer = Arc::new(Mutex::new(
        stream
            .try_clone()
            .context("cloning connection for PTY output")?,
    ));
    let output_writer = Arc::clone(&writer);
    let output_thread = std::thread::spawn(move || -> Result<()> {
        relay_pty_output(&mut pty_reader, &output_writer)
    });
    let mut stdin_reader = stream;
    let input_thread = std::thread::spawn(move || -> Result<()> {
        loop {
            match read_exec_frame(&mut stdin_reader).context("reading guest PTY input frame")? {
                Some(AvfLinuxExecFrame::Stdin(bytes)) => pty_writer
                    .write_all(&bytes)
                    .and_then(|_| pty_writer.flush())
                    .context("writing guest PTY stdin")?,
                Some(AvfLinuxExecFrame::Resize(resize)) => {
                    let master = resize_master
                        .lock()
                        .map_err(|_| anyhow::anyhow!("guest PTY master mutex poisoned"))?;
                    master
                        .resize(PtySize {
                            rows: resize.rows,
                            cols: resize.cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .context("resizing guest PTY")?;
                }
                Some(AvfLinuxExecFrame::CloseStdin) | None => return Ok(()),
                Some(other) => bail!("unexpected PTY frame after request: {other:?}"),
            }
        }
    });

    let exit_code = i32::try_from(
        child
            .wait()
            .context("waiting for guest PTY command")?
            .exit_code(),
    )
    .unwrap_or(1);
    let _ = input_thread.join();
    let _ = output_thread.join();
    write_stream_frame(
        &writer,
        AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code }),
    )
}
