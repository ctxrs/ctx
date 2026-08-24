use super::*;

pub fn published_refresh_receipt_for_index(
    response: &Value,
    verified_index: &VerifiedIndex,
) -> Result<SourceBackedRefreshReceipt> {
    parse_published_refresh_receipt(response, Some(verified_index))
}

/// Parses and validates the durable terminal receipt shape without requiring
/// the disposable lexical generation to be readable. Recovery uses this
/// before replacing an incompatible active generation.
pub fn published_refresh_receipt_for_recovery(
    response: &Value,
) -> Result<SourceBackedRefreshReceipt> {
    parse_published_refresh_receipt(response, None)
}

fn parse_published_refresh_receipt(
    response: &Value,
    verified_index: Option<&VerifiedIndex>,
) -> Result<SourceBackedRefreshReceipt> {
    let value = response
        .get("receipt")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("published daemon source refresh has no terminal receipt"))?;
    let previous_generation = optional_generation(value.get("previous_generation"))?;
    let published_generation = required_generation(
        value.get("published_generation"),
        "terminal receipt published generation",
    )?;
    let generation_changed = value
        .get("generation_changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no generation_changed fact")
        })?;
    let published_explicit_source_catalog = value
        .get("published_explicit_source_catalog")
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
    let current_value = value
        .get("current")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no current generation facts")
        })?;
    let current = SourceBackedRefreshCurrent {
        source_count: required_usize(current_value, "current_source_count")?,
        indexed_documents: required_u64(current_value, "current_indexed_documents")?,
        complete_records: required_u64(current_value, "current_complete_records")?,
        retained_records: required_u64(current_value, "current_retained_records")?,
        rejected_records: required_u64(current_value, "current_rejected_records")?,
        ignored_records: required_u64(current_value, "current_ignored_records")?,
        certified_source_bytes: required_u64(current_value, "current_certified_source_bytes")?,
        sources_with_rejections: required_usize(current_value, "current_sources_with_rejections")?,
        removed_source_count: required_usize(current_value, "removed_source_count")?,
    };
    if current.source_count
        != required_usize_from_value(
            response.get("certified_source_count"),
            "certified_source_count",
        )?
        || current.certified_source_bytes
            != required_u64_from_value(
                response.get("certified_source_bytes"),
                "certified_source_bytes",
            )?
    {
        bail!("published daemon source refresh receipt does not match its certified current facts");
    }
    let selected_route_total = required_usize(value, "selected_route_total")?;
    let successful_route_total = required_usize(value, "successful_route_total")?;
    let route_results = required_route_results(value.get("route_results"))?;
    let zero_source_authority =
        parse_zero_source_authority(value.get("zero_source_authority"), &route_results)?;
    let expected_catalog_lineages = published_explicit_source_catalog
        .as_ref()
        .map(ExplicitSourceCatalogAuthority::route_lineages)
        .unwrap_or_default();
    let catalog_route_bindings = match verified_index {
        Some(index) => required_catalog_route_bindings(
            value.get("catalog_route_bindings"),
            index.manifest(),
            &route_results,
            &expected_catalog_lineages,
        )?,
        None => required_catalog_route_bindings_shape(
            value.get("catalog_route_bindings"),
            &route_results,
            &expected_catalog_lineages,
        )?,
    };
    let actual_catalog_lineages = catalog_route_bindings
        .iter()
        .map(|binding| binding.catalog_lineage.clone())
        .collect::<BTreeSet<_>>();
    let derived_successful_route_total = route_results
        .iter()
        .filter(|result| result.outcome.is_success())
        .count();
    let derived_source_failure_total =
        route_results.iter().try_fold(0_usize, |total, result| {
            total
                .checked_add(result.source_failure_total)
                .ok_or_else(|| anyhow!("published daemon source-failure total overflow"))
        })?;
    let source_failure_diagnostic_total =
        route_results.iter().try_fold(0_usize, |total, result| {
            total
                .checked_add(result.source_failures.len())
                .ok_or_else(|| anyhow!("published daemon source-failure diagnostic total overflow"))
        })?;
    let derived_rejected_record_total = route_results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(result.rejected_record_total)
            .ok_or_else(|| anyhow!("published daemon rejected-record total overflow"))
    })?;
    let rejection_diagnostic_total = route_results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(result.rejection_diagnostics.len() as u64)
            .ok_or_else(|| anyhow!("published daemon rejection diagnostic total overflow"))
    })?;
    let source_failure_total = required_usize(value, "source_failure_total")?;
    let source_failures_omitted = required_usize(value, "source_failures_omitted")?;
    let rejected_record_total = required_u64(value, "rejected_record_total")?;
    let rejection_diagnostics_omitted = required_u64(value, "rejection_diagnostics_omitted")?;
    if selected_route_total != route_results.len()
        || successful_route_total != derived_successful_route_total
        || source_failure_total != derived_source_failure_total
        || source_failures_omitted
            != source_failure_total.saturating_sub(source_failure_diagnostic_total)
        || rejected_record_total != derived_rejected_record_total
        || rejected_record_total > current.rejected_records
        || rejection_diagnostics_omitted
            != rejected_record_total.saturating_sub(rejection_diagnostic_total)
        || !expected_catalog_lineages.is_subset(&actual_catalog_lineages)
    {
        bail!("published daemon source refresh has an invalid route-result partition");
    }
    validate_zero_source_authority(
        &published_generation,
        current.source_count,
        &route_results,
        &zero_source_authority,
        false,
    )?;

    let top_previous_generation = optional_generation(response.get("previous_generation"))?;
    let top_published_generation = required_generation(
        response.get("published_generation"),
        "published daemon source refresh generation",
    )?;
    let top_generation_changed = response
        .get("generation_changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("published daemon source refresh has no generation_changed fact"))?;
    let identity_changed = previous_generation.as_deref() != Some(published_generation.as_str());
    let request_identity_changed =
        top_previous_generation.as_deref() != Some(top_published_generation.as_str());
    if published_generation != top_published_generation
        || generation_changed != identity_changed
        || top_generation_changed != request_identity_changed
    {
        bail!(
            "published daemon source refresh receipt has inconsistent publication identity facts"
        );
    }

    if let Some(verified_index) = verified_index {
        let manifest = verified_index.manifest();
        let verified_current = SourceBackedRefreshCurrent::from_sources(
            &manifest.sources,
            current.removed_source_count,
        )?;
        if current != verified_current {
            bail!(
                "published daemon source refresh receipt does not match the verified current generation"
            );
        }
    }

    Ok(SourceBackedRefreshReceipt {
        previous_generation,
        published_generation,
        generation_changed,
        published_explicit_source_catalog,
        current,
        route_results,
        zero_source_authority,
        catalog_route_bindings,
    })
}

