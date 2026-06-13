use super::*;

mod secrets;

mod types;
pub use types::*;
impl Store {
    // Mobile connection profiles + devices
    pub async fn create_mobile_connection_profile(
        &self,
        label: String,
        base_url: String,
        token_hash: String,
        token_prefix: String,
        scopes: Vec<String>,
    ) -> Result<MobileConnectionProfile> {
        let now = Utc::now();
        let profile = MobileConnectionProfile {
            id: ConnectionProfileId::new(),
            label,
            base_url,
            token_prefix,
            scopes,
            created_at: now,
            last_used_at: None,
        };
        let scopes_json = serde_json::to_string(&profile.scopes)?;
        self.query(
            r#"INSERT INTO mobile_connection_profiles
               (id, label, base_url, token_hash, token_prefix, scopes_json, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(profile.id.0.to_string())
        .bind(&profile.label)
        .bind(&profile.base_url)
        .bind(&token_hash)
        .bind(&profile.token_prefix)
        .bind(scopes_json)
        .bind(profile.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(profile)
    }

    pub async fn list_mobile_connection_profiles(&self) -> Result<Vec<MobileConnectionProfile>> {
        let rows = self
            .query(
                r#"SELECT id, label, base_url, token_prefix, scopes_json, created_at, last_used_at
               FROM mobile_connection_profiles
               ORDER BY created_at DESC"#,
            )
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(build_mobile_connection_profile_from_row(row)?);
        }
        Ok(out)
    }

    pub async fn get_mobile_connection_profile(
        &self,
        id: ConnectionProfileId,
    ) -> Result<Option<MobileConnectionProfile>> {
        let row = self
            .query(
                r#"SELECT id, label, base_url, token_prefix, scopes_json, created_at, last_used_at
               FROM mobile_connection_profiles WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(build_mobile_connection_profile_from_row)
            .transpose()
    }

    pub async fn get_mobile_connection_profile_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<MobileConnectionProfile>> {
        let row = self
            .query(
                r#"SELECT id, label, base_url, token_prefix, scopes_json, created_at, last_used_at
               FROM mobile_connection_profiles WHERE token_hash = ?"#,
            )
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await?;
        row.map(build_mobile_connection_profile_from_row)
            .transpose()
    }

    pub async fn mark_mobile_connection_profile_used(&self, id: ConnectionProfileId) -> Result<()> {
        self.query(r#"UPDATE mobile_connection_profiles SET last_used_at = ? WHERE id = ?"#)
            .bind(Utc::now().to_rfc3339())
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_mobile_connection_profile_scopes(
        &self,
        id: ConnectionProfileId,
        scopes: Vec<String>,
    ) -> Result<()> {
        let scopes_json = serde_json::to_string(&scopes)?;
        self.query(r#"UPDATE mobile_connection_profiles SET scopes_json = ? WHERE id = ?"#)
            .bind(scopes_json)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_mobile_connection_profile(&self, id: ConnectionProfileId) -> Result<()> {
        self.query(r#"DELETE FROM mobile_connection_profiles WHERE id = ?"#)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_runtime_settings_document(&self) -> Result<Option<RuntimeSettingsDocument>> {
        let row = self
            .query(
                r#"SELECT id, schema_version, settings_json, secret_ref, updated_at
               FROM runtime_settings
               WHERE id = ?"#,
            )
            .bind("default")
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        Ok(Some(RuntimeSettingsDocument {
            id: row.try_get("id")?,
            schema_version: row.try_get("schema_version")?,
            settings_json: row.try_get("settings_json")?,
            secret_ref: row.try_get("secret_ref")?,
            updated_at: parse_dt(&row.try_get::<String, _>("updated_at")?)?,
        }))
    }

    pub async fn upsert_runtime_settings_document(
        &self,
        schema_version: i64,
        settings_json: &str,
    ) -> Result<RuntimeSettingsDocument> {
        let _write_guard = self.write_gate.lock().await;
        let old_secret_ref = self.lookup_runtime_settings_secret_ref().await?;
        let updated_at = Utc::now().to_rfc3339();
        self.query(
            r#"INSERT INTO runtime_settings (id, schema_version, settings_json, secret_ref, updated_at)
               VALUES (?, ?, ?, NULL, ?)
               ON CONFLICT(id) DO UPDATE SET
                   schema_version = excluded.schema_version,
                   settings_json = excluded.settings_json,
                   secret_ref = excluded.secret_ref,
                   updated_at = excluded.updated_at"#,
        )
        .bind("default")
        .bind(schema_version)
        .bind(settings_json)
        .bind(&updated_at)
        .execute(&self.pool)
        .await?;
        if let Some(old_secret_ref) = old_secret_ref {
            self.remove_runtime_settings_secrets_if_present(&old_secret_ref)
                .await?;
        }

        self.get_runtime_settings_document()
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to read back runtime settings"))
    }

    pub async fn upsert_runtime_settings_document_with_secrets(
        &self,
        schema_version: i64,
        settings_json: &str,
        settings_secret_json: &str,
    ) -> Result<RuntimeSettingsDocument> {
        let _write_guard = self.write_gate.lock().await;
        let old_secret_ref = self.lookup_runtime_settings_secret_ref().await?;
        let new_secret_ref = Self::next_runtime_settings_secret_ref();
        self.write_runtime_settings_secrets(&new_secret_ref, settings_secret_json)
            .await?;
        let updated_at = Utc::now().to_rfc3339();
        let upsert_result = self
            .query(
                r#"INSERT INTO runtime_settings (id, schema_version, settings_json, secret_ref, updated_at)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   schema_version = excluded.schema_version,
                   settings_json = excluded.settings_json,
                   secret_ref = excluded.secret_ref,
                   updated_at = excluded.updated_at"#,
            )
            .bind("default")
            .bind(schema_version)
            .bind(settings_json)
            .bind(&new_secret_ref)
            .bind(&updated_at)
            .execute(&self.pool)
            .await;
        if let Err(err) = upsert_result {
            let _ = self
                .remove_runtime_settings_secrets_if_present(&new_secret_ref)
                .await;
            return Err(err.into());
        }
        if let Some(old_secret_ref) = old_secret_ref {
            if old_secret_ref != new_secret_ref {
                self.remove_runtime_settings_secrets_if_present(&old_secret_ref)
                    .await?;
            }
        }

        self.get_runtime_settings_document()
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to read back runtime settings"))
    }

    pub async fn get_mobile_access_config(&self) -> Result<Option<MobileAccessConfig>> {
        let row = self
            .query(
                r#"SELECT id, profile_id, tunnel_id, public_base_url, relay_base_url, secret_ref, tunnel_secret,
                      daemon_public_key, daemon_private_key, enabled, created_at, updated_at
               FROM mobile_access_config
               WHERE id = ?"#,
            )
            .bind("default")
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let id: String = row.try_get("id")?;
        let secret_ref: Option<String> = row.try_get("secret_ref")?;
        let tunnel_secret: String = row.try_get("tunnel_secret")?;
        let daemon_private_key: String = row.try_get("daemon_private_key")?;
        let (tunnel_secret, daemon_private_key) = match secret_ref {
            Some(secret_ref) => match self
                .read_mobile_access_secrets_if_present(&secret_ref)
                .await?
            {
                Some(secrets) => {
                    if !tunnel_secret.is_empty() || !daemon_private_key.is_empty() {
                        self.clear_legacy_mobile_access_secrets(&id).await?;
                    }
                    if id != secret_ref {
                        self.remove_mobile_access_secrets_if_present(&id).await?;
                    }
                    secrets
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "mobile access secrets are missing for config {} (secret_ref={})",
                        id,
                        secret_ref
                    ));
                }
            },
            None => {
                if let Some(secrets) = self.migrate_legacy_mobile_access_sidecar(&id).await? {
                    secrets
                } else {
                    self.migrate_legacy_mobile_access_secrets(
                        &id,
                        &tunnel_secret,
                        &daemon_private_key,
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "mobile access secrets are missing for config {} and no legacy secrets remain",
                            id
                        )
                    })?
                }
            }
        };
        Ok(Some(MobileAccessConfig {
            id,
            profile_id: ConnectionProfileId(uuid::Uuid::parse_str(
                &row.try_get::<String, _>("profile_id")?,
            )?),
            tunnel_id: row.try_get("tunnel_id")?,
            public_base_url: row.try_get("public_base_url")?,
            relay_base_url: row.try_get("relay_base_url")?,
            tunnel_secret,
            daemon_public_key: row.try_get("daemon_public_key")?,
            daemon_private_key,
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            created_at: parse_dt(&row.try_get::<String, _>("created_at")?)?,
            updated_at: parse_dt(&row.try_get::<String, _>("updated_at")?)?,
        }))
    }

    pub async fn upsert_mobile_access_config(
        &self,
        config: MobileAccessConfig,
    ) -> Result<MobileAccessConfig> {
        let _write_guard = self.write_gate.lock().await;
        let old_secret_ref = self.lookup_mobile_access_secret_ref(&config.id).await?;
        let new_secret_ref = Self::next_mobile_access_secret_ref();
        self.write_mobile_access_secrets(
            &new_secret_ref,
            &config.tunnel_secret,
            &config.daemon_private_key,
        )
        .await?;
        let created_at = config.created_at.to_rfc3339();
        let updated_at = Utc::now().to_rfc3339();
        let upsert_result = self
            .query(
                r#"INSERT INTO mobile_access_config
                    (id, profile_id, tunnel_id, public_base_url, relay_base_url, secret_ref, tunnel_secret, daemon_public_key, daemon_private_key, enabled, created_at, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(id) DO UPDATE SET
                        profile_id=excluded.profile_id,
                        tunnel_id=excluded.tunnel_id,
                        public_base_url=excluded.public_base_url,
                        relay_base_url=excluded.relay_base_url,
                        secret_ref=excluded.secret_ref,
                        tunnel_secret=excluded.tunnel_secret,
                        daemon_public_key=excluded.daemon_public_key,
                        daemon_private_key=excluded.daemon_private_key,
                        enabled=excluded.enabled,
                        updated_at=excluded.updated_at"#,
            )
            .bind(&config.id)
            .bind(config.profile_id.0.to_string())
            .bind(&config.tunnel_id)
            .bind(&config.public_base_url)
            .bind(&config.relay_base_url)
            .bind(&new_secret_ref)
            .bind("")
            .bind(&config.daemon_public_key)
            .bind("")
            .bind(if config.enabled { 1 } else { 0 })
            .bind(created_at)
            .bind(updated_at)
            .execute(&self.pool)
            .await;
        if let Err(err) = upsert_result {
            let _ = self
                .remove_mobile_access_secrets_if_present(&new_secret_ref)
                .await;
            return Err(err.into());
        }
        if let Some(old_secret_ref) = old_secret_ref {
            if old_secret_ref != new_secret_ref {
                self.remove_mobile_access_secrets_if_present(&old_secret_ref)
                    .await?;
            }
        }
        if config.id != new_secret_ref {
            self.remove_mobile_access_secrets_if_present(&config.id)
                .await?;
        }

        self.get_mobile_access_config()
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to read back mobile access config"))
    }

    pub async fn set_mobile_access_enabled(&self, enabled: bool) -> Result<()> {
        self.query(r#"UPDATE mobile_access_config SET enabled = ?, updated_at = ? WHERE id = ?"#)
            .bind(if enabled { 1 } else { 0 })
            .bind(Utc::now().to_rfc3339())
            .bind("default")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_mobile_access_config(&self) -> Result<()> {
        let secret_ref = self.lookup_mobile_access_secret_ref("default").await?;
        self.query(r#"DELETE FROM mobile_access_config WHERE id = ?"#)
            .bind("default")
            .execute(&self.pool)
            .await?;
        if let Some(secret_ref) = secret_ref {
            self.remove_mobile_access_secrets_if_present(&secret_ref)
                .await?;
        }
        self.remove_mobile_access_secrets_if_present("default")
            .await?;
        Ok(())
    }

    pub async fn insert_mobile_pairing_token(
        &self,
        token_id: &str,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        self.query(
            r#"INSERT INTO mobile_pairing_tokens
                (id, token_hash, created_at, expires_at)
               VALUES (?, ?, ?, ?)"#,
        )
        .bind(token_id)
        .bind(token_hash)
        .bind(Utc::now().to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn consume_mobile_pairing_token(&self, token_hash: &str) -> Result<bool> {
        let row = self
            .query(r#"SELECT id, expires_at FROM mobile_pairing_tokens WHERE token_hash = ?"#)
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else {
            return Ok(false);
        };
        let expires_at: String = row.try_get("expires_at")?;
        let expires_at = parse_dt(&expires_at)?;
        if expires_at < Utc::now() {
            let _ = self
                .query(r#"DELETE FROM mobile_pairing_tokens WHERE token_hash = ?"#)
                .bind(token_hash)
                .execute(&self.pool)
                .await;
            return Ok(false);
        }

        self.query(r#"DELETE FROM mobile_pairing_tokens WHERE token_hash = ?"#)
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    pub async fn clear_mobile_pairing_tokens(&self) -> Result<()> {
        self.query(r#"DELETE FROM mobile_pairing_tokens"#)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn advance_mobile_device_seq(
        &self,
        id: MobileDeviceId,
        seq: i64,
    ) -> Result<MobileDeviceSeqAdvance> {
        let _write_guard = self.write_gate.lock().await;
        let mut tx = self.pool.begin().await?;
        let row = self
            .query(r#"SELECT last_seen_seq FROM mobile_devices WHERE id = ?"#)
            .bind(id.0.to_string())
            .fetch_optional(&mut *tx)
            .await?;

        let Some(row) = row else {
            return Ok(MobileDeviceSeqAdvance::Missing);
        };

        let last_seen: Option<i64> = row.try_get("last_seen_seq").ok();
        if let Some(current) = last_seen {
            if seq <= current {
                return Ok(MobileDeviceSeqAdvance::Stale { current });
            }
        }
        self.query(
            r#"UPDATE mobile_devices
               SET last_seen_seq = ?, last_seen_at = ?
               WHERE id = ?"#,
        )
        .bind(seq)
        .bind(Utc::now().to_rfc3339())
        .bind(id.0.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(MobileDeviceSeqAdvance::Advanced)
    }

    pub async fn upsert_mobile_device(
        &self,
        id: MobileDeviceId,
        profile_id: ConnectionProfileId,
        update: MobileDeviceUpsert,
    ) -> Result<MobileDeviceRegistration> {
        let MobileDeviceUpsert {
            device_label,
            platform,
            push_token,
            push_provider,
            public_key,
            app_version,
        } = update;
        let now = Utc::now();
        self.query(
            r#"INSERT INTO mobile_devices
                (id, profile_id, device_label, platform, push_token, push_provider, public_key, app_version, created_at, last_seen_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    device_label=excluded.device_label,
                    platform=excluded.platform,
                    push_token=excluded.push_token,
                    push_provider=excluded.push_provider,
                    public_key=excluded.public_key,
                    app_version=excluded.app_version,
                    last_seen_at=excluded.last_seen_at"#,
        )
        .bind(id.0.to_string())
        .bind(profile_id.0.to_string())
        .bind(device_label.clone())
        .bind(platform.clone())
        .bind(push_token.clone())
        .bind(push_provider.clone())
        .bind(public_key.clone())
        .bind(app_version.clone())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;

        self.get_mobile_device(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to read back mobile device {}", id.0))
    }

    pub async fn get_mobile_device(
        &self,
        id: MobileDeviceId,
    ) -> Result<Option<MobileDeviceRegistration>> {
        let row = self.query(
            r#"SELECT id, profile_id, device_label, platform, push_token, push_provider, public_key, app_version, created_at, last_seen_at
               FROM mobile_devices WHERE id = ?"#,
        )
        .bind(id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(build_mobile_device_from_row).transpose()
    }

    pub async fn list_mobile_devices(
        &self,
        profile_id: ConnectionProfileId,
    ) -> Result<Vec<MobileDeviceRegistration>> {
        let rows = self.query(
            r#"SELECT id, profile_id, device_label, platform, push_token, push_provider, public_key, app_version, created_at, last_seen_at
               FROM mobile_devices WHERE profile_id = ? ORDER BY created_at DESC"#,
        )
        .bind(profile_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(build_mobile_device_from_row(row)?);
        }
        Ok(out)
    }
}
