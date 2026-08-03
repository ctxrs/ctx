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
        route.metadata.route_identity = Some(preserved);
        Ok(())
    }
}