#[doc(hidden)]
pub fn required_route_results(
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshRouteResult>> {
    let value = value
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has no route_results"))?;
    let values = value.as_object().ok_or_else(|| {
        anyhow!("published daemon source refresh receipt route_results must be an object")
    })?;
    if values.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!(
            "published daemon source refresh exceeds the bounded route-result limit of {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT}"
        );
    }
    values
        .iter()
        .map(|(route_identity, value)| {
            let parsed_route_identity = SourceRouteIdentity::from_sha256(route_identity.clone())
                .map_err(|_| {
                    anyhow!("published daemon source refresh route identity is invalid")
                })?;
            let fields = value.as_array().ok_or_else(|| {
                anyhow!("published daemon source refresh compact route result must be an array")
            })?;
            let (
                outcome,
                source_failure_total,
                source_retryable_failure_total,
                source_failures,
                rejected_record_total,
                rejection_diagnostics,
            ) = match fields.first().and_then(Value::as_str) {
                Some("s") if fields.len() == 2 => {
                    let changed = fields[1].as_bool().ok_or_else(|| {
                        anyhow!("published daemon successful route result has no changed fact")
                    })?;
                    (
                        SourceBackedRefreshRouteOutcome::Succeeded { changed },
                        0,
                        0,
                        Vec::new(),
                        0,
                        Vec::new(),
                    )
                }
                Some("s") if matches!(fields.len(), 6 | 7) => {
                    let changed = fields[1].as_bool().ok_or_else(|| {
                        anyhow!("published daemon successful route result has no changed fact")
                    })?;
                    let total =
                        required_usize_from_value(fields.get(2), "route source_failure_total")?;
                    let has_exact_retryability = fields.len() == 7;
                    let diagnostic_index = usize::from(has_exact_retryability) + 3;
                    let failures = required_route_source_failures(
                        parsed_route_identity.as_str(),
                        fields.get(diagnostic_index),
                    )?;
                    let retryable_total = if has_exact_retryability {
                        required_usize_from_value(
                            fields.get(3),
                            "route source_retryable_failure_total",
                        )?
                    } else {
                        // Older receipts remain conservative when bounded
                        // diagnostics could hide a transient failure.
                        failures
                            .iter()
                            .filter(|failure| {
                                matches!(failure.class.as_str(), "unavailable" | "source_changed")
                            })
                            .count()
                            .saturating_add(total.saturating_sub(failures.len()))
                    };
                    let rejected_record_total = required_u64_from_value(
                        fields.get(diagnostic_index + 1),
                        "route rejected_record_total",
                    )?;
                    let rejection_diagnostics = required_route_rejection_diagnostics(
                        parsed_route_identity.as_str(),
                        fields.get(diagnostic_index + 2),
                    )?;
                    (
                        SourceBackedRefreshRouteOutcome::Succeeded { changed },
                        total,
                        retryable_total,
                        failures,
                        rejected_record_total,
                        rejection_diagnostics,
                    )
                }
                Some("f") if fields.len() == 5 => {
                    let class = compact_source_failure_class(fields[1].as_str())?;
                    let carried_forward = fields[2].as_bool().ok_or_else(|| {
                        anyhow!("published daemon failed route result has no carried-forward fact")
                    })?;
                    let total =
                        required_usize_from_value(fields.get(3), "route source_failure_total")?;
                    let failures = required_route_source_failures(
                        parsed_route_identity.as_str(),
                        fields.get(4),
                    )?;
                    (
                        SourceBackedRefreshRouteOutcome::Failed {
                            class,
                            carried_forward,
                        },
                        total,
                        if matches!(fields[1].as_str(), Some("u") | Some("c")) {
                            total
                        } else {
                            0
                        },
                        failures,
                        0,
                        Vec::new(),
                    )
                }
                _ => bail!("published daemon source refresh route result has inconsistent fields"),
            };
            let result = SourceBackedRefreshRouteResult {
                route_identity: parsed_route_identity.as_str().to_owned(),
                outcome,
                source_failure_total,
                source_retryable_failure_total,
                source_failures,
                rejected_record_total,
                rejection_diagnostics,
            };
            result.validate_source_failures()?;
            Ok(result)
        })
        .collect()
}

