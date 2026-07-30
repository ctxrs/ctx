use std::{
    fs::Metadata,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl CursorObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorFileIdentity {
    device: u64,
    inode: u64,
}

pub(crate) fn cursor_complete_content_source_from_admitted(
    metadata: &Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let (file_identity, changed) = (
        Some(CursorFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }),
        unix_changed_time(metadata),
    );
    #[cfg(not(unix))]
    let (file_identity, changed) = (None, None);
    Ok((
        cursor_complete_content_revision(
            metadata.len(),
            CursorObservedTime::from_system_time(metadata.modified()?),
            changed,
            metadata.permissions().readonly(),
            file_identity,
        ),
        path_identity,
    ))
}

fn cursor_complete_content_revision(
    length: u64,
    modified: CursorObservedTime,
    changed: Option<CursorObservedTime>,
    readonly: bool,
    file_identity: Option<CursorFileIdentity>,
) -> String {
    let changed = changed.map_or_else(|| "none".to_owned(), cursor_observed_time_stamp);
    format!(
        "cursor-complete-content-source-v1:length={length};modified={};changed={changed};readonly={readonly};device={};inode={}",
        cursor_observed_time_stamp(modified),
        file_identity.map_or_else(|| "none".to_owned(), |identity| identity.device.to_string()),
        file_identity.map_or_else(|| "none".to_owned(), |identity| identity.inode.to_string()),
    )
}

fn cursor_observed_time_stamp(time: CursorObservedTime) -> String {
    format!(
        "{}{}.{:09}",
        if time.before_epoch { '-' } else { '+' },
        time.seconds,
        time.nanos
    )
}

#[cfg(unix)]
fn unix_changed_time(metadata: &Metadata) -> Option<CursorObservedTime> {
    use std::os::unix::fs::MetadataExt;

    let seconds = metadata.ctime();
    let nanos = metadata.ctime_nsec();
    if !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    if seconds >= 0 {
        Some(CursorObservedTime {
            before_epoch: false,
            seconds: seconds as u64,
            nanos: nanos as u32,
        })
    } else {
        Some(CursorObservedTime {
            before_epoch: true,
            seconds: seconds.unsigned_abs(),
            nanos: nanos as u32,
        })
    }
}
