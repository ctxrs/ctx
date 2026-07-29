use super::*;

pub(super) fn discover_sessions(path: &Path) -> Result<Vec<MuxSessionSource>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut sessions = Vec::new();
    visit_mux_session_sources(path, &mut |source| {
        sessions.push(source);
        Ok(())
    })?;
    Ok(sessions)
}
