use super::*;

#[derive(Debug, Clone)]
pub struct SourceBackedResolverRegistry {
    pub(super) routes: Vec<SourceBackedRoute>,
}

#[derive(Debug)]
struct ResolvedHydrationGroup {
    route_index: usize,
    source: SourceKey,
    positions: Vec<usize>,
    events: Vec<EventHydrationRequest>,
}

impl SourceBackedResolverRegistry {
    fn resolve_route_index(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<usize, HydrationFailure> {
        let source = request.locator().source();
        if let Some(source_path) = request.source_path_hint() {
            let source_path = Path::new(source_path);
            let mut path_matches = self.routes.iter().enumerate().filter(|(_, route)| {
                route.metadata.source.provider.as_str() == source.provider()
                    && route.metadata.certified_source_format == source.source_format()
                    && route.driver.is_some()
                    && source_path.is_absolute()
                    && source_path.starts_with(&route.metadata.source.path)
            });
            if let Some((route_index, _)) = path_matches.next() {
                if path_matches.next().is_none() {
                    return Ok(route_index);
                }
            }
        }
        let mut matches = self.routes.iter().enumerate().filter(|(_, route)| {
            route.metadata.source.provider.as_str() == source.provider()
                && route.metadata.certified_source_format == source.source_format()
                && route
                    .driver
                    .as_ref()
                    .is_some_and(|driver| (driver.owns_source)(source))
        });
        let Some((route_index, _)) = matches.next() else {
            let unsupported = self.routes.iter().any(|route| {
                route.metadata.source.provider.as_str() == source.provider()
                    && route.metadata.certified_source_format == source.source_format()
                    && route.driver.is_none()
            });
            return Err(hydration_failure(
                if unsupported {
                    HydrationFailureKind::UnsupportedParserRevision
                } else {
                    HydrationFailureKind::InvalidLocator
                },
                if unsupported {
                    "the detected provider source format has no exact hydration route"
                } else {
                    "no registered provider route owns the exact source descriptor"
                },
            ));
        };
        if matches.next().is_some() {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "more than one provider route claimed the exact source descriptor",
            ));
        }
        Ok(route_index)
    }

    fn hydrate_ordered_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let mut groups = Vec::<ResolvedHydrationGroup>::new();
        let mut group_indices = HashMap::<(usize, [u8; 32]), usize>::new();

        // Resolve every event before invoking any provider callback so routing
        // failures cannot produce a partially hydrated return value.
        for (position, event) in request.events().iter().enumerate() {
            let source = event.locator().source();
            let route_index = self.resolve_route_index(event)?;
            let key = (route_index, source.exact_descriptor_digest());
            if let Some(group_index) = group_indices.get(&key).copied() {
                let group = groups.get_mut(group_index).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "batch hydration group index was invalid",
                    )
                })?;
                if !group.source.exact_descriptor_eq(source) {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "distinct exact source descriptors shared a batch grouping digest",
                    ));
                }
                group.positions.push(position);
                group.events.push(event.clone());
            } else {
                group_indices.insert(key, groups.len());
                groups.push(ResolvedHydrationGroup {
                    route_index,
                    source: source.clone(),
                    positions: vec![position],
                    events: vec![event.clone()],
                });
            }
        }

        let mut ordered = (0..request.len())
            .map(|_| None)
            .collect::<Vec<Option<HydratedProviderRecord>>>();
        for group in groups {
            let route = self.routes.get(group.route_index).ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "resolved batch hydration route was absent",
                )
            })?;
            let driver = route.driver.as_ref().ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::UnsupportedParserRevision,
                    "the provider route has no exact hydration driver",
                )
            })?;
            let group_request = BatchHydrationRequest::new(group.events).map_err(|error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid grouped batch hydration request: {error}"),
                )
            })?;
            let group_result = driver.hydrate_batch(&group_request)?;
            group_result.validate_for_request(&group_request)?;

            for (position, record) in group.positions.into_iter().zip(group_result.into_records()) {
                let slot = ordered.get_mut(position).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "batch hydration produced an out-of-range result position",
                    )
                })?;
                if slot.replace(record).is_some() {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "batch hydration produced a duplicate result position",
                    ));
                }
            }
        }

        let records = ordered
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "batch hydration did not produce every requested event",
                )
            })?;
        let result = BatchHydrationResult::new(records).map_err(|error| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                format!("invalid ordered batch hydration result: {error}"),
            )
        })?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

impl ContentSourceResolver for SourceBackedResolverRegistry {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let route_index = self.resolve_route_index(request)?;
        let route = self.routes.get(route_index).ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "resolved event hydration route was absent",
            )
        })?;
        let driver = route.driver.as_ref().ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "the provider route has no exact hydration driver",
            )
        })?;
        let record = (driver.hydrate)(request)?;
        if record.event_id != request.event_id() {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "event hydration returned the wrong event identity",
            ));
        }
        Ok(record)
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.hydrate_ordered_batch(request)
    }
}