#[doc(hidden)]
pub fn zero_source_authority_json(
    authority: &[SourceBackedZeroSourceAuthority],
    route_results: &[SourceBackedRefreshRouteResult],
) -> Option<Value> {
    let generation_id = authority.first()?.generation_id.clone();
    let authority = authority
        .iter()
        .map(|entry| (entry.route_identity.as_str(), entry.kind))
        .collect::<BTreeMap<_, _>>();
    let mut route_results = route_results.iter().collect::<Vec<_>>();
    route_results.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
    // The disposition string is positionally bound to the sorted route-result
    // identities: `e` is complete-empty inventory and `d` is confirmed
    // deletion. This avoids repeating 64-byte route IDs and keeps the full
    // bounded route set inside the durable receipt budget.
    let route_kinds = route_results
        .iter()
        .filter_map(|result| authority.get(result.route_identity.as_str()))
        .map(|kind| kind.compact_code())
        .collect::<String>();
    Some(json!({
        "generation_id": generation_id,
        "route_kinds": route_kinds,
    }))
}

#[doc(hidden)]
pub fn parse_zero_source_authority(
    value: Option<&Value>,
    route_results: &[SourceBackedRefreshRouteResult],
) -> Result<Vec<SourceBackedZeroSourceAuthority>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("Core zero-source authority must be an object"))?;
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["generation_id", "route_kinds"])
    {
        bail!("Core zero-source authority has unknown or missing fields");
    }
    let generation_id = fields
        .get("generation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Core zero-source authority has no generation binding"))?;
    let route_kinds = fields
        .get("route_kinds")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Core zero-source authority has no route entries"))?;
    let route_kinds = route_kinds.chars().collect::<Vec<_>>();
    if route_kinds.is_empty()
        || route_kinds.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
        || route_kinds.len() != route_results.len()
    {
        bail!(
            "Core zero-source authority must contain 1..={SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
        );
    }
    let mut route_results = route_results.iter().collect::<Vec<_>>();
    route_results.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
    route_results
        .into_iter()
        .zip(route_kinds)
        .map(|(result, kind)| {
            Ok(SourceBackedZeroSourceAuthority {
                generation_id: generation_id.to_owned(),
                route_identity: SourceRouteIdentity::from_sha256(result.route_identity.clone())?,
                kind: SourceBackedZeroSourceAuthorityKind::from_compact_code(kind)?,
            })
        })
        .collect()
}

