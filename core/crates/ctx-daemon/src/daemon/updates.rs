use std::path::PathBuf;

use ctx_update_service::route_contract::{
    ApplyAppImageUpdateRequest, ApplyAppImageUpdateResult, DownloadAppImageUpdateRequest,
    DownloadAppImageUpdateResult, UpdateActivitySnapshot, UpdateCheckSnapshot, UpdateRouteError,
};

use crate::daemon::{UpdateActivityHandle, UpdateReleaseHandle};

fn normalize_channel(raw: Option<&str>) -> Result<String, UpdateRouteError> {
    ctx_update_service::normalize_release_channel(raw.unwrap_or("stable"))
        .map_err(UpdateRouteError::bad_request)
}

fn current_version(package_version: &'static str) -> Result<String, UpdateRouteError> {
    ctx_update_service::current_build_identity(package_version)
        .map(|identity| identity.exact_version.clone())
        .map_err(UpdateRouteError::internal)
}

fn required_platform() -> Result<&'static str, UpdateRouteError> {
    ctx_update_service::platform_key()
        .ok_or_else(|| UpdateRouteError::bad_request("unsupported platform"))
}

fn appimage_target_path() -> Result<PathBuf, UpdateRouteError> {
    ctx_update_service::appimage_path_env().ok_or_else(|| {
        UpdateRouteError::bad_request("CTX_APPIMAGE_PATH not set; cannot apply in place")
    })
}

impl UpdateReleaseHandle {
    pub async fn check_updates(
        &self,
        package_version: &'static str,
        channel: Option<String>,
    ) -> Result<UpdateCheckSnapshot, UpdateRouteError> {
        let channel = normalize_channel(channel.as_deref())?;
        let base_url = ctx_update_service::default_download_base_url();
        let platform = ctx_update_service::platform_key().map(|s| s.to_string());
        let current_version = current_version(package_version)?;

        let query = platform.as_ref().map(|p| {
            vec![
                ("current_version", current_version.clone()),
                ("platform", p.clone()),
            ]
        });

        let manifest = ctx_update_service::fetch_latest_manifest_with_params(
            &base_url,
            &channel,
            query.as_deref(),
        )
        .await
        .map_err(UpdateRouteError::bad_gateway)?;

        let latest_version = manifest.latest_version.clone();
        let min_supported_version = manifest.min_supported_version.clone();
        let platform_supported =
            ctx_update_service::platform_supported(&manifest, platform.as_deref());
        let (in_place_update_supported, in_place_update_reason) =
            ctx_update_service::in_place_update_capability(
                &manifest,
                platform.as_deref(),
                platform_supported,
            );
        let update_available = ctx_update_service::is_update_available(
            &current_version,
            &latest_version,
            platform_supported,
        );

        Ok(UpdateCheckSnapshot {
            channel,
            base_url,
            platform,
            current_version,
            latest_version: Some(latest_version),
            min_supported_version,
            platform_supported,
            in_place_update_supported,
            in_place_update_reason,
            update_available,
            manifest: serde_json::to_value(manifest).unwrap_or(serde_json::Value::Null),
        })
    }

