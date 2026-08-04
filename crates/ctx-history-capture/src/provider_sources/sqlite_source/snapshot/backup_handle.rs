use super::*;

#[cfg(test)]
pub(super) fn run_online_backup_until(
    source: &Connection,
    destination: &Connection,
    deadline: Instant,
) -> SqliteSourceAccessResult<()> {
    let backup = unsafe {
        ffi::sqlite3_backup_init(
            destination.handle(),
            c"main".as_ptr(),
            source.handle(),
            c"main".as_ptr(),
        )
    };
    if backup.is_null() {
        return Err(SqliteSourceAccessError::SqliteControl {
            operation: "initializing the logical SQLite online backup",
            code: unsafe { ffi::sqlite3_extended_errcode(destination.handle()) },
        });
    }
    let mut backup = OnlineBackupHandle(Some(backup));
    loop {
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_error());
        }
        let code = unsafe {
            ffi::sqlite3_backup_step(backup.pointer(), SQLITE_ONLINE_BACKUP_PAGES_PER_STEP)
        };
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_error());
        }
        match code {
            ffi::SQLITE_DONE => break,
            ffi::SQLITE_OK => continue,
            ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            code => {
                return Err(SqliteSourceAccessError::SqliteControl {
                    operation: "copying the pinned logical SQLite snapshot",
                    code,
                });
            }
        }
    }
    let code = backup.finish();
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "finishing the logical SQLite online backup",
            code,
        })
    }
}

pub(super) fn online_backup_deadline_error() -> SqliteSourceAccessError {
    SqliteSourceAccessError::SnapshotUnavailable {
        reason: "the logical SQLite online backup exceeded its five-minute deadline".to_owned(),
    }
}

pub(super) struct OnlineBackupHandle(pub(super) Option<*mut ffi::sqlite3_backup>);

impl OnlineBackupHandle {
    pub(super) fn pointer(&self) -> *mut ffi::sqlite3_backup {
        self.0.unwrap_or(ptr::null_mut())
    }

    pub(super) fn finish(&mut self) -> i32 {
        self.0.take().map_or(ffi::SQLITE_OK, |backup| unsafe {
            ffi::sqlite3_backup_finish(backup)
        })
    }
}

impl Drop for OnlineBackupHandle {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

pub(super) fn end_pinned_read_snapshot(connection: &Connection) -> SqliteSourceAccessResult<()> {
    clear_snapshot_authorizer(connection)?;
    connection
        .execute_batch("ROLLBACK")
        .map_err(|source| sqlite_error("ending the provider online-backup snapshot", source))
}