#[doc(hidden)]
pub fn validate_zero_source_authority(
    generation_id: &str,
    source_count: usize,
    route_results: &[SourceBackedRefreshRouteResult],
    authority: &[SourceBackedZeroSourceAuthority],
    required_for_empty: bool,
) -> Result<()> {
    if source_count != 0 {
        if !authority.is_empty() {
            bail!("nonempty Core generation carries zero-source authority");
        }
        return Ok(());
    }
    if authority.is_empty() {
        if required_for_empty {
            bail!("zero-source Core generation has no publication authority");
        }
        return Ok(());
    }
    if authority.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
        || authority
            .iter()
            .any(|entry| entry.generation_id != generation_id)
    {
        bail!("Core zero-source authority is not bound to its exact generation");
    }
    let authority_routes = authority
        .iter()
        .map(|entry| entry.route_identity.as_str())
        .collect::<BTreeSet<_>>();
    if authority_routes.len() != authority.len() {
        bail!("Core zero-source authority contains a duplicate route");
    }
    let successful_routes = route_results
        .iter()
        .filter(|result| result.outcome.is_success())
        .map(|result| result.route_identity.as_str())
        .collect::<BTreeSet<_>>();
    if successful_routes.len() != route_results.len() || successful_routes != authority_routes {
        bail!("Core zero-source authority does not cover every successful terminal route");
    }
    Ok(())
}

fn required_route_rejection_diagnostics(
    route_identity: &str,
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshRecordRejection>> {
    let value =
        value.ok_or_else(|| anyhow!("terminal route result has no rejection diagnostics"))?;
    value
        .as_array()
        .ok_or_else(|| anyhow!("terminal route result rejection diagnostics must be an array"))?
        .iter()
        .map(|value| {
            let fields = value
                .as_array()
                .filter(|fields| fields.len() == 7)
                .ok_or_else(|| {
                    anyhow!("daemon source refresh compact rejection diagnostic is malformed")
                })?;
            let required = |index: usize, field: &'static str| {
                fields[index]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh rejection diagnostic has no {field}")
                    })
            };
            Ok(SourceBackedRefreshRecordRejection {
                route_identity: route_identity.to_owned(),
                source_identity: required(0, "source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required(1, "provider")?,
                source_selector: required(2, "source_selector")?,
                line: required_u64_from_value(fields.get(3), "rejection line")?,
                payload_type: required(4, "payload_type")?,
                class: compact_record_rejection_class(fields[5].as_str())?,
                detail: required(6, "detail")?,
            })
        })
        .collect()
}

fn compact_record_rejection_class(value: Option<&str>) -> Result<String> {
    Ok(match value {
        Some("m") => "malformed_record",
        Some("u") => "unsupported_record",
        _ => bail!("published daemon source refresh record rejection class is invalid"),
    }
    .to_owned())
}

fn required_catalog_route_bindings(
    value: Option<&Value>,
    manifest: &GenerationManifest,
    route_results: &[SourceBackedRefreshRouteResult],
    expected_catalog_lineages: &BTreeSet<String>,
) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
    let values = value
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no catalog_route_bindings")
        })?
        .as_object()
        .ok_or_else(|| {
            anyhow!(
                "published daemon source refresh receipt catalog_route_bindings must be an object"
            )
        })?;
    let retained = manifest
        .source_routes()
        .iter()
        .map(|route| route.route_identity().as_str())
        .collect::<BTreeSet<_>>();
    values
        .iter()
        .map(|(catalog_lineage, route_identity)| {
            if !is_sha256_identity(catalog_lineage) {
                bail!("published daemon source refresh catalog lineage is invalid");
            }
            let route_identity = route_identity.as_str().ok_or_else(|| {
                anyhow!("published daemon source refresh catalog binding route is invalid")
            })?;
            let retained_witness = expected_catalog_lineages.contains(catalog_lineage)
                && retained.contains(route_identity);
            let transient_request_failure = !expected_catalog_lineages.contains(catalog_lineage)
                && route_results.iter().any(|result| {
                    result.route_identity == route_identity
                        && matches!(
                            result.outcome,
                            SourceBackedRefreshRouteOutcome::Failed {
                                carried_forward,
                                ..
                            } if carried_forward == retained.contains(route_identity)
                        )
                });
            if !retained_witness && !transient_request_failure {
                bail!("published daemon source refresh catalog binding is neither a retained witness nor a consistent request failure");
            }
            Ok(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: catalog_lineage.clone(),
                route_identity: route_identity.to_owned(),
            })
        })
        .collect()
}

