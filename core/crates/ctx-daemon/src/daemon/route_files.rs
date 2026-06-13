use std::path::Path;

use ctx_route_contracts::downloads::TextRouteDownload;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RouteFileDownloadError {
    NotFound,
    Internal,
}

pub async fn path_resolves_within_root(path: &Path, root: &Path) -> bool {
    let Ok(canonical_path) = tokio::fs::canonicalize(path).await else {
        return false;
    };
    let Ok(canonical_root) = tokio::fs::canonicalize(root).await else {
        return false;
    };
    canonical_path.starts_with(&canonical_root)
}

pub async fn read_text_route_file(
    path: &Path,
    root: &Path,
    filename: String,
) -> Result<TextRouteDownload, RouteFileDownloadError> {
    if !path_resolves_within_root(path, root).await {
        return Err(RouteFileDownloadError::NotFound);
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| RouteFileDownloadError::NotFound)?;
    Ok(TextRouteDownload { bytes, filename })
}

pub async fn open_canonical_route_file(
    path: &Path,
) -> Result<tokio::fs::File, RouteFileDownloadError> {
    #[cfg(unix)]
    {
        let canonical = path.to_path_buf();
        let std_file = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;

            let mut options = std::fs::OpenOptions::new();
            options.read(true).custom_flags(libc::O_NOFOLLOW);
            options.open(canonical)
        })
        .await
        .map_err(|_| RouteFileDownloadError::Internal)?
        .map_err(|_| RouteFileDownloadError::NotFound)?;
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(not(unix))]
    {
        tokio::fs::File::open(path)
            .await
            .map_err(|_| RouteFileDownloadError::NotFound)
    }
}
