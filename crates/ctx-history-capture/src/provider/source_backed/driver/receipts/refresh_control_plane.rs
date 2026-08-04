use super::*;

impl SourceBackedRecordRejectionClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedRecord => "malformed_record",
            Self::UnsupportedRecord => "unsupported_record",
        }
    }
}

impl SourceBackedProviderRegistry {
    /// Rebinds one newly constructed explicit route to a previously certified
    /// identity during an explicit relocation. Callers must establish exact
    /// provider/format/source-lineage continuity before invoking this seam.
    pub fn preserve_explicit_route_identity(
        &mut self,
        constructed: &SourceRouteIdentity,
        preserved: SourceRouteIdentity,
        relocate_from: &Path,
    ) -> SourceBackedCoordinatorResult<()> {
        if self
            .routes
            .iter()
            .any(|route| route.metadata.route_identity.as_ref() == Some(&preserved))
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: preserved.as_str().to_owned(),
            });
        }
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(constructed))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: constructed.as_str().to_owned(),
            })?;
        if route.metadata.selection != Some(SourceBackedRouteSelection::ExplicitManual) {
            return Err(SourceBackedCoordinatorError::InvalidRoute {
                provider: route.metadata.source.provider,
                detail: "only an explicit route can preserve relocation identity".to_owned(),
            });
        }
        let driver =
            route
                .driver
                .as_mut()
                .ok_or_else(|| SourceBackedCoordinatorError::InvalidRoute {
                    provider: route.metadata.source.provider,
                    detail: "relocated explicit route has no executable driver".to_owned(),
                })?;
        let original_revalidate = Arc::clone(&driver.revalidate);
        let relocate_from = relocate_from.to_path_buf();
        let publication_relocate_from = relocate_from.clone();
        driver.revalidate = Arc::new(move |target| {
            if relocation_source_remains_absent(&relocate_from) {
                original_revalidate(target)
            } else {
                Ok(false)
            }
        });
        driver.revalidate_at_publication = Some(Arc::new(move || {
            relocation_source_remains_absent(&publication_relocate_from)
        }));
        route.metadata.route_identity = Some(preserved);
        Ok(())
    }
}

fn relocation_source_remains_absent(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocation_absence_witness_fails_when_the_old_exact_path_reappears() {
        let temp = tempfile::tempdir().unwrap();
        let old_path = temp.path().join("old.jsonl");
        assert!(relocation_source_remains_absent(&old_path));
        std::fs::write(&old_path, b"reappeared\n").unwrap();
        assert!(!relocation_source_remains_absent(&old_path));
    }
}
