use super::*;

impl Store {
    /// Binds one persisted capture source to the exact physical alias group
    /// selected during provider-source reconciliation. The binding is local
    /// authorization state and does not affect the semantic projection journal.
    pub fn bind_capture_source_provider_route(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<()> {
        self.with_atomic_write(|| {
            self.bind_capture_source_provider_route_tx(capture_source_id, binding)
        })
    }

    pub(crate) fn bind_capture_source_provider_route_tx(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<()> {
        if self.capture_source_provider_route_is_authorized(capture_source_id, binding)? {
            return Ok(());
        }
        if self.insert_capture_source_provider_route_if_authorized(capture_source_id, binding)? {
            return Ok(());
        }
        self.bind_capture_source_provider_route_inner(capture_source_id, binding)
    }

    /// Revokes the exact current physical route without deleting canonical
    /// source, session, event, run, touch, or cursor history.
    pub(crate) fn retire_provider_source_route_tx(
        &self,
        retirement: &ProviderSourceRouteRetirement,
    ) -> Result<ProviderSourceRouteRetirementDisposition> {
        let locator_identity = locator_storage_key(&retirement.locator_identity);
        let locator = self
            .conn
            .query_row(
                "SELECT cursor_stream, canonical_source_identity,
                        alias_group_identity, source_revision, is_current
                 FROM provider_source_locators
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND locator_identity = ?4",
                params![
                    retirement.provider.as_str(),
                    retirement.source_format,
                    retirement.machine_id,
                    locator_identity,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? != 0,
                    ))
                },
            )
            .optional()?;
        let Some((
            cursor_stream,
            canonical_source_identity,
            alias_group_identity,
            source_revision,
            is_current,
        )) = locator
        else {
            return Err(self.provider_source_route_retirement_conflict(retirement));
        };
        if cursor_stream != retirement.cursor_stream
            || canonical_source_identity != retirement.expected_canonical_source_identity
            || source_revision != retirement.expected_source_revision
        {
            return Err(self.provider_source_route_retirement_conflict(retirement));
        }

        let current_count = self.conn.query_row(
            "SELECT COUNT(*)
             FROM provider_source_locators
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND alias_group_identity = ?4 AND is_current = 1",
            params![
                retirement.provider.as_str(),
                retirement.source_format,
                retirement.machine_id,
                alias_group_identity,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        let route_count = self.conn.query_row(
            "SELECT COUNT(*)
             FROM capture_source_provider_routes route
             JOIN capture_sources source ON source.id = route.capture_source_id
             WHERE route.provider = ?1 AND route.source_format = ?2
               AND route.machine_id = ?3 AND route.alias_group_identity = ?4
               AND source.provider = ?1 AND source.source_format = ?2
               AND source.machine_id = ?3 AND source.source_identity = ?5",
            params![
                retirement.provider.as_str(),
                retirement.source_format,
                retirement.machine_id,
                alias_group_identity,
                retirement.expected_canonical_source_identity,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        let all_route_count = self.conn.query_row(
            "SELECT COUNT(*) FROM capture_source_provider_routes
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND alias_group_identity = ?4",
            params![
                retirement.provider.as_str(),
                retirement.source_format,
                retirement.machine_id,
                alias_group_identity,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        if route_count != all_route_count {
            return Err(self.provider_source_route_retirement_conflict(retirement));
        }

        if !is_current {
            if current_count == 0 && route_count == 0 {
                return Ok(ProviderSourceRouteRetirementDisposition::AlreadyRetired);
            }
            return Err(self.provider_source_route_retirement_conflict(retirement));
        }
        if current_count != 1 {
            return Err(self.provider_source_route_retirement_conflict(retirement));
        }

        let retired = self.conn.execute(
            "UPDATE provider_source_locators
             SET is_current = 0, observed_at_ms = ?5
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND cursor_stream = ?6
               AND canonical_source_identity = ?7
               AND alias_group_identity = ?8 AND source_revision = ?9
               AND is_current = 1",
            params![
                retirement.provider.as_str(),
                retirement.source_format,
                retirement.machine_id,
                locator_identity,
                retirement.retired_at_ms,
                retirement.cursor_stream,
                retirement.expected_canonical_source_identity,
                alias_group_identity,
                retirement.expected_source_revision,
            ],
        )?;
        if retired != 1 {
            return Err(self.provider_source_route_retirement_conflict(retirement));
        }
        // A retired physical route cannot resume an in-flight generation.
        // Close its local staging record without applying omission retirement
        // so a later reappearance can start a fresh generation while all
        // previously published canonical entities remain live.
        self.conn.execute(
            "UPDATE native_path_source_generations
             SET state = 'complete'
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND cursor_stream = ?5
               AND canonical_source_identity = ?6 AND source_revision = ?7
               AND state IN ('staging', 'retiring')",
            params![
                retirement.provider.as_str(),
                retirement.source_format,
                retirement.machine_id,
                locator_identity,
                retirement.cursor_stream,
                retirement.expected_canonical_source_identity,
                retirement.expected_source_revision,
            ],
        )?;
        self.conn.execute(
            "DELETE FROM capture_source_provider_routes
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND alias_group_identity = ?4",
            params![
                retirement.provider.as_str(),
                retirement.source_format,
                retirement.machine_id,
                alias_group_identity,
            ],
        )?;
        Ok(ProviderSourceRouteRetirementDisposition::Retired)
    }

    fn provider_source_route_retirement_conflict(
        &self,
        retirement: &ProviderSourceRouteRetirement,
    ) -> StoreError {
        StoreError::ProviderSourceRouteRetirementConflict {
            provider: retirement.provider.as_str().to_owned(),
            source_format: retirement.source_format.clone(),
        }
    }

    fn capture_source_provider_route_is_authorized(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<bool> {
        // A matching route row is caller-controlled evidence, not authority.
        // Re-prove both the canonical capture source and its current physical
        // locator before taking the common replay path.
        let mut statement = self.conn.prepare_cached(
            "SELECT EXISTS(
                 SELECT 1
                 FROM capture_source_provider_routes route
                 JOIN capture_sources cs ON cs.id = route.capture_source_id
                 JOIN provider_source_locators locator
                   ON locator.provider = route.provider
                  AND locator.source_format = route.source_format
                  AND locator.machine_id = route.machine_id
                  AND locator.alias_group_identity = route.alias_group_identity
                  AND locator.is_current = 1
                 WHERE route.capture_source_id = ?1
                   AND route.provider = ?2
                   AND route.source_format = ?3
                   AND route.machine_id = ?4
                   AND route.alias_group_identity = ?5
                   AND cs.provider = ?2
                   AND cs.source_format = ?3
                   AND cs.machine_id = ?4
                   AND cs.source_identity = ?6
                   AND locator.canonical_source_identity = ?6
             )",
        )?;
        Ok(statement.query_row(
            params![
                capture_source_id.to_string(),
                binding.provider.as_str(),
                binding.source_format,
                binding.machine_id,
                binding.alias_group_identity,
                binding.canonical_source_identity,
            ],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    fn insert_capture_source_provider_route_if_authorized(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<bool> {
        // The first binding is written only from the same authoritative join.
        // A zero-row result retains the complete conflict/rename slow path.
        let mut statement = self.conn.prepare_cached(
            "INSERT INTO capture_source_provider_routes
                 (capture_source_id, provider, source_format, machine_id,
                  alias_group_identity)
             SELECT cs.id, ?2, ?3, ?4, ?5
             FROM capture_sources cs
             JOIN provider_source_locators locator
               ON locator.provider = ?2
              AND locator.source_format = ?3
              AND locator.machine_id = ?4
              AND locator.canonical_source_identity = ?6
              AND locator.alias_group_identity = ?5
              AND locator.is_current = 1
             WHERE cs.id = ?1
               AND cs.provider = ?2
               AND cs.source_format = ?3
               AND cs.machine_id = ?4
               AND cs.source_identity = ?6
             ON CONFLICT(capture_source_id) DO NOTHING",
        )?;
        Ok(statement.execute(params![
            capture_source_id.to_string(),
            binding.provider.as_str(),
            binding.source_format,
            binding.machine_id,
            binding.alias_group_identity,
            binding.canonical_source_identity,
        ])? == 1)
    }

    fn bind_capture_source_provider_route_inner(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<()> {
        let expected = self
            .conn
            .query_row(
                "SELECT provider, source_format, machine_id, source_identity
                 FROM capture_sources WHERE id = ?1",
                [capture_source_id.to_string()],
                |row| {
                    Ok((
                        parse_text_enum::<CaptureProvider>(row.get::<_, String>(0)?)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let (matches_capture_source, matches_except_machine) = expected.map_or(
            (false, false),
            |(provider, source_format, machine_id, canonical_source_identity)| {
                let stable_fields_match = provider == binding.provider
                    && source_format.as_deref() == Some(binding.source_format.as_str())
                    && canonical_source_identity.as_deref()
                        == Some(binding.canonical_source_identity.as_str());
                (
                    stable_fields_match && machine_id == binding.machine_id,
                    stable_fields_match,
                )
            },
        );
        let locator_exists = self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM provider_source_locators
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND canonical_source_identity = ?4 AND alias_group_identity = ?5
                   AND is_current = 1
             )",
            params![
                binding.provider.as_str(),
                binding.source_format,
                binding.machine_id,
                binding.canonical_source_identity,
                binding.alias_group_identity,
            ],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !locator_exists {
            return Err(StoreError::CaptureSourceProviderRouteConflict { capture_source_id });
        }
        if !matches_capture_source {
            // `machine_id` is operator-configurable. A later import may rename
            // the same machine while retaining the exact path and provider
            // revision. Preserve the already-authorized route in that one
            // byte-equivalent case; all other rebinding attempts fail closed.
            let equivalent = self.equivalent_current_provider_route(capture_source_id, binding)?;
            if matches_except_machine && equivalent {
                return Ok(());
            }
            return Err(StoreError::CaptureSourceProviderRouteConflict { capture_source_id });
        }

        self.conn.execute(
            "INSERT INTO capture_source_provider_routes
             (capture_source_id, provider, source_format, machine_id, alias_group_identity)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(capture_source_id) DO NOTHING",
            params![
                capture_source_id.to_string(),
                binding.provider.as_str(),
                binding.source_format,
                binding.machine_id,
                binding.alias_group_identity,
            ],
        )?;
        let persisted = self.conn.query_row(
            "SELECT provider, source_format, machine_id, alias_group_identity
             FROM capture_source_provider_routes WHERE capture_source_id = ?1",
            [capture_source_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let proposed = (
            binding.provider.as_str().to_owned(),
            binding.source_format.clone(),
            binding.machine_id.clone(),
            binding.alias_group_identity.clone(),
        );
        if persisted != proposed {
            if self.equivalent_current_provider_route(capture_source_id, binding)? {
                self.conn.execute(
                    "UPDATE capture_source_provider_routes
                     SET provider = ?2, source_format = ?3, machine_id = ?4,
                         alias_group_identity = ?5
                     WHERE capture_source_id = ?1",
                    params![
                        capture_source_id.to_string(),
                        binding.provider.as_str(),
                        binding.source_format,
                        binding.machine_id,
                        binding.alias_group_identity,
                    ],
                )?;
                return Ok(());
            }
            return Err(StoreError::CaptureSourceProviderRouteConflict { capture_source_id });
        }
        Ok(())
    }

    fn equivalent_current_provider_route(
        &self,
        capture_source_id: Uuid,
        proposed: &ProviderSourceRouteBinding,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM capture_source_provider_routes bound
                 JOIN provider_source_locators current
                   ON current.provider = bound.provider
                  AND current.source_format = bound.source_format
                  AND current.machine_id = bound.machine_id
                  AND current.alias_group_identity = bound.alias_group_identity
                  AND current.is_current = 1
                 JOIN provider_source_locators candidate
                   ON candidate.provider = ?2
                  AND candidate.source_format = ?3
                  AND candidate.machine_id = ?4
                  AND candidate.alias_group_identity = ?5
                  AND candidate.is_current = 1
                 WHERE bound.capture_source_id = ?1
                   AND current.canonical_source_identity = ?6
                   AND candidate.canonical_source_identity = ?6
                   AND current.raw_source_path IS NOT NULL
                   AND current.raw_source_path = candidate.raw_source_path
                   AND current.source_revision = candidate.source_revision
             )",
            params![
                capture_source_id.to_string(),
                proposed.provider.as_str(),
                proposed.source_format,
                proposed.machine_id,
                proposed.alias_group_identity,
                proposed.canonical_source_identity,
            ],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    /// Resolves the one current physical provider source authorized for an
    /// event. Historical capture paths are deliberately never consulted.
    pub fn authorized_source_route_for_event(
        &self,
        event_id: Uuid,
    ) -> Result<AuthorizedSourceRoute> {
        let mut statement = self.conn.prepare(
            "SELECT e.id, cs.id, cs.provider, cs.source_format, cs.machine_id,
                    cs.source_identity, locator.raw_source_path, locator.source_revision
             FROM events e
             JOIN capture_sources cs ON cs.id = e.capture_source_id
             JOIN capture_source_provider_routes route ON route.capture_source_id = cs.id
             JOIN provider_source_locators locator
               ON locator.provider = route.provider
              AND locator.source_format = route.source_format
              AND locator.machine_id = route.machine_id
              AND locator.alias_group_identity = route.alias_group_identity
              AND locator.is_current = 1
             WHERE e.id = ?1
               AND e.deleted_at_ms IS NULL
               AND route.provider = cs.provider
               AND route.source_format = cs.source_format
               AND route.machine_id = cs.machine_id
               AND locator.canonical_source_identity = cs.source_identity
               AND locator.raw_source_path IS NOT NULL
               AND locator.raw_source_path <> ''
             ORDER BY locator.locator_identity
             LIMIT 2",
        )?;
        let rows = statement.query_map([event_id.to_string()], authorized_source_route_row)?;
        let mut routes = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        match routes.len() {
            0 => Err(StoreError::AuthorizedSourceRouteUnavailable { event_id }),
            1 => Ok(routes.pop().expect("one route was counted")),
            _ => Err(StoreError::AuthorizedSourceRouteAmbiguous { event_id }),
        }
    }

    /// Reports whether an exact provider path was previously backed by local
    /// route authority.
    ///
    /// Current authority requires the live locator and capture-source binding.
    /// Historical authority is retained only after clean route retirement:
    /// relocated stale aliases remain ineligible while their alias group still
    /// has a current locator or route binding.
    pub fn has_prior_provider_source_route(
        &self,
        provider: CaptureProvider,
        source_format: &str,
        requested_path: &Path,
    ) -> Result<bool> {
        let Some(requested_path) = requested_path.to_str() else {
            return Ok(false);
        };
        let matched = self.conn.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM provider_source_locators locator
                 JOIN capture_sources source
                   ON source.provider = locator.provider
                  AND source.source_format = locator.source_format
                  AND source.machine_id = locator.machine_id
                  AND source.source_identity = locator.canonical_source_identity
                 LEFT JOIN capture_source_provider_routes route
                   ON route.capture_source_id = source.id
                  AND route.provider = locator.provider
                  AND route.source_format = locator.source_format
                  AND route.machine_id = locator.machine_id
                  AND route.alias_group_identity = locator.alias_group_identity
                 WHERE locator.provider = ?1
                   AND locator.source_format = ?2
                   AND locator.raw_source_path IS NOT NULL
                   AND locator.raw_source_path <> ''
                   AND (
                       locator.raw_source_path = ?3
                       OR source.source_root = ?3
                   )
                   AND (
                       (
                           locator.is_current = 1
                           AND route.capture_source_id IS NOT NULL
                       )
                       OR (
                           locator.is_current = 0
                           AND route.capture_source_id IS NULL
                           AND source.raw_source_path = locator.raw_source_path
                           AND NOT EXISTS (
                               SELECT 1
                               FROM provider_source_locators current
                               WHERE current.provider = locator.provider
                                 AND current.source_format = locator.source_format
                                 AND current.machine_id = locator.machine_id
                                 AND current.alias_group_identity =
                                     locator.alias_group_identity
                                 AND current.is_current = 1
                           )
                           AND NOT EXISTS (
                               SELECT 1
                               FROM capture_source_provider_routes current_route
                               WHERE current_route.provider = locator.provider
                                 AND current_route.source_format =
                                     locator.source_format
                                 AND current_route.machine_id = locator.machine_id
                                 AND current_route.alias_group_identity =
                                     locator.alias_group_identity
                           )
                           AND locator.observed_at_ms = (
                               SELECT MAX(retired.observed_at_ms)
                               FROM provider_source_locators retired
                               WHERE retired.provider = locator.provider
                                 AND retired.source_format = locator.source_format
                                 AND retired.machine_id = locator.machine_id
                                 AND retired.alias_group_identity =
                                     locator.alias_group_identity
                           )
                           AND 1 = (
                               SELECT COUNT(*)
                               FROM provider_source_locators retired
                               WHERE retired.provider = locator.provider
                                 AND retired.source_format = locator.source_format
                                 AND retired.machine_id = locator.machine_id
                                 AND retired.alias_group_identity =
                                     locator.alias_group_identity
                                 AND retired.observed_at_ms = locator.observed_at_ms
                           )
                       )
                   )
                 LIMIT 1
             )",
            params![provider.as_str(), source_format, requested_path],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(matched != 0)
    }
}