fn required_catalog_route_bindings_shape(
    value: Option<&Value>,
    route_results: &[SourceBackedRefreshRouteResult],
    expected_catalog_lineages: &BTreeSet<String>,
) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
    let values = value
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no catalog_route_bindings")
        })?
        .as_object()
        .ok_or_else(|| {
            anyhow!(
                "published daemon source refresh receipt catalog_route_bindings must be an object"
            )
        })?;
    let bindings = values
        .iter()
        .map(|(catalog_lineage, route_identity)| {
            if !is_sha256_identity(catalog_lineage) {
                bail!("published daemon source refresh catalog lineage is invalid");
            }
            let route_identity = route_identity.as_str().ok_or_else(|| {
                anyhow!("published daemon source refresh catalog binding route is invalid")
            })?;
            if !is_sha256_identity(route_identity) {
                bail!("published daemon source refresh catalog binding route is invalid");
            }
            let route_result = route_results
                .iter()
                .find(|result| result.route_identity == route_identity)
                .ok_or_else(|| {
                    anyhow!(
                        "published daemon source refresh catalog binding route is absent from route_results"
                    )
                })?;
            if !expected_catalog_lineages.contains(catalog_lineage)
                && !matches!(
                    route_result.outcome,
                    SourceBackedRefreshRouteOutcome::Failed { .. }
                )
            {
                bail!(
                    "published daemon source refresh unretained catalog binding has no terminal failure"
                );
            }
            Ok(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: catalog_lineage.clone(),
                route_identity: route_identity.to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let actual = bindings
        .iter()
        .map(|binding| binding.catalog_lineage.clone())
        .collect::<BTreeSet<_>>();
    if actual.len() != bindings.len() || !expected_catalog_lineages.is_subset(&actual) {
        bail!("published daemon source refresh catalog bindings are inconsistent");
    }
    Ok(bindings)
}

fn required_route_source_failures(
    route_identity: &str,
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshSourceFailure>> {
    let value = value.ok_or_else(|| anyhow!("terminal route result has no source diagnostics"))?;
    value
        .as_array()
        .ok_or_else(|| anyhow!("terminal route result source diagnostics must be an array"))?
        .iter()
        .map(|value| {
            let fields = value
                .as_array()
                .filter(|fields| fields.len() == 6)
                .ok_or_else(|| {
                    anyhow!("daemon source refresh compact source diagnostic is malformed")
                })?;
            let required = |index: usize, field: &'static str| {
                fields[index]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh source diagnostic has no {field}")
                    })
            };
            Ok(SourceBackedRefreshSourceFailure {
                route_identity: route_identity.to_owned(),
                source_identity: required(0, "source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required(1, "provider")?,
                class: compact_source_failure_class(fields[2].as_str())?,
                carried_forward: fields[3].as_bool().ok_or_else(|| {
                    anyhow!("daemon source refresh source diagnostic has no carried_forward fact")
                })?,
                source_selector: required(4, "source_selector")?,
                detail: required(5, "detail")?,
            })
        })
        .collect()
}

fn compact_source_failure_class(value: Option<&str>) -> Result<String> {
    Ok(match value {
        Some("u") => "unavailable",
        Some("c") => "source_changed",
        Some("r") => "unreadable",
        Some("i") => "incompatible",
        _ => bail!("published daemon source refresh source failure class is invalid"),
    }
    .to_owned())
}

trait Sha256IdentityString {
    fn into_sha256_identity(self, field: &'static str) -> Result<String>;
}

impl Sha256IdentityString for String {
    fn into_sha256_identity(self, field: &'static str) -> Result<String> {
        if is_sha256_identity(&self) {
            Ok(self)
        } else {
            bail!("daemon source refresh source failure {field} is malformed")
        }
    }
}

#[doc(hidden)]
pub fn is_sha256_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn optional_generation(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("daemon source refresh generation identity is malformed"),
    }
}

#[doc(hidden)]
pub fn required_generation(value: Option<&Value>, label: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} is missing"))
}

fn required_usize(value: &serde_json::Map<String, Value>, field: &str) -> Result<usize> {
    required_usize_from_value(value.get(field), field)
}

fn required_usize_from_value(value: Option<&Value>, field: &str) -> Result<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has invalid {field}"))
}

fn required_u64(value: &serde_json::Map<String, Value>, field: &str) -> Result<u64> {
    required_u64_from_value(value.get(field), field)
}

fn required_u64_from_value(value: Option<&Value>, field: &str) -> Result<u64> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has invalid {field}"))
}
