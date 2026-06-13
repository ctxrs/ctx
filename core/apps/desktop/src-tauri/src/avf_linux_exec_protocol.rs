use std::collections::HashMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub(crate) const AVF_LINUX_EXEC_PROTOCOL_VERSION: u32 = 1;

const FRAME_REQUEST: u8 = 1;
const FRAME_STDIN: u8 = 2;
const FRAME_STDOUT: u8 = 3;
const FRAME_STDERR: u8 = 4;
const FRAME_EXIT: u8 = 5;
const FRAME_ERROR: u8 = 6;
const FRAME_CLOSE_STDIN: u8 = 7;
const FRAME_RESIZE: u8 = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AvfLinuxExecRequest {
    pub(crate) protocol_version: u32,
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    pub(crate) cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) user: Option<String>,
    #[serde(default)]
    pub(crate) env: HashMap<String, String>,
    #[serde(default)]
    pub(crate) pty: bool,
}

impl AvfLinuxExecRequest {
    pub(crate) fn new(
        command: impl Into<String>,
        args: Vec<String>,
        cwd: impl Into<String>,
        user: Option<String>,
        env: HashMap<String, String>,
        pty: bool,
    ) -> Self {
        Self {
            protocol_version: AVF_LINUX_EXEC_PROTOCOL_VERSION,
            command: command.into(),
            args,
            cwd: cwd.into(),
            user,
            env,
            pty,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AvfLinuxExecExit {
    pub(crate) exit_code: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AvfLinuxExecError {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AvfLinuxExecResize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AvfLinuxExecFrame {
    Request(AvfLinuxExecRequest),
    Stdin(Vec<u8>),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(AvfLinuxExecExit),
    Error(AvfLinuxExecError),
    CloseStdin,
    Resize(AvfLinuxExecResize),
}

impl AvfLinuxExecFrame {
    fn tag(&self) -> u8 {
        match self {
            Self::Request(_) => FRAME_REQUEST,
            Self::Stdin(_) => FRAME_STDIN,
            Self::Stdout(_) => FRAME_STDOUT,
            Self::Stderr(_) => FRAME_STDERR,
            Self::Exit(_) => FRAME_EXIT,
            Self::Error(_) => FRAME_ERROR,
            Self::CloseStdin => FRAME_CLOSE_STDIN,
            Self::Resize(_) => FRAME_RESIZE,
        }
    }
}

pub(crate) fn write_exec_frame(
    writer: &mut impl Write,
    frame: &AvfLinuxExecFrame,
) -> io::Result<()> {
    let payload = match frame {
        AvfLinuxExecFrame::Request(request) => serde_json::to_vec(request)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        AvfLinuxExecFrame::Stdin(bytes)
        | AvfLinuxExecFrame::Stdout(bytes)
        | AvfLinuxExecFrame::Stderr(bytes) => bytes.clone(),
        AvfLinuxExecFrame::Exit(exit) => serde_json::to_vec(exit)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        AvfLinuxExecFrame::Error(error) => serde_json::to_vec(error)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        AvfLinuxExecFrame::CloseStdin => Vec::new(),
        AvfLinuxExecFrame::Resize(resize) => serde_json::to_vec(resize)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
    };
    writer.write_all(&[frame.tag()])?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    if !payload.is_empty() {
        writer.write_all(&payload)?;
    }
    writer.flush()
}

pub(crate) fn read_exec_frame(reader: &mut impl Read) -> io::Result<Option<AvfLinuxExecFrame>> {
    let mut tag = [0u8; 1];
    match reader.read_exact(&mut tag) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut payload)?;
    }
    let frame = match tag[0] {
        FRAME_REQUEST => AvfLinuxExecFrame::Request(
            serde_json::from_slice(&payload)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        ),
        FRAME_STDIN => AvfLinuxExecFrame::Stdin(payload),
        FRAME_STDOUT => AvfLinuxExecFrame::Stdout(payload),
        FRAME_STDERR => AvfLinuxExecFrame::Stderr(payload),
        FRAME_EXIT => AvfLinuxExecFrame::Exit(
            serde_json::from_slice(&payload)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        ),
        FRAME_ERROR => AvfLinuxExecFrame::Error(
            serde_json::from_slice(&payload)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        ),
        FRAME_CLOSE_STDIN => AvfLinuxExecFrame::CloseStdin,
        FRAME_RESIZE => AvfLinuxExecFrame::Resize(
            serde_json::from_slice(&payload)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
        ),
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown AVF Linux exec frame tag {other}"),
            ))
        }
    };
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_frame_round_trips_request_and_stdout() {
        let mut bytes = Vec::new();
        write_exec_frame(
            &mut bytes,
            &AvfLinuxExecFrame::Request(AvfLinuxExecRequest::new(
                "/bin/echo",
                vec!["hello".to_string()],
                "/ctx/ws/worktrees/wt-456",
                Some("ctxagent".to_string()),
                HashMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
                false,
            )),
        )
        .expect("write request frame");
        write_exec_frame(&mut bytes, &AvfLinuxExecFrame::Stdout(b"hello\n".to_vec()))
            .expect("write stdout frame");
        write_exec_frame(
            &mut bytes,
            &AvfLinuxExecFrame::Resize(AvfLinuxExecResize {
                cols: 120,
                rows: 40,
            }),
        )
        .expect("write resize frame");

        let mut cursor = std::io::Cursor::new(bytes);
        let request = read_exec_frame(&mut cursor)
            .expect("read request frame")
            .expect("request frame present");
        let stdout = read_exec_frame(&mut cursor)
            .expect("read stdout frame")
            .expect("stdout frame present");
        let resize = read_exec_frame(&mut cursor)
            .expect("read resize frame")
            .expect("resize frame present");
        let eof = read_exec_frame(&mut cursor).expect("read eof");

        assert!(matches!(request, AvfLinuxExecFrame::Request(_)));
        assert_eq!(stdout, AvfLinuxExecFrame::Stdout(b"hello\n".to_vec()));
        assert_eq!(
            resize,
            AvfLinuxExecFrame::Resize(AvfLinuxExecResize {
                cols: 120,
                rows: 40
            })
        );
        assert!(eof.is_none());
    }
}