    pub async fn download_appimage_update(
        &self,
        package_version: &'static str,
        request: DownloadAppImageUpdateRequest,
    ) -> Result<DownloadAppImageUpdateResult, UpdateRouteError> {
        let channel = normalize_channel(request.channel())?;
        let base_url = ctx_update_service::default_download_base_url();
        let platform = required_platform()?;

        let manifest = ctx_update_service::fetch_latest_manifest(&base_url, &channel)
            .await
            .map_err(UpdateRouteError::bad_gateway)?;
        let platform_entry = manifest.platforms.get(platform).ok_or_else(|| {
            UpdateRouteError::bad_gateway(format!("manifest missing platform {platform}"))
        })?;
        let appimage = platform_entry
            .appimage
            .as_ref()
            .ok_or_else(|| UpdateRouteError::bad_gateway("manifest missing appimage artifact"))?;

        let target_path = appimage_target_path()?;
        let current_version = current_version(package_version)?;
        let url = ctx_update_service::resolve_release_artifact_url(&base_url, &appimage.url_path)
            .map_err(UpdateRouteError::bad_gateway)?;
        let manifest_url = ctx_update_service::release_manifest_url(&base_url, &channel);
        let meta = ctx_update_service::download_verified_appimage_candidate(
            ctx_update_service::AppImageCandidateRequest {
                data_root: self.data_root(),
                target_path: &target_path,
                channel: &channel,
                platform,
                target_version: &manifest.latest_version,
                current_version: &current_version,
                artifact_url: &url,
                artifact_url_path: &appimage.url_path,
                manifest_url: &manifest_url,
                base_url: &base_url,
                sha256: &appimage.sha256,
            },
        )
        .await
        .map_err(UpdateRouteError::bad_gateway)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(md) = tokio::fs::metadata(&meta.candidate_path).await {
                let mut p = md.permissions();
                p.set_mode(0o755);
                let _ = tokio::fs::set_permissions(&meta.candidate_path, p).await;
            }
        }

        Ok(DownloadAppImageUpdateResult {
            downloaded_path: meta.candidate_path.to_string_lossy().to_string(),
            can_apply_in_place: ctx_update_service::appimage_path_env().is_some(),
        })
    }

    pub async fn apply_appimage_update(
        &self,
        package_version: &'static str,
        request: ApplyAppImageUpdateRequest,
    ) -> Result<ApplyAppImageUpdateResult, UpdateRouteError> {
        if !request.confirm() {
            return Err(UpdateRouteError::bad_request("confirm required"));
        }

        let channel = normalize_channel(request.channel())?;
        let base_url = ctx_update_service::default_download_base_url();
        let platform = required_platform()?;
        let target = appimage_target_path()?;
        let current_version = current_version(package_version)?;
        let (downloaded, _meta) = ctx_update_service::validate_verified_appimage_candidate(
            self.data_root(),
            &target,
            &channel,
            platform,
            &base_url,
            &current_version,
        )
        .await
        .map_err(UpdateRouteError::bad_request)?;

        ctx_update_service::atomic_replace_file(&target, &downloaded)
            .await
            .map_err(UpdateRouteError::internal)?;
        ctx_update_service::clear_appimage_candidate(self.data_root()).await;

        Ok(ApplyAppImageUpdateResult {
            applied: true,
            target_path: Some(target.to_string_lossy().to_string()),
            message:
                "Update applied in place. Quit and relaunch the desktop app to run the new version."
                    .to_string(),
        })
    }
}

impl UpdateActivityHandle {
    pub async fn update_activity_snapshot(
        &self,
    ) -> Result<UpdateActivitySnapshot, UpdateRouteError> {
        let activity = crate::daemon::activity::daemon_turn_activity_summary_parts(
            self.global_store(),
            self.stores(),
            self.update_drain(),
        )
        .await
        .map_err(UpdateRouteError::internal)?;
        let managed_daemon_auto_update =
            ctx_update_service::managed_daemon_auto_update_status_snapshot(self.data_root()).await;
        Ok(UpdateActivitySnapshot {
            activity,
            managed_daemon_auto_update,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_update_service::route_contract::UpdateRouteErrorKind;

    #[test]
    fn invalid_update_channel_is_bad_request() {
        let error = normalize_channel(Some("x/../../../secret")).unwrap_err();
        assert_eq!(error.kind(), UpdateRouteErrorKind::BadRequest);
    }

    #[test]
    fn missing_appimage_path_is_bad_request() {
        let previous = std::env::var("CTX_APPIMAGE_PATH").ok();
        std::env::remove_var("CTX_APPIMAGE_PATH");
        let error = appimage_target_path().unwrap_err();
        assert_eq!(error.kind(), UpdateRouteErrorKind::BadRequest);
        if let Some(value) = previous {
            std::env::set_var("CTX_APPIMAGE_PATH", value);
        }
    }
}
