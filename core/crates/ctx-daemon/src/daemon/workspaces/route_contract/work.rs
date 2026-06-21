use ctx_core::ids::{
    ArtifactId, ChangeSetId, ContributionId, SessionId, WorkEventId, WorkEvidenceId, WorkRecordId,
    WorkSearchDocId, WorkSummaryClaimId, WorkSummaryId,
};
use ctx_core::models::{
    Artifact, ChangeSet, Contribution, ContributionEndpoint, PullRequestLink, RecordFidelity,
    RecordSource, RecordTrust, WorkActorKind, WorkEvent, WorkEventType, WorkEvidence,
    WorkEvidenceFreshness, WorkEvidenceStatus, WorkLinkTargetKind, WorkRecord, WorkRecordLink,
    WorkRedactionClass, WorkSearchDoc, WorkSummary, WorkSummaryClaim, WorkSummaryFreshness,
    WorkSummaryGenerationMethod, WorkTrustVerdict, WORK_OBSERVABILITY_SCHEMA_VERSION,
};
use ctx_core::redaction::is_sensitive_key;
use ctx_route_contracts::workspaces::{
    WorkspaceRouteParams, WorkspaceWorkArtifactRenderKind, WorkspaceWorkArtifactRouteItem,
    WorkspaceWorkArtifactSummaryRouteResponse, WorkspaceWorkChangeSummaryRouteResponse,
    WorkspaceWorkCommandPreviewRouteResponse, WorkspaceWorkContextRouteQuery,
    WorkspaceWorkContextRouteResponse, WorkspaceWorkDetailRouteResponse,
    WorkspaceWorkDuplicateStrongLinkRouteItem, WorkspaceWorkEventRouteItem,
    WorkspaceWorkEvidenceCreateRouteRequest, WorkspaceWorkEvidenceCreateRouteResponse,
    WorkspaceWorkEvidenceRouteItem, WorkspaceWorkEvidenceRouteResponse,
    WorkspaceWorkEvidenceSummaryRouteResponse, WorkspaceWorkInspectorOverviewRouteResponse,
    WorkspaceWorkInspectorRouteResponse, WorkspaceWorkLinkRouteItem, WorkspaceWorkListRouteQuery,
    WorkspaceWorkListRouteResponse, WorkspaceWorkRecordRouteItem, WorkspaceWorkReportRouteResponse,
    WorkspaceWorkSafeJsonRouteResponse, WorkspaceWorkSummaryClaimCreateRouteRequest,
    WorkspaceWorkSummaryClaimRouteItem, WorkspaceWorkSummaryCreateRouteRequest,
    WorkspaceWorkSummaryCreateRouteResponse, WorkspaceWorkSummaryRouteItem,
    WorkspaceWorkTimelineItemRouteResponse, WorkspaceWorkTimelineRouteQuery,
    WorkspaceWorkTimelineRouteResponse, WorkspaceWorkTranscriptItemRouteResponse,
    WorkspaceWorkTrustRouteSummary,
};
use serde_json::{json, Map, Value};
use sha2::Digest;

use super::super::{workspace_store_route_error, WorkspaceRouteError};
use crate::daemon::WorkspaceWorkHandle;

const REPORT_TEXT_LIMIT: usize = 16 * 1024;
const CONTEXT_TEXT_LIMIT: usize = 6 * 1024;
const EVENT_TEXT_LIMIT: usize = 8 * 1024;
const COMMAND_OUTPUT_PREVIEW_LIMIT: usize = 4 * 1024;
const WORK_PROJECTION_SESSION_REFRESH_LIMIT: usize = 128;

#[derive(Debug, Clone)]
pub struct WorkspaceWorkArtifactRouteTarget {
    pub session_id: SessionId,
    pub artifact_id: ArtifactId,
    pub mime_type: String,
    pub name: Option<String>,
}

impl WorkspaceWorkHandle {
    pub async fn list_workspace_work_for_route(
        &self,
        params: WorkspaceRouteParams,
        query: WorkspaceWorkListRouteQuery,
    ) -> Result<WorkspaceWorkListRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work = store
            .list_workspace_work_records(workspace_id, query.limit)
            .await
            .map_err(WorkspaceRouteError::internal)?
            .into_iter()
            .map(|work| route_work_record(&work, None, None))
            .collect();
        Ok(WorkspaceWorkListRouteResponse { work })
    }

    pub async fn get_workspace_work_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
    ) -> Result<WorkspaceWorkDetailRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        let raw = load_work_detail(&store, workspace_id, work_id).await?;
        let route_summaries = raw
            .summaries
            .iter()
            .filter(|summary| is_default_route_summary(summary))
            .collect::<Vec<_>>();
        let material_key = material_revision_key(
            &raw.work,
            &raw.links,
            &raw.events,
            &raw.evidence,
            &raw.change_sets,
            &raw.contributions,
        );
        let summary_freshness = aggregate_summary_freshness_refs(&route_summaries, &material_key);
        let trust = computed_trust_verdict(&raw.work, &raw.evidence);
        Ok(WorkspaceWorkDetailRouteResponse {
            work: route_work_record(&raw.work, Some(trust), Some(summary_freshness)),
            links: raw.links.iter().map(route_work_link).collect(),
            evidence: raw.evidence.iter().map(route_work_evidence).collect(),
            summaries: route_summaries
                .into_iter()
                .map(|summary| route_work_summary(summary, &material_key, REPORT_TEXT_LIMIT))
                .collect(),
            summary_claims: raw
                .summary_claims
                .iter()
                .filter(|claim| {
                    raw.summaries.iter().any(|summary| {
                        is_default_route_summary(summary) && summary.summary_id == claim.summary_id
                    })
                })
                .map(|claim| route_work_summary_claim(claim, &material_key))
                .collect(),
            duplicate_strong_links: raw
                .duplicate_strong_links
                .into_iter()
                .map(|duplicate| WorkspaceWorkDuplicateStrongLinkRouteItem {
                    target_kind: duplicate.target_kind,
                    target_id: duplicate.target_id,
                    work_ids: duplicate.work_ids,
                })
                .collect(),
            raw_detail_included: false,
        })
    }

    pub async fn get_workspace_work_report_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
    ) -> Result<WorkspaceWorkReportRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        refresh_session_linked_work_projection(&store, workspace_id, &work_id).await;
        build_report(&store, workspace_id, work_id).await
    }

    pub async fn get_workspace_work_inspector_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
    ) -> Result<WorkspaceWorkInspectorRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        refresh_session_linked_work_projection(&store, workspace_id, &work_id).await;
        build_inspector(&store, workspace_id, work_id).await
    }

    pub async fn resolve_workspace_work_artifact_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
        artifact_id: String,
    ) -> Result<WorkspaceWorkArtifactRouteTarget, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let artifact_id = uuid::Uuid::parse_str(artifact_id.trim())
            .map(ArtifactId)
            .map_err(|_| WorkspaceRouteError::bad_request("invalid artifact id"))?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        let raw = load_work_detail(&store, workspace_id, work_id).await?;
        let artifact = store
            .get_artifact(artifact_id)
            .await
            .map_err(WorkspaceRouteError::internal)?
            .filter(|artifact| artifact.workspace_id == workspace_id)
            .ok_or_else(|| WorkspaceRouteError::not_found("artifact not found"))?;

        if !work_artifact_is_linked(&raw, &artifact) {
            return Err(WorkspaceRouteError::not_found(
                "artifact not linked to work",
            ));
        }

        Ok(WorkspaceWorkArtifactRouteTarget {
            session_id: artifact.session_id,
            artifact_id: artifact.id,
            mime_type: artifact.mime_type,
            name: artifact.name,
        })
    }

    pub async fn get_workspace_work_context_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
        query: WorkspaceWorkContextRouteQuery,
    ) -> Result<WorkspaceWorkContextRouteResponse, WorkspaceRouteError> {
        let report = self
            .get_workspace_work_report_for_route(params, work_id.clone())
            .await?;
        let budget_tokens = query.budget.unwrap_or(12_000).clamp(1_000, 32_000);
        let text_budget = (budget_tokens.saturating_mul(4)).min(CONTEXT_TEXT_LIMIT);
        let objective = report
            .work
            .objective
            .clone()
            .or_else(|| report.work.title.clone())
            .unwrap_or_else(|| "Untitled Work".to_string());
        let current_result = report.trust.reason.clone();
        let evidence = report
            .evidence
            .iter()
            .take(8)
            .map(|item| {
                json!({
                    "evidence_id": item.evidence_id,
                    "claim": item.claim.as_deref().map(|text| bounded_redacted_text(text, 800)),
                    "freshness": item.freshness,
                    "status": item.status,
                })
            })
            .collect::<Vec<_>>();
        let key_decisions = report
            .summaries
            .iter()
            .take(3)
            .map(|summary| {
                json!({
                    "text": bounded_redacted_text(&summary.text, text_budget / 3),
                    "citations": [{
                        "source_kind": "summary",
                        "source_id": summary.summary_id,
                        "freshness": summary.freshness,
                    }]
                })
            })
            .collect::<Vec<_>>();
        Ok(WorkspaceWorkContextRouteResponse {
            work_id: report.work.work_id,
            budget_tokens,
            title: report.work.title,
            state: serde_json::to_value(report.work.lifecycle)
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "active".to_string()),
            trust_verdict: report.trust.verdict,
            summary_freshness: report.work.summary_freshness,
            context: json!({
                "objective": bounded_redacted_text(&objective, 1_200),
                "current_result": bounded_redacted_text(&current_result, 1_200),
                "key_decisions": key_decisions,
                "evidence": evidence,
                "open_risks": report.trust.open_risks,
                "duplicate_strong_links": report.duplicate_strong_links,
            }),
            raw_transcript_available: false,
            raw_transcript_included: false,
        })
    }

    pub async fn get_workspace_work_timeline_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
        query: WorkspaceWorkTimelineRouteQuery,
    ) -> Result<WorkspaceWorkTimelineRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        load_work_record(&store, workspace_id, work_id.clone()).await?;
        let events = store
            .list_work_events(workspace_id, work_id.clone(), query.limit)
            .await
            .map_err(WorkspaceRouteError::internal)?
            .iter()
            .map(route_work_event)
            .collect();
        Ok(WorkspaceWorkTimelineRouteResponse {
            work_id,
            events,
            raw_transcript_included: false,
        })
    }

    pub async fn get_workspace_work_evidence_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
    ) -> Result<WorkspaceWorkEvidenceRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        load_work_record(&store, workspace_id, work_id.clone()).await?;
        let evidence = store
            .list_work_evidence(workspace_id, work_id.clone())
            .await
            .map_err(WorkspaceRouteError::internal)?
            .iter()
            .map(route_work_evidence)
            .collect();
        Ok(WorkspaceWorkEvidenceRouteResponse { work_id, evidence })
    }

    pub async fn create_workspace_work_evidence_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
        request: WorkspaceWorkEvidenceCreateRouteRequest,
    ) -> Result<WorkspaceWorkEvidenceCreateRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        load_work_record(&store, workspace_id, work_id.clone()).await?;

        let now = chrono::Utc::now();
        let started_at = request.started_at.unwrap_or(now);
        let finished_at = request.finished_at.unwrap_or(started_at);
        let source = route_record_source_or(request.source, RecordSource::Manual);
        let fidelity = route_record_fidelity_or(request.fidelity, RecordFidelity::Declared);
        let trust = route_evidence_trust_or(request.trust);
        let evidence = WorkEvidence {
            evidence_id: WorkEvidenceId::new(),
            work_id: work_id.clone(),
            workspace_id,
            kind: request.kind,
            status: request.status,
            freshness: request.freshness,
            claim: bounded_optional_text(request.claim, 1_200),
            command: bounded_optional_text(request.command, 2_000),
            argv: request
                .argv
                .into_iter()
                .take(128)
                .map(|arg| bounded_redacted_text(&arg, 600))
                .collect(),
            cwd: bounded_optional_text(request.cwd, 1_000),
            exit_code: request.exit_code,
            repo_root: bounded_optional_text(request.repo_root, 1_000),
            head_sha: request.head_sha,
            branch: bounded_optional_text(request.branch, 500),
            fingerprint: None,
            current_fingerprint: None,
            output_ref: request.output_ref.as_ref().map(redact_route_value),
            artifact_ref: request.artifact_ref.as_ref().map(redact_route_value),
            source,
            fidelity,
            trust,
            started_at,
            finished_at,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        };

        let evidence = store
            .upsert_work_evidence(&evidence)
            .await
            .map_err(WorkspaceRouteError::internal)?;
        append_route_work_event(
            &store,
            workspace_id,
            &work_id,
            WorkEventType::EvidenceObserved,
            WorkActorKind::System,
            "evidence",
            &evidence.evidence_id.0,
            evidence.claim.as_deref().unwrap_or("Evidence observed"),
            evidence.source,
            evidence.fidelity,
            evidence.trust,
        )
        .await?;
        index_route_work_evidence(&store, &evidence).await?;
        refresh_route_work_trust(&store, workspace_id, &work_id).await?;

        Ok(WorkspaceWorkEvidenceCreateRouteResponse {
            work_id,
            evidence: route_work_evidence(&evidence),
        })
    }

    pub async fn create_workspace_work_summary_for_route(
        &self,
        params: WorkspaceRouteParams,
        work_id: String,
        request: WorkspaceWorkSummaryCreateRouteRequest,
    ) -> Result<WorkspaceWorkSummaryCreateRouteResponse, WorkspaceRouteError> {
        validate_summary_create_request(&request)?;
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let work_id = WorkRecordId::from_id(work_id);
        let raw = load_work_detail(&store, workspace_id, work_id.clone()).await?;
        let material_key = material_revision_key(
            &raw.work,
            &raw.links,
            &raw.events,
            &raw.evidence,
            &raw.change_sets,
            &raw.contributions,
        );
        let trust = trust_summary(&raw.work, &raw.evidence);
        let text = request
            .text
            .map(|text| bounded_redacted_text(&text, REPORT_TEXT_LIMIT));
        let text = text.unwrap_or_else(|| deterministic_route_summary_text(&raw.work, &trust));
        let now = chrono::Utc::now();
        let summary = WorkSummary {
            summary_id: WorkSummaryId::new(),
            work_id: work_id.clone(),
            workspace_id,
            kind: request.kind,
            audience: request.audience,
            text,
            structured_json: request.structured_json.as_ref().map(redact_route_value),
            generation_method: request.generation_method,
            provider: None,
            model: None,
            template: request
                .template
                .map(|value| bounded_redacted_text(&value, 200)),
            source_material_left_machine: false,
            freshness: route_summary_freshness_or(request.freshness, WorkSummaryFreshness::Fresh),
            source_revision_key: Some(request.source_revision_key.unwrap_or(material_key.clone())),
            generated_at: now,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        };
        let summary = store
            .upsert_work_summary(&summary)
            .await
            .map_err(WorkspaceRouteError::internal)?;

        let mut claim_requests = request.claims;
        if claim_requests.is_empty() {
            claim_requests.push(default_summary_claim_request(
                &summary,
                &work_id,
                &material_key,
            ));
        }
        let mut claims = Vec::with_capacity(claim_requests.len().min(64));
        for claim_request in claim_requests.into_iter().take(64) {
            let claim = route_summary_claim_from_request(
                claim_request,
                &summary,
                workspace_id,
                &work_id,
                &material_key,
            )?;
            let claim = store
                .upsert_work_summary_claim(&claim)
                .await
                .map_err(WorkspaceRouteError::internal)?;
            claims.push(claim);
        }

        append_route_work_event(
            &store,
            workspace_id,
            &work_id,
            WorkEventType::SummaryGenerated,
            WorkActorKind::System,
            "summary",
            &summary.summary_id.0,
            "Work summary generated",
            RecordSource::Manual,
            RecordFidelity::Summary,
            RecordTrust::Medium,
        )
        .await?;
        index_route_work_summary(&store, &summary).await?;
        refresh_route_summary_freshness(&store, workspace_id, &work_id, summary.freshness).await?;

        Ok(WorkspaceWorkSummaryCreateRouteResponse {
            work_id,
            summary: route_work_summary(&summary, &material_key, REPORT_TEXT_LIMIT),
            claims: claims
                .iter()
                .map(|claim| route_work_summary_claim(claim, &material_key))
                .collect(),
        })
    }
}

struct RawWorkDetail {
    work: WorkRecord,
    links: Vec<WorkRecordLink>,
    evidence: Vec<WorkEvidence>,
    summaries: Vec<WorkSummary>,
    summary_claims: Vec<WorkSummaryClaim>,
    events: Vec<WorkEvent>,
    change_sets: Vec<ChangeSet>,
    contributions: Vec<Contribution>,
    duplicate_strong_links: Vec<ctx_store::WorkStrongLinkDuplicate>,
}

fn bounded_optional_text(value: Option<String>, limit: usize) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| bounded_redacted_text(text, limit))
}

fn route_record_source_or(value: RecordSource, fallback: RecordSource) -> RecordSource {
    if value == RecordSource::Unknown {
        fallback
    } else {
        value
    }
}

fn route_record_fidelity_or(value: RecordFidelity, fallback: RecordFidelity) -> RecordFidelity {
    if value == RecordFidelity::Unknown {
        fallback
    } else {
        value
    }
}

fn route_record_trust_or(value: RecordTrust, fallback: RecordTrust) -> RecordTrust {
    if value == RecordTrust::Unknown {
        fallback
    } else {
        value
    }
}

fn route_evidence_trust_or(value: RecordTrust) -> RecordTrust {
    match route_record_trust_or(value, RecordTrust::Medium) {
        RecordTrust::Verified => RecordTrust::Medium,
        other => other,
    }
}

fn route_summary_freshness_or(
    value: WorkSummaryFreshness,
    fallback: WorkSummaryFreshness,
) -> WorkSummaryFreshness {
    if value == WorkSummaryFreshness::Missing {
        fallback
    } else {
        value
    }
}

fn is_default_route_summary(summary: &WorkSummary) -> bool {
    summary.generation_method != WorkSummaryGenerationMethod::ProviderLlm
        && !summary.source_material_left_machine
}

fn validate_summary_create_request(
    request: &WorkspaceWorkSummaryCreateRouteRequest,
) -> Result<(), WorkspaceRouteError> {
    if request.generation_method == WorkSummaryGenerationMethod::ProviderLlm
        || request.source_material_left_machine
        || request.provider.is_some()
        || request.model.is_some()
    {
        return Err(WorkspaceRouteError::bad_request(
            "provider-backed summaries are out of scope for local Work routes",
        ));
    }
    if let Some(text) = request.text.as_deref() {
        if text.trim().is_empty() {
            return Err(WorkspaceRouteError::bad_request(
                "summary text cannot be empty",
            ));
        }
    }
    Ok(())
}

fn deterministic_route_summary_text(
    work: &WorkRecord,
    trust: &WorkspaceWorkTrustRouteSummary,
) -> String {
    let title = work.title.as_deref().unwrap_or("Untitled Work");
    format!(
        "{title}\n\nTrust verdict: {:?}. Next action: {}",
        trust.verdict, trust.recommended_next_action
    )
}

fn default_summary_claim_request(
    summary: &WorkSummary,
    work_id: &WorkRecordId,
    material_key: &str,
) -> WorkspaceWorkSummaryClaimCreateRouteRequest {
    WorkspaceWorkSummaryClaimCreateRouteRequest {
        claim_text: summary
            .text
            .lines()
            .next()
            .unwrap_or("Work summary generated")
            .to_string(),
        claim_kind: Some("summary".to_string()),
        source_kind: Some("work_report".to_string()),
        source_id: Some(work_id.0.clone()),
        record_hash: Some(material_key.to_string()),
        freshness: WorkSummaryFreshness::Fresh,
        redaction_class: WorkRedactionClass::LocalRedacted,
    }
}

fn route_summary_claim_from_request(
    request: WorkspaceWorkSummaryClaimCreateRouteRequest,
    summary: &WorkSummary,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
    material_key: &str,
) -> Result<WorkSummaryClaim, WorkspaceRouteError> {
    let claim_text = request.claim_text.trim();
    if claim_text.is_empty() {
        return Err(WorkspaceRouteError::bad_request(
            "summary claim text is required",
        ));
    }
    Ok(WorkSummaryClaim {
        claim_id: WorkSummaryClaimId::new(),
        summary_id: summary.summary_id.clone(),
        work_id: work_id.clone(),
        workspace_id,
        claim_text: bounded_redacted_text(claim_text, 2_000),
        claim_kind: bounded_optional_text(request.claim_kind, 200),
        source_kind: request
            .source_kind
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| bounded_redacted_text(value, 200))
            .unwrap_or_else(|| "work_report".to_string()),
        source_id: request
            .source_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| bounded_redacted_text(value, 500))
            .unwrap_or_else(|| work_id.0.clone()),
        record_hash: request
            .record_hash
            .or_else(|| Some(material_key.to_string())),
        freshness: route_summary_freshness_or(request.freshness, WorkSummaryFreshness::Fresh),
        redaction_class: request.redaction_class,
        created_at: chrono::Utc::now(),
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    })
}

async fn append_route_work_event(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
    event_type: WorkEventType,
    actor_kind: WorkActorKind,
    source_kind: &str,
    source_id: &str,
    redacted_text: &str,
    source: RecordSource,
    fidelity: RecordFidelity,
    trust: RecordTrust,
) -> Result<(), WorkspaceRouteError> {
    let now = chrono::Utc::now();
    let event = WorkEvent {
        event_id: WorkEventId::new(),
        work_id: work_id.clone(),
        workspace_id,
        sequence: 0,
        source_kind: Some(source_kind.to_string()),
        source_id: Some(source_id.to_string()),
        event_type,
        event_time: now,
        actor_kind,
        provider: None,
        harness: None,
        model: None,
        redaction_class: WorkRedactionClass::LocalRedacted,
        source,
        fidelity,
        trust,
        payload_json: None,
        redacted_text: Some(bounded_redacted_text(redacted_text, EVENT_TEXT_LIMIT)),
        artifact_ref: None,
        created_at: now,
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    };
    store
        .append_work_event(&event)
        .await
        .map_err(WorkspaceRouteError::internal)?;
    Ok(())
}

async fn index_route_work_evidence(
    store: &ctx_store::Store,
    evidence: &WorkEvidence,
) -> Result<(), WorkspaceRouteError> {
    let now = chrono::Utc::now();
    let doc = WorkSearchDoc {
        doc_id: stable_route_search_doc_id(
            evidence.workspace_id,
            "work_evidence",
            &evidence.evidence_id.0,
        ),
        workspace_id: evidence.workspace_id,
        work_id: evidence.work_id.clone(),
        doc_type: "evidence".to_string(),
        source_id: evidence.evidence_id.0.clone(),
        source_kind: "evidence".to_string(),
        event_time: evidence.finished_at,
        repo_root: evidence
            .repo_root
            .as_deref()
            .map(|root| bounded_redacted_text(root, 1_000)),
        path: None,
        branch: evidence
            .branch
            .as_deref()
            .map(|branch| bounded_redacted_text(branch, 500)),
        commit_sha: evidence.head_sha.clone(),
        pr_owner: None,
        pr_repo: None,
        pr_number: None,
        agent_provider: None,
        freshness: evidence.freshness,
        redaction_class: WorkRedactionClass::LocalRedacted,
        title: evidence
            .claim
            .as_deref()
            .map(|claim| bounded_redacted_text(claim, 1_000)),
        search_text_redacted: bounded_redacted_text(
            &[
                evidence.claim.as_deref().unwrap_or(""),
                evidence.command.as_deref().unwrap_or(""),
                &evidence.argv.join(" "),
            ]
            .join("\n"),
            16 * 1024,
        ),
        created_at: now,
        updated_at: now,
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    };
    store
        .upsert_work_search_doc(&doc)
        .await
        .map_err(WorkspaceRouteError::internal)?;
    Ok(())
}

async fn index_route_work_summary(
    store: &ctx_store::Store,
    summary: &WorkSummary,
) -> Result<(), WorkspaceRouteError> {
    let now = chrono::Utc::now();
    let freshness = match summary.freshness {
        WorkSummaryFreshness::Fresh | WorkSummaryFreshness::Locked => WorkEvidenceFreshness::Fresh,
        WorkSummaryFreshness::Stale => WorkEvidenceFreshness::Stale,
        WorkSummaryFreshness::Partial => WorkEvidenceFreshness::Partial,
        WorkSummaryFreshness::Missing => WorkEvidenceFreshness::Unknown,
    };
    let doc = WorkSearchDoc {
        doc_id: stable_route_search_doc_id(
            summary.workspace_id,
            "work_summary",
            &summary.summary_id.0,
        ),
        workspace_id: summary.workspace_id,
        work_id: summary.work_id.clone(),
        doc_type: "summary".to_string(),
        source_id: summary.summary_id.0.clone(),
        source_kind: "summary".to_string(),
        event_time: summary.generated_at,
        repo_root: None,
        path: None,
        branch: None,
        commit_sha: None,
        pr_owner: None,
        pr_repo: None,
        pr_number: None,
        agent_provider: summary.provider.clone(),
        freshness,
        redaction_class: WorkRedactionClass::LocalRedacted,
        title: Some(format!("{:?}", summary.kind)),
        search_text_redacted: bounded_redacted_text(&summary.text, 16 * 1024),
        created_at: now,
        updated_at: now,
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    };
    store
        .upsert_work_search_doc(&doc)
        .await
        .map_err(WorkspaceRouteError::internal)?;
    Ok(())
}

fn stable_route_search_doc_id(
    workspace_id: ctx_core::ids::WorkspaceId,
    kind: &str,
    source_id: &str,
) -> WorkSearchDocId {
    let digest = sha2::Sha256::digest(format!("{}:{kind}:{source_id}", workspace_id.0).as_bytes());
    WorkSearchDocId::from_id(format!("wsd_{}", hex::encode(digest)))
}

fn redact_route_serializable<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value)
        .map(|value| redact_route_value(&value))
        .unwrap_or_else(|_| Value::String("[redacted:unserializable]".to_string()))
}

fn inspector_change_set_value(change_set: &ChangeSet) -> Value {
    json!({
        "id": change_set.id,
        "source": change_set.source,
        "fidelity": change_set.fidelity,
        "trust": change_set.trust,
        "title": change_set.title.as_deref().map(|text| bounded_redacted_text(text, 1_000)),
        "summary": change_set.summary.as_deref().map(|text| bounded_redacted_text(text, 2_000)),
        "base_revision": change_set.base_revision.as_deref().map(|text| bounded_redacted_text(text, 200)),
        "head_revision": change_set.head_revision.as_deref().map(|text| bounded_redacted_text(text, 200)),
        "target_branch": change_set.target_branch.as_deref().map(|text| bounded_redacted_text(text, 500)),
        "fingerprint": change_set.fingerprint.as_ref().map(|fingerprint| json!({
            "head_sha": fingerprint.head_sha,
            "branch": fingerprint.branch.as_deref().map(|text| bounded_redacted_text(text, 500)),
            "patch_sha256": fingerprint.patch_sha256,
            "status_sha256": fingerprint.status_sha256,
            "untracked_sha256": fingerprint.untracked_sha256,
            "changed_paths_sha256": fingerprint.changed_paths_sha256,
            "dirty": fingerprint.dirty,
        })),
        "pull_requests": change_set
            .pull_requests
            .iter()
            .map(inspector_pull_request_link_value)
            .collect::<Vec<_>>(),
        "created_at": change_set.created_at,
        "updated_at": change_set.updated_at,
        "schema_version": change_set.schema_version,
    })
}

fn inspector_contribution_value(contribution: &Contribution) -> Value {
    json!({
        "id": contribution.id,
        "change_set_id": contribution.change_set_id,
        "subject": inspector_contribution_endpoint_value(&contribution.subject),
        "target": inspector_contribution_endpoint_value(&contribution.target),
        "role": contribution.role,
        "source": contribution.source,
        "fidelity": contribution.fidelity,
        "trust": contribution.trust,
        "summary": contribution.summary.as_deref().map(|text| bounded_redacted_text(text, 2_000)),
        "fingerprint": contribution.fingerprint.as_ref().map(|fingerprint| json!({
            "head_sha": fingerprint.head_sha,
            "branch": fingerprint.branch.as_deref().map(|text| bounded_redacted_text(text, 500)),
            "patch_sha256": fingerprint.patch_sha256,
            "status_sha256": fingerprint.status_sha256,
            "untracked_sha256": fingerprint.untracked_sha256,
            "changed_paths_sha256": fingerprint.changed_paths_sha256,
            "dirty": fingerprint.dirty,
        })),
        "created_at": contribution.created_at,
        "updated_at": contribution.updated_at,
        "schema_version": contribution.schema_version,
    })
}

fn inspector_contribution_endpoint_value(endpoint: &ContributionEndpoint) -> Value {
    match endpoint {
        ContributionEndpoint::Account { account_id } => json!({
            "kind": "account",
            "account_id": account_id,
        }),
        ContributionEndpoint::Workspace { workspace_id } => json!({
            "kind": "workspace",
            "workspace_id": workspace_id,
        }),
        ContributionEndpoint::Task { task_id, id } => json!({
            "kind": "task",
            "task_id": task_id,
            "id": id.as_deref().map(|text| bounded_redacted_text(text, 400)),
        }),
        ContributionEndpoint::Session {
            session_id,
            provider,
            id,
            turn_id,
            run_id,
        } => json!({
            "kind": "session",
            "session_id": session_id,
            "provider": provider.as_deref().map(|text| bounded_redacted_text(text, 200)),
            "id": id.as_deref().map(|text| bounded_redacted_text(text, 400)),
            "turn_id": turn_id,
            "run_id": run_id,
        }),
        ContributionEndpoint::Run {
            run_id,
            id,
            session_id,
        } => json!({
            "kind": "run",
            "run_id": run_id,
            "id": id.as_deref().map(|text| bounded_redacted_text(text, 400)),
            "session_id": session_id,
        }),
        ContributionEndpoint::Agent {
            session_id,
            run_id,
            label,
        } => json!({
            "kind": "agent",
            "session_id": session_id,
            "run_id": run_id,
            "label": label.as_deref().map(|text| bounded_redacted_text(text, 400)),
        }),
        ContributionEndpoint::System { label } => json!({
            "kind": "system",
            "label": label.as_deref().map(|text| bounded_redacted_text(text, 400)),
        }),
        ContributionEndpoint::Worktree { worktree_id, id } => json!({
            "kind": "worktree",
            "worktree_id": worktree_id,
            "id": id.as_deref().map(|text| bounded_redacted_text(text, 400)),
        }),
        ContributionEndpoint::ChangeSet { change_set_id } => json!({
            "kind": "change_set",
            "change_set_id": change_set_id,
        }),
        ContributionEndpoint::PullRequest { pull_request } => json!({
            "kind": "pull_request",
            "pull_request": inspector_pull_request_ref_value(
                &pull_request.provider,
                &pull_request.owner,
                &pull_request.repo,
                pull_request.number,
                pull_request.id.as_deref(),
                pull_request.url.as_deref(),
                pull_request.title.as_deref(),
                None,
            ),
        }),
        ContributionEndpoint::Artifact {
            artifact_id,
            digest,
            relative_path,
        } => json!({
            "kind": "artifact",
            "artifact_id": artifact_id,
            "digest": digest.as_deref().map(|text| bounded_redacted_text(text, 200)),
            "relative_path": relative_path.as_deref().and_then(safe_relative_display_path),
        }),
        ContributionEndpoint::Check { check_id } => json!({
            "kind": "check",
            "check_id": bounded_redacted_text(check_id, 400),
        }),
        ContributionEndpoint::Evidence { id } => json!({
            "kind": "evidence",
            "id": bounded_redacted_text(id, 400),
        }),
        ContributionEndpoint::ReviewAttestation { id } => json!({
            "kind": "review_attestation",
            "id": bounded_redacted_text(id, 400),
        }),
        ContributionEndpoint::Commit { sha } => json!({
            "kind": "commit",
            "sha": bounded_redacted_text(sha, 200),
        }),
        ContributionEndpoint::Branch { name } => json!({
            "kind": "branch",
            "name": bounded_redacted_text(name, 500),
        }),
        ContributionEndpoint::File { path, worktree_id } => json!({
            "kind": "file",
            "path": safe_relative_display_path(path),
            "worktree_id": worktree_id,
        }),
        ContributionEndpoint::External {
            source,
            identifier,
            url,
        } => json!({
            "kind": "external",
            "source": bounded_redacted_text(source, 200),
            "identifier": identifier.as_deref().map(|text| bounded_redacted_text(text, 400)),
            "url": url.as_deref().and_then(safe_http_url_text),
        }),
    }
}

fn inspector_pull_request_link_value(link: &PullRequestLink) -> Value {
    inspector_pull_request_ref_value(
        &link.pull_request.provider,
        &link.pull_request.owner,
        &link.pull_request.repo,
        link.pull_request.number,
        link.pull_request.id.as_deref(),
        link.url.as_deref().or(link.pull_request.url.as_deref()),
        link.title.as_deref().or(link.pull_request.title.as_deref()),
        link.state.as_deref(),
    )
}

fn inspector_pull_request_ref_value(
    provider: &str,
    owner: &str,
    repo: &str,
    number: i64,
    id: Option<&str>,
    url: Option<&str>,
    title: Option<&str>,
    state: Option<&str>,
) -> Value {
    json!({
        "provider": bounded_redacted_text(provider, 100),
        "owner": bounded_redacted_text(owner, 200),
        "repo": bounded_redacted_text(repo, 200),
        "number": number,
        "id": id.map(|text| bounded_redacted_text(text, 300)),
        "url": url.and_then(safe_http_url_text),
        "title": title.map(|text| bounded_redacted_text(text, 1_000)),
        "state": state.map(|text| bounded_redacted_text(text, 100)),
    })
}

async fn refresh_route_work_trust(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
) -> Result<(), WorkspaceRouteError> {
    let evidence = store
        .list_work_evidence(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?;
    if let Some(mut work) = store
        .get_workspace_work_record(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?
    {
        work.trust_verdict = computed_trust_verdict(&work, &evidence);
        work.updated_at = chrono::Utc::now();
        store
            .upsert_work_record(&work)
            .await
            .map_err(WorkspaceRouteError::internal)?;
    }
    Ok(())
}

async fn refresh_route_summary_freshness(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
    fallback: WorkSummaryFreshness,
) -> Result<(), WorkspaceRouteError> {
    let raw = load_work_detail(store, workspace_id, work_id.clone()).await?;
    let material_key = material_revision_key(
        &raw.work,
        &raw.links,
        &raw.events,
        &raw.evidence,
        &raw.change_sets,
        &raw.contributions,
    );
    let summary_freshness = if raw.summaries.is_empty() {
        fallback
    } else {
        aggregate_summary_freshness(&raw.summaries, &material_key)
    };
    if let Some(mut work) = store
        .get_workspace_work_record(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?
    {
        work.summary_freshness = summary_freshness;
        work.updated_at = chrono::Utc::now();
        store
            .upsert_work_record(&work)
            .await
            .map_err(WorkspaceRouteError::internal)?;
    }
    Ok(())
}

async fn build_report(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: WorkRecordId,
) -> Result<WorkspaceWorkReportRouteResponse, WorkspaceRouteError> {
    let raw = load_work_detail(store, workspace_id, work_id.clone()).await?;
    let route_summaries = raw
        .summaries
        .iter()
        .filter(|summary| is_default_route_summary(summary))
        .collect::<Vec<_>>();
    let material_key = material_revision_key(
        &raw.work,
        &raw.links,
        &raw.events,
        &raw.evidence,
        &raw.change_sets,
        &raw.contributions,
    );
    let summary_freshness = aggregate_summary_freshness_refs(&route_summaries, &material_key);
    let trust = trust_summary(&raw.work, &raw.evidence);
    Ok(WorkspaceWorkReportRouteResponse {
        change_summary: WorkspaceWorkChangeSummaryRouteResponse {
            change_sets: raw.change_sets.len(),
            contributions: raw.contributions.len(),
            pull_requests: pull_request_links(&raw.links),
            commits: commit_links(&raw.links),
        },
        work: route_work_record(&raw.work, Some(trust.verdict), Some(summary_freshness)),
        links: raw.links.iter().map(route_work_link).collect(),
        trust,
        evidence_summary: evidence_summary(&raw.evidence),
        evidence: raw.evidence.iter().map(route_work_evidence).collect(),
        change_sets: raw
            .change_sets
            .iter()
            .map(redact_route_serializable)
            .collect(),
        contributions: raw
            .contributions
            .iter()
            .map(redact_route_serializable)
            .collect(),
        summaries: route_summaries
            .into_iter()
            .map(|summary| route_work_summary(summary, &material_key, REPORT_TEXT_LIMIT))
            .collect(),
        summary_claims: raw
            .summary_claims
            .iter()
            .filter(|claim| {
                raw.summaries.iter().any(|summary| {
                    is_default_route_summary(summary) && summary.summary_id == claim.summary_id
                })
            })
            .map(|claim| route_work_summary_claim(claim, &material_key))
            .collect(),
        timeline: raw.events.iter().map(route_work_event).collect(),
        duplicate_strong_links: raw
            .duplicate_strong_links
            .into_iter()
            .map(|duplicate| WorkspaceWorkDuplicateStrongLinkRouteItem {
                target_kind: duplicate.target_kind,
                target_id: duplicate.target_id,
                work_ids: duplicate.work_ids,
            })
            .collect(),
        raw_transcript_available: raw.events.iter().any(|event| event.payload_json.is_some()),
        raw_transcript_included: false,
    })
}

async fn build_inspector(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: WorkRecordId,
) -> Result<WorkspaceWorkInspectorRouteResponse, WorkspaceRouteError> {
    let raw = load_work_detail(store, workspace_id, work_id.clone()).await?;
    let route_summaries = raw
        .summaries
        .iter()
        .filter(|summary| is_default_route_summary(summary))
        .collect::<Vec<_>>();
    let material_key = material_revision_key(
        &raw.work,
        &raw.links,
        &raw.events,
        &raw.evidence,
        &raw.change_sets,
        &raw.contributions,
    );
    let summary_freshness = aggregate_summary_freshness_refs(&route_summaries, &material_key);
    let trust = trust_summary(&raw.work, &raw.evidence);
    let work = route_work_record(&raw.work, Some(trust.verdict), Some(summary_freshness));
    let links = raw
        .links
        .iter()
        .map(route_work_link_metadata_only)
        .collect::<Vec<_>>();
    let evidence = raw
        .evidence
        .iter()
        .map(route_work_inspector_evidence)
        .collect::<Vec<_>>();
    let change_summary = WorkspaceWorkChangeSummaryRouteResponse {
        change_sets: raw.change_sets.len(),
        contributions: raw.contributions.len(),
        pull_requests: pull_request_links(&raw.links),
        commits: commit_links(&raw.links),
    };
    let change_sets = raw
        .change_sets
        .iter()
        .map(inspector_change_set_value)
        .collect::<Vec<_>>();
    let contributions = raw
        .contributions
        .iter()
        .map(inspector_contribution_value)
        .collect::<Vec<_>>();
    let summaries = route_summaries
        .into_iter()
        .map(|summary| route_work_summary(summary, &material_key, REPORT_TEXT_LIMIT))
        .collect::<Vec<_>>();
    let summary_claims = raw
        .summary_claims
        .iter()
        .filter(|claim| {
            raw.summaries.iter().any(|summary| {
                is_default_route_summary(summary) && summary.summary_id == claim.summary_id
            })
        })
        .map(|claim| route_work_summary_claim(claim, &material_key))
        .collect::<Vec<_>>();
    let timeline = raw.events.iter().map(route_work_event).collect::<Vec<_>>();
    let transcript = inspector_transcript_items(&raw.events);
    let commands = inspector_command_previews(&raw.evidence);
    let artifacts = inspector_artifacts(
        store,
        workspace_id,
        work_id.clone(),
        &raw.links,
        &raw.events,
        &raw.evidence,
    )
    .await?;
    let artifact_summary = WorkspaceWorkArtifactSummaryRouteResponse {
        total: artifacts.len(),
        refs: artifacts
            .iter()
            .take(50)
            .map(|artifact| {
                safe_json_response(
                    json!({
                        "id": artifact.id,
                        "artifact_id": artifact.artifact_id,
                        "kind": artifact.kind,
                        "label": artifact.label,
                        "display_name": artifact.display_name,
                        "mime_type": artifact.mime_type,
                        "bytes": artifact.bytes,
                        "missing": artifact.missing,
                        "unavailable_reason": artifact.unavailable_reason,
                        "render_kind": artifact.render_kind,
                        "download_url": artifact.download_url,
                        "open_url": artifact.open_url,
                        "thumbnail_url": artifact.thumbnail_url,
                        "preview_url": artifact.preview_url,
                    }),
                    vec![
                        "artifact metadata only; local paths and raw refs are omitted".to_string(),
                    ],
                )
            })
            .collect(),
    };
    let timeline_items = inspector_timeline_items(&raw.events, &raw.evidence);
    let duplicate_strong_links = raw
        .duplicate_strong_links
        .into_iter()
        .map(|duplicate| WorkspaceWorkDuplicateStrongLinkRouteItem {
            target_kind: duplicate.target_kind,
            target_id: duplicate.target_id,
            work_ids: duplicate.work_ids,
        })
        .collect::<Vec<_>>();
    let overview = WorkspaceWorkInspectorOverviewRouteResponse {
        title: work.title.clone(),
        objective: work.objective.clone(),
        lifecycle: work.lifecycle,
        primary_branch: work.primary_branch.clone(),
        base_commit: work.base_commit.clone(),
        head_commit: work.head_commit.clone(),
        created_at: work.created_at,
        updated_at: work.updated_at,
    };
    let evidence_summary = evidence_summary(&raw.evidence);
    let raw_transcript_available = raw.events.iter().any(|event| event.payload_json.is_some());
    let context = safe_json_response(
        inspector_context_value(
            &work,
            &trust,
            &summaries,
            &summary_claims,
            &evidence,
            &commands,
            &artifacts,
            &change_summary,
            &duplicate_strong_links,
        ),
        vec!["agent handoff context is built from typed, redacted inspector fields".to_string()],
    );
    let safe_json_value = json!({
        "work": &work,
        "links": &links,
        "overview": &overview,
        "trust": &trust,
        "context": &context.value,
        "evidence_summary": &evidence_summary,
        "change_summary": &change_summary,
        "artifact_summary": &artifact_summary,
        "transcript": &transcript,
        "commands": &commands,
        "artifacts": &artifacts,
        "evidence": &evidence,
        "change_sets": &change_sets,
        "contributions": &contributions,
        "summaries": &summaries,
        "summary_claims": &summary_claims,
        "timeline": &timeline,
        "timeline_items": &timeline_items,
        "duplicate_strong_links": &duplicate_strong_links,
        "raw_transcript_available": raw_transcript_available,
        "raw_transcript_included": false,
    });
    let safe_json = safe_json_response(
        safe_json_value,
        vec![
            "whitelist projection only".to_string(),
            "raw payload_json, raw transcript bodies, local artifact paths, and raw command output are excluded"
                .to_string(),
        ],
    );

    Ok(WorkspaceWorkInspectorRouteResponse {
        work,
        links,
        overview,
        trust,
        context,
        safe_json: safe_json.clone(),
        raw_redacted_json: safe_json,
        evidence_summary,
        change_summary,
        artifact_summary,
        transcript,
        commands,
        artifacts,
        evidence,
        change_sets,
        contributions,
        summaries,
        summary_claims,
        timeline,
        timeline_items,
        duplicate_strong_links,
        raw_transcript_available,
        raw_transcript_included: false,
    })
}

async fn load_work_detail(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: WorkRecordId,
) -> Result<RawWorkDetail, WorkspaceRouteError> {
    let work = load_work_record(store, workspace_id, work_id.clone()).await?;
    let links = store
        .list_work_record_links(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let evidence = store
        .list_work_evidence(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let summaries = store
        .list_work_summaries(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let summary_claims = store
        .list_work_summary_claims(workspace_id, None, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let events = store
        .list_work_events(workspace_id, work_id.clone(), Some(500))
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let duplicate_strong_links = store
        .list_strong_work_link_duplicates_for_work(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let (change_sets, contributions) = linked_graph_for_work(store, workspace_id, &links).await?;
    Ok(RawWorkDetail {
        work,
        links,
        evidence,
        summaries,
        summary_claims,
        events,
        change_sets,
        contributions,
        duplicate_strong_links,
    })
}

async fn refresh_session_linked_work_projection(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
) {
    let mut pending = match session_link_ids_for_work(store, workspace_id, work_id).await {
        Ok(session_ids) => session_ids,
        Err(error) => {
            warn_projection_link_list_failed(workspace_id, work_id, &error);
            return;
        }
    };
    let mut visited = Vec::new();

    while let Some(session_id) = pending.pop() {
        if visited.contains(&session_id) {
            continue;
        }
        if visited.len() >= WORK_PROJECTION_SESSION_REFRESH_LIMIT {
            tracing::warn!(
                work_id = %work_id.0,
                workspace_id = %workspace_id.0,
                limit = WORK_PROJECTION_SESSION_REFRESH_LIMIT,
                "stopped ADE session projection refresh after reaching session limit"
            );
            break;
        }
        visited.push(session_id);

        if let Err(error) = store.project_session_to_work(session_id).await {
            tracing::warn!(
                work_id = %work_id.0,
                workspace_id = %workspace_id.0,
                session_id = %session_id.0,
                "failed to refresh ADE session projection before Work route: {error:#}"
            );
        }

        let linked = match session_link_ids_for_work(store, workspace_id, work_id).await {
            Ok(session_ids) => session_ids,
            Err(error) => {
                warn_projection_link_list_failed(workspace_id, work_id, &error);
                break;
            }
        };
        for linked_session_id in linked {
            if !visited.contains(&linked_session_id) && !pending.contains(&linked_session_id) {
                pending.push(linked_session_id);
            }
        }
        pending.sort_by_key(|id| id.0);
    }
}

async fn session_link_ids_for_work(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
) -> Result<Vec<SessionId>, anyhow::Error> {
    let mut session_ids = store
        .list_work_record_links(workspace_id, work_id.clone())
        .await?
        .into_iter()
        .filter(|link| link.target_kind == WorkLinkTargetKind::Session)
        .filter_map(|link| link.target_id)
        .filter_map(|id| uuid::Uuid::parse_str(id.trim()).ok().map(SessionId))
        .collect::<Vec<_>>();
    session_ids.sort_by_key(|id| id.0);
    session_ids.dedup();
    Ok(session_ids)
}

fn warn_projection_link_list_failed(
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
    error: &anyhow::Error,
) {
    tracing::warn!(
        work_id = %work_id.0,
        workspace_id = %workspace_id.0,
        "failed to list Work links before ADE session projection refresh: {error:#}"
    );
}

async fn load_work_record(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: WorkRecordId,
) -> Result<WorkRecord, WorkspaceRouteError> {
    store
        .get_workspace_work_record(workspace_id, work_id.clone())
        .await
        .map_err(WorkspaceRouteError::internal)?
        .ok_or_else(|| {
            WorkspaceRouteError::not_found(format!("work record {} not found", work_id.0))
        })
}

async fn linked_graph_for_work(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    links: &[WorkRecordLink],
) -> Result<(Vec<ChangeSet>, Vec<Contribution>), WorkspaceRouteError> {
    let mut change_sets = Vec::new();
    let mut contributions = Vec::new();
    for link in links {
        match (
            link.target_kind,
            link.target_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty()),
        ) {
            (ctx_core::models::WorkLinkTargetKind::ChangeSet, Some(id)) => {
                if let Some(change_set) = store
                    .get_workspace_change_set(workspace_id, ChangeSetId::from_id(id))
                    .await
                    .map_err(WorkspaceRouteError::internal)?
                {
                    change_sets.push(change_set);
                }
            }
            (ctx_core::models::WorkLinkTargetKind::Contribution, Some(id)) => {
                if let Some(contribution) = store
                    .get_contribution(ContributionId::from_id(id))
                    .await
                    .map_err(WorkspaceRouteError::internal)?
                {
                    if contribution.workspace_id == workspace_id {
                        contributions.push(contribution);
                    }
                }
            }
            _ => {}
        }
    }
    change_sets.sort_by(|left, right| left.id.0.cmp(&right.id.0));
    change_sets.dedup_by(|left, right| left.id == right.id);
    contributions.sort_by(|left, right| left.id.0.cmp(&right.id.0));
    contributions.dedup_by(|left, right| left.id == right.id);
    Ok((change_sets, contributions))
}

fn inspector_transcript_items(
    events: &[WorkEvent],
) -> Vec<WorkspaceWorkTranscriptItemRouteResponse> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type,
                WorkEventType::Session
                    | WorkEventType::UserMessage
                    | WorkEventType::AssistantMessage
                    | WorkEventType::ToolCallStart
                    | WorkEventType::ToolCallEnd
                    | WorkEventType::ToolOutput
                    | WorkEventType::CommandCapture
                    | WorkEventType::ArtifactCreated
                    | WorkEventType::Note
                    | WorkEventType::Other
            )
        })
        .map(|event| WorkspaceWorkTranscriptItemRouteResponse {
            event_id: event.event_id.clone(),
            sequence: event.sequence,
            event_type: event.event_type,
            event_time: event.event_time,
            actor_kind: event.actor_kind,
            provider: event
                .provider
                .as_deref()
                .map(|text| bounded_redacted_text(text, 200)),
            harness: event
                .harness
                .as_deref()
                .map(|text| bounded_redacted_text(text, 200)),
            model: event
                .model
                .as_deref()
                .map(|text| bounded_redacted_text(text, 200)),
            redaction_class: event.redaction_class,
            text_preview: event
                .redacted_text
                .as_deref()
                .map(|text| bounded_redacted_text(text, EVENT_TEXT_LIMIT)),
        })
        .collect()
}

fn inspector_command_previews(
    evidence: &[WorkEvidence],
) -> Vec<WorkspaceWorkCommandPreviewRouteResponse> {
    evidence
        .iter()
        .filter(|item| item.command.is_some() || !item.argv.is_empty())
        .map(|item| {
            let output_ref = item.output_ref.as_ref();
            let stdout_preview =
                output_ref.and_then(|value| safe_output_preview(value, "stdout_redacted"));
            let stderr_preview =
                output_ref.and_then(|value| safe_output_preview(value, "stderr_redacted"));
            let output_truncated = output_ref_bool(output_ref, "truncated");
            WorkspaceWorkCommandPreviewRouteResponse {
                evidence_id: item.evidence_id.clone(),
                id: item.evidence_id.0.clone(),
                command: item
                    .command
                    .as_deref()
                    .map(|text| bounded_redacted_text(text, 2_000)),
                argv: item
                    .argv
                    .iter()
                    .take(128)
                    .map(|arg| bounded_redacted_text(arg, 600))
                    .collect(),
                cwd: item
                    .cwd
                    .as_deref()
                    .map(|text| bounded_redacted_text(text, 1_000)),
                exit_code: item.exit_code,
                status: item.status,
                freshness: item.freshness,
                stdout_preview,
                stderr_preview,
                output_truncated,
                preview_limit_bytes: output_ref_i64(output_ref, "preview_limit_bytes"),
                stdout_size_bytes: output_ref_i64(output_ref, "stdout_size_bytes"),
                stderr_size_bytes: output_ref_i64(output_ref, "stderr_size_bytes"),
                stdout_sha256: output_ref_sha256(output_ref, "stdout_sha256"),
                stderr_sha256: output_ref_sha256(output_ref, "stderr_sha256"),
                stdout_truncated: output_ref_bool(output_ref, "stdout_truncated"),
                stderr_truncated: output_ref_bool(output_ref, "stderr_truncated"),
                output_ref: None,
                started_at: Some(item.started_at),
                finished_at: Some(item.finished_at),
            }
        })
        .collect()
}

async fn inspector_artifacts(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: WorkRecordId,
    links: &[WorkRecordLink],
    events: &[WorkEvent],
    evidence: &[WorkEvidence],
) -> Result<Vec<WorkspaceWorkArtifactRouteItem>, WorkspaceRouteError> {
    let mut artifacts = Vec::new();
    for item in evidence {
        if item.artifact_ref.is_some() {
            artifacts.push(
                work_artifact_route_item(
                    store,
                    workspace_id,
                    &work_id,
                    ArtifactRouteSource {
                        id: item.evidence_id.0.clone(),
                        source_kind: "evidence",
                        source_id: Some(item.evidence_id.0.clone()),
                        kind: Some(enum_json_label(&item.kind)),
                        label: item
                            .claim
                            .as_deref()
                            .or(item.command.as_deref())
                            .map(|text| bounded_redacted_text(text, 300)),
                        artifact_id: item.artifact_ref.as_ref().and_then(extract_artifact_id),
                        created_at: Some(item.created_at),
                    },
                )
                .await?,
            );
        }
    }
    for event in events {
        if event.artifact_ref.is_some() {
            artifacts.push(
                work_artifact_route_item(
                    store,
                    workspace_id,
                    &work_id,
                    ArtifactRouteSource {
                        id: event.event_id.0.clone(),
                        source_kind: "event",
                        source_id: Some(event.event_id.0.clone()),
                        kind: Some(enum_json_label(&event.event_type)),
                        label: event
                            .redacted_text
                            .as_deref()
                            .map(|text| bounded_redacted_text(text, 300)),
                        artifact_id: event.artifact_ref.as_ref().and_then(extract_artifact_id),
                        created_at: Some(event.created_at),
                    },
                )
                .await?,
            );
        }
    }
    for link in links {
        if link.target_kind == WorkLinkTargetKind::Artifact {
            artifacts.push(
                work_artifact_route_item(
                    store,
                    workspace_id,
                    &work_id,
                    ArtifactRouteSource {
                        id: link.link_id.0.clone(),
                        source_kind: "link",
                        source_id: Some(link.link_id.0.clone()),
                        kind: Some("artifact".to_string()),
                        label: link
                            .target_json
                            .as_ref()
                            .and_then(|value| value.get("name"))
                            .and_then(Value::as_str)
                            .or(link.target_id.as_deref())
                            .map(|text| bounded_redacted_text(text, 300)),
                        artifact_id: link.target_id.as_deref().and_then(parse_artifact_id_string),
                        created_at: Some(link.created_at),
                    },
                )
                .await?,
            );
        }
    }
    artifacts.sort_by(|left, right| left.id.cmp(&right.id));
    artifacts.dedup_by(|left, right| left.id == right.id);
    Ok(artifacts)
}

struct ArtifactRouteSource {
    id: String,
    source_kind: &'static str,
    source_id: Option<String>,
    kind: Option<String>,
    label: Option<String>,
    artifact_id: Option<ArtifactId>,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn work_artifact_route_item(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
    source: ArtifactRouteSource,
) -> Result<WorkspaceWorkArtifactRouteItem, WorkspaceRouteError> {
    let Some(artifact_id) = source.artifact_id else {
        return Ok(unavailable_work_artifact_item(
            source,
            "artifact reference does not include a session artifact id",
        ));
    };

    let Some(artifact) = store
        .get_artifact(artifact_id)
        .await
        .map_err(WorkspaceRouteError::internal)?
        .filter(|artifact| artifact.workspace_id == workspace_id)
    else {
        return Ok(unavailable_work_artifact_item(
            ArtifactRouteSource {
                artifact_id: Some(artifact_id),
                ..source
            },
            "artifact metadata unavailable",
        ));
    };

    Ok(available_work_artifact_item(
        workspace_id,
        work_id,
        source,
        artifact,
    ))
}

fn available_work_artifact_item(
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: &WorkRecordId,
    source: ArtifactRouteSource,
    artifact: Artifact,
) -> WorkspaceWorkArtifactRouteItem {
    let render_kind = artifact_render_kind(&artifact.mime_type);
    let route_url = format!(
        "/api/workspaces/{}/work/{}/artifacts/{}",
        workspace_id.0, work_id.0, artifact.id.0
    );
    let is_missing = artifact.missing.unwrap_or(false);
    let usable =
        !is_missing && !matches!(render_kind, WorkspaceWorkArtifactRenderKind::Unavailable);
    let thumbnail_url = matches!(render_kind, WorkspaceWorkArtifactRenderKind::RasterImage)
        .then(|| route_url.clone());
    let preview_url = matches!(
        render_kind,
        WorkspaceWorkArtifactRenderKind::RasterImage | WorkspaceWorkArtifactRenderKind::Text
    )
    .then(|| route_url.clone());

    WorkspaceWorkArtifactRouteItem {
        id: source.id,
        artifact_id: Some(artifact.id.0.to_string()),
        source_kind: Some(source.source_kind.to_string()),
        source_id: source.source_id,
        kind: source.kind,
        label: source.label,
        display_name: artifact
            .name
            .as_deref()
            .map(|text| bounded_redacted_text(text, 300)),
        mime_type: Some(bounded_redacted_text(&artifact.mime_type, 200)),
        bytes: Some(artifact.bytes),
        missing: is_missing,
        unavailable_reason: is_missing
            .then(|| "artifact file is missing or unavailable".to_string()),
        render_kind,
        download_url: usable.then(|| route_url.clone()),
        open_url: usable.then(|| route_url.clone()),
        thumbnail_url: usable.then_some(()).and(thumbnail_url),
        preview_url: usable.then_some(()).and(preview_url),
        created_at: Some(artifact.created_at),
    }
}

fn unavailable_work_artifact_item(
    source: ArtifactRouteSource,
    reason: impl Into<String>,
) -> WorkspaceWorkArtifactRouteItem {
    WorkspaceWorkArtifactRouteItem {
        id: source.id,
        artifact_id: source.artifact_id.map(|id| id.0.to_string()),
        source_kind: Some(source.source_kind.to_string()),
        source_id: source.source_id,
        kind: source.kind,
        label: source.label,
        display_name: None,
        mime_type: None,
        bytes: None,
        missing: true,
        unavailable_reason: Some(reason.into()),
        render_kind: WorkspaceWorkArtifactRenderKind::Unavailable,
        download_url: None,
        open_url: None,
        thumbnail_url: None,
        preview_url: None,
        created_at: source.created_at,
    }
}

fn artifact_render_kind(mime_type: &str) -> WorkspaceWorkArtifactRenderKind {
    let mime = mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => {
            WorkspaceWorkArtifactRenderKind::RasterImage
        }
        "text/plain" | "text/markdown" | "application/json" | "text/csv" => {
            WorkspaceWorkArtifactRenderKind::Text
        }
        "text/html" | "image/svg+xml" => WorkspaceWorkArtifactRenderKind::DownloadOnly,
        _ => WorkspaceWorkArtifactRenderKind::DownloadOnly,
    }
}

fn extract_artifact_id(value: &Value) -> Option<ArtifactId> {
    for key in ["artifact_id", "id"] {
        if let Some(id) = value
            .get(key)
            .and_then(Value::as_str)
            .and_then(parse_artifact_id_string)
        {
            return Some(id);
        }
    }
    None
}

fn parse_artifact_id_string(value: &str) -> Option<ArtifactId> {
    uuid::Uuid::parse_str(value.trim()).ok().map(ArtifactId)
}

fn work_artifact_is_linked(raw: &RawWorkDetail, artifact: &Artifact) -> bool {
    raw.links.iter().any(|link| match link.target_kind {
        WorkLinkTargetKind::Artifact => link
            .target_id
            .as_deref()
            .and_then(parse_artifact_id_string)
            .is_some_and(|id| id == artifact.id),
        WorkLinkTargetKind::Session => link
            .target_id
            .as_deref()
            .and_then(parse_session_id_string)
            .is_some_and(|id| id == artifact.session_id),
        _ => false,
    }) || raw
        .evidence
        .iter()
        .any(|evidence| artifact_ref_matches(evidence.artifact_ref.as_ref(), artifact))
        || raw
            .events
            .iter()
            .any(|event| artifact_ref_matches(event.artifact_ref.as_ref(), artifact))
        || raw.events.iter().any(|event| {
            event.event_type == WorkEventType::ArtifactCreated
                && event
                    .source_id
                    .as_deref()
                    .and_then(parse_artifact_id_string)
                    .is_some_and(|id| id == artifact.id)
        })
}

fn artifact_ref_matches(value: Option<&Value>, artifact: &Artifact) -> bool {
    let Some(value) = value else {
        return false;
    };
    if extract_artifact_id(value) != Some(artifact.id) {
        return false;
    }
    value
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(parse_session_id_string)
        .is_none_or(|session_id| session_id == artifact.session_id)
}

fn parse_session_id_string(value: &str) -> Option<SessionId> {
    uuid::Uuid::parse_str(value.trim()).ok().map(SessionId)
}

fn inspector_timeline_items(
    events: &[WorkEvent],
    evidence: &[WorkEvidence],
) -> Vec<WorkspaceWorkTimelineItemRouteResponse> {
    let mut items = events
        .iter()
        .map(|event| WorkspaceWorkTimelineItemRouteResponse {
            sequence: event.sequence,
            event_time: event.event_time,
            kind: enum_json_label(&event.event_type),
            title: event
                .redacted_text
                .as_deref()
                .map(|text| bounded_redacted_text(text, 160))
                .unwrap_or_else(|| enum_json_label(&event.event_type)),
            detail: event
                .source_kind
                .as_deref()
                .map(|kind| bounded_redacted_text(kind, 120)),
            source_event_id: Some(event.event_id.clone()),
            source_evidence_id: None,
        })
        .collect::<Vec<_>>();
    items.extend(evidence.iter().map(|item| {
        WorkspaceWorkTimelineItemRouteResponse {
            sequence: i64::MAX,
            event_time: item.finished_at,
            kind: enum_json_label(&item.kind),
            title: item
                .claim
                .as_deref()
                .or(item.command.as_deref())
                .map(|text| bounded_redacted_text(text, 160))
                .unwrap_or_else(|| "Evidence observed".to_string()),
            detail: Some(format!(
                "{} / {}",
                enum_json_label(&item.status),
                enum_json_label(&item.freshness)
            )),
            source_event_id: None,
            source_evidence_id: Some(item.evidence_id.clone()),
        }
    }));
    items.sort_by(|left, right| {
        left.event_time
            .cmp(&right.event_time)
            .then_with(|| left.sequence.cmp(&right.sequence))
    });
    items
}

#[allow(clippy::too_many_arguments)]
fn inspector_context_value(
    work: &WorkspaceWorkRecordRouteItem,
    trust: &WorkspaceWorkTrustRouteSummary,
    summaries: &[WorkspaceWorkSummaryRouteItem],
    summary_claims: &[WorkspaceWorkSummaryClaimRouteItem],
    evidence: &[WorkspaceWorkEvidenceRouteItem],
    commands: &[WorkspaceWorkCommandPreviewRouteResponse],
    artifacts: &[WorkspaceWorkArtifactRouteItem],
    change_summary: &WorkspaceWorkChangeSummaryRouteResponse,
    duplicate_strong_links: &[WorkspaceWorkDuplicateStrongLinkRouteItem],
) -> Value {
    json!({
        "objective": work.objective.as_deref().or(work.title.as_deref()).unwrap_or("Untitled Work"),
        "current_result": trust.reason,
        "recommended_next_action": trust.recommended_next_action,
        "open_risks": trust.open_risks,
        "key_decisions": summaries.iter().take(8).map(|summary| {
            json!({
                "text": bounded_redacted_text(&summary.text, 1_200),
                "freshness": summary.freshness,
                "citations": [{
                    "source_kind": "summary",
                    "source_id": summary.summary_id,
                    "freshness": summary.freshness,
                }],
            })
        }).collect::<Vec<_>>(),
        "summary_claims": summary_claims.iter().take(16).map(|claim| {
            json!({
                "claim_text": bounded_redacted_text(&claim.claim_text, 800),
                "claim_kind": claim.claim_kind,
                "source_kind": claim.source_kind,
                "source_id": claim.source_id,
                "freshness": claim.freshness,
            })
        }).collect::<Vec<_>>(),
        "evidence": evidence.iter().take(16).map(|item| {
            json!({
                "evidence_id": item.evidence_id,
                "kind": item.kind,
                "status": item.status,
                "freshness": item.freshness,
                "claim": item.claim,
                "command": item.command,
                "exit_code": item.exit_code,
            })
        }).collect::<Vec<_>>(),
        "commands": commands.iter().take(16).map(|item| {
            json!({
                "id": item.id,
                "command": item.command,
                "argv": item.argv,
                "exit_code": item.exit_code,
                "status": item.status,
                "freshness": item.freshness,
                "stdout_preview": item.stdout_preview,
                "stderr_preview": item.stderr_preview,
                "output_truncated": item.output_truncated,
            })
        }).collect::<Vec<_>>(),
        "changes": {
            "change_sets": change_summary.change_sets,
            "contributions": change_summary.contributions,
            "pull_requests": change_summary.pull_requests,
            "commits": change_summary.commits,
        },
        "artifacts": artifacts.iter().take(16).map(|artifact| {
            json!({
                "id": artifact.id,
                "artifact_id": artifact.artifact_id,
                "kind": artifact.kind,
                "label": artifact.label,
                "display_name": artifact.display_name,
                "mime_type": artifact.mime_type,
                "bytes": artifact.bytes,
                "missing": artifact.missing,
                "unavailable_reason": artifact.unavailable_reason,
                "render_kind": artifact.render_kind,
                "download_url": artifact.download_url,
                "open_url": artifact.open_url,
                "thumbnail_url": artifact.thumbnail_url,
                "preview_url": artifact.preview_url,
            })
        }).collect::<Vec<_>>(),
        "duplicate_strong_links": duplicate_strong_links,
        "raw_transcript_included": false,
    })
}

fn safe_json_response(
    value: Value,
    redaction_notes: Vec<String>,
) -> WorkspaceWorkSafeJsonRouteResponse {
    WorkspaceWorkSafeJsonRouteResponse {
        value: redact_route_value(&value),
        redacted: true,
        redaction_notes,
    }
}

fn safe_output_preview(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|text| bounded_redacted_text(text, COMMAND_OUTPUT_PREVIEW_LIMIT))
}

fn output_ref_i64(value: Option<&Value>, key: &str) -> Option<i64> {
    let value = value?.get(key)?;
    value_i64(value)
}

fn value_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number.max(0));
    }
    if let Some(number) = value.as_u64() {
        return i64::try_from(number).ok();
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<i64>().ok())
        .map(|number| number.max(0))
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as i64)
        })
}

fn output_ref_bool(value: Option<&Value>, key: &str) -> bool {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn output_ref_sha256(value: Option<&Value>, key: &str) -> Option<String> {
    let text = value?.get(key)?.as_str()?.trim();
    if text.len() == 64 && text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(text.to_ascii_lowercase())
    } else {
        None
    }
}

fn safe_relative_display_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        || value.chars().any(char::is_control)
    {
        return None;
    }
    Some(bounded_redacted_text(value, 1_000))
}

fn safe_http_url_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    if value.starts_with("https://") || value.starts_with("http://") {
        Some(bounded_redacted_text(value, 2_000))
    } else {
        None
    }
}

fn enum_json_label<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn route_work_record(
    work: &WorkRecord,
    trust_verdict: Option<WorkTrustVerdict>,
    summary_freshness: Option<WorkSummaryFreshness>,
) -> WorkspaceWorkRecordRouteItem {
    WorkspaceWorkRecordRouteItem {
        work_id: work.work_id.clone(),
        workspace_id: work.workspace_id,
        title: work
            .title
            .as_deref()
            .map(|text| bounded_redacted_text(text, 1_000)),
        objective: work
            .objective
            .as_deref()
            .map(|text| bounded_redacted_text(text, 2_000)),
        lifecycle: work.lifecycle,
        primary_branch: work
            .primary_branch
            .as_deref()
            .map(|text| bounded_redacted_text(text, 500)),
        base_commit: work.base_commit.clone(),
        head_commit: work.head_commit.clone(),
        trust_verdict: trust_verdict.unwrap_or(work.trust_verdict),
        summary_freshness: summary_freshness.unwrap_or(work.summary_freshness),
        created_at: work.created_at,
        updated_at: work.updated_at,
        schema_version: work.schema_version,
    }
}

fn route_work_link(link: &WorkRecordLink) -> WorkspaceWorkLinkRouteItem {
    WorkspaceWorkLinkRouteItem {
        link_id: link.link_id.clone(),
        work_id: link.work_id.clone(),
        workspace_id: link.workspace_id,
        target_kind: link.target_kind,
        target_id: link
            .target_id
            .as_deref()
            .map(|text| bounded_redacted_text(text, 1_000)),
        target_json: link.target_json.as_ref().map(redact_route_value),
        role: link.role,
        source: link.source,
        fidelity: link.fidelity,
        trust: link.trust,
        created_at: link.created_at,
        updated_at: link.updated_at,
        schema_version: link.schema_version,
    }
}

fn route_work_link_metadata_only(link: &WorkRecordLink) -> WorkspaceWorkLinkRouteItem {
    let mut item = route_work_link(link);
    item.target_json = None;
    item
}

fn route_work_event(event: &WorkEvent) -> WorkspaceWorkEventRouteItem {
    WorkspaceWorkEventRouteItem {
        event_id: event.event_id.clone(),
        work_id: event.work_id.clone(),
        workspace_id: event.workspace_id,
        sequence: event.sequence,
        source_kind: event.source_kind.clone(),
        source_id: event.source_id.clone(),
        event_type: event.event_type,
        event_time: event.event_time,
        actor_kind: event.actor_kind,
        provider: event.provider.clone(),
        harness: event.harness.clone(),
        model: event.model.clone(),
        redaction_class: event.redaction_class,
        source: event.source,
        fidelity: event.fidelity,
        trust: event.trust,
        redacted_text: event
            .redacted_text
            .as_deref()
            .map(|text| bounded_redacted_text(text, EVENT_TEXT_LIMIT)),
        created_at: event.created_at,
        schema_version: event.schema_version,
    }
}

fn route_work_evidence(evidence: &WorkEvidence) -> WorkspaceWorkEvidenceRouteItem {
    WorkspaceWorkEvidenceRouteItem {
        evidence_id: evidence.evidence_id.clone(),
        work_id: evidence.work_id.clone(),
        workspace_id: evidence.workspace_id,
        kind: evidence.kind,
        status: evidence.status,
        freshness: evidence.freshness,
        claim: evidence
            .claim
            .as_deref()
            .map(|text| bounded_redacted_text(text, 1_200)),
        command: evidence
            .command
            .as_deref()
            .map(|text| bounded_redacted_text(text, 2_000)),
        argv: evidence
            .argv
            .iter()
            .map(|arg| bounded_redacted_text(arg, 600))
            .collect(),
        cwd: evidence
            .cwd
            .as_deref()
            .map(|text| bounded_redacted_text(text, 1_000)),
        exit_code: evidence.exit_code,
        head_sha: evidence.head_sha.clone(),
        branch: evidence
            .branch
            .as_deref()
            .map(|text| bounded_redacted_text(text, 500)),
        output_ref: evidence.output_ref.as_ref().map(redact_route_value),
        artifact_ref: evidence.artifact_ref.as_ref().map(redact_route_value),
        source: evidence.source,
        fidelity: evidence.fidelity,
        trust: evidence.trust,
        started_at: evidence.started_at,
        finished_at: evidence.finished_at,
        created_at: evidence.created_at,
        updated_at: evidence.updated_at,
        schema_version: evidence.schema_version,
    }
}

fn route_work_inspector_evidence(evidence: &WorkEvidence) -> WorkspaceWorkEvidenceRouteItem {
    let mut item = route_work_evidence(evidence);
    item.output_ref = None;
    item.artifact_ref = None;
    item
}

fn route_work_summary(
    summary: &WorkSummary,
    material_key: &str,
    text_limit: usize,
) -> WorkspaceWorkSummaryRouteItem {
    WorkspaceWorkSummaryRouteItem {
        summary_id: summary.summary_id.clone(),
        work_id: summary.work_id.clone(),
        workspace_id: summary.workspace_id,
        kind: summary.kind,
        audience: summary.audience,
        text: bounded_redacted_text(&summary.text, text_limit),
        structured_json: summary.structured_json.as_ref().map(redact_route_value),
        generation_method: summary.generation_method,
        provider: summary.provider.clone(),
        model: summary.model.clone(),
        template: summary.template.clone(),
        source_material_left_machine: summary.source_material_left_machine,
        freshness: effective_summary_freshness(
            summary.freshness,
            summary.source_revision_key.as_deref(),
            material_key,
        ),
        source_revision_key: summary.source_revision_key.clone(),
        generated_at: summary.generated_at,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        schema_version: summary.schema_version,
    }
}

fn route_work_summary_claim(
    claim: &WorkSummaryClaim,
    material_key: &str,
) -> WorkspaceWorkSummaryClaimRouteItem {
    WorkspaceWorkSummaryClaimRouteItem {
        claim_id: claim.claim_id.clone(),
        summary_id: claim.summary_id.clone(),
        work_id: claim.work_id.clone(),
        workspace_id: claim.workspace_id,
        claim_text: bounded_redacted_text(&claim.claim_text, 2_000),
        claim_kind: claim.claim_kind.clone(),
        source_kind: claim.source_kind.clone(),
        source_id: claim.source_id.clone(),
        record_hash: claim.record_hash.clone(),
        freshness: effective_summary_freshness(
            claim.freshness,
            claim.record_hash.as_deref(),
            material_key,
        ),
        redaction_class: claim.redaction_class,
        created_at: claim.created_at,
        schema_version: claim.schema_version,
    }
}

fn evidence_summary(evidence: &[WorkEvidence]) -> WorkspaceWorkEvidenceSummaryRouteResponse {
    WorkspaceWorkEvidenceSummaryRouteResponse {
        total: evidence.len(),
        passing: evidence
            .iter()
            .filter(|item| item.status == WorkEvidenceStatus::ObservedPass)
            .count(),
        failing: evidence
            .iter()
            .filter(|item| item.status == WorkEvidenceStatus::ObservedFail)
            .count(),
        stale: evidence
            .iter()
            .filter(|item| item.freshness == WorkEvidenceFreshness::Stale)
            .count(),
        missing: usize::from(evidence.is_empty()),
    }
}

fn computed_trust_verdict(work: &WorkRecord, evidence: &[WorkEvidence]) -> WorkTrustVerdict {
    if evidence
        .iter()
        .any(|item| item.status == WorkEvidenceStatus::ObservedFail)
    {
        WorkTrustVerdict::Failed
    } else if evidence.is_empty() {
        WorkTrustVerdict::MissingEvidence
    } else if evidence
        .iter()
        .any(|item| item.freshness == WorkEvidenceFreshness::Stale)
    {
        WorkTrustVerdict::Stale
    } else if evidence.iter().any(|item| {
        item.status == WorkEvidenceStatus::ObservedPass
            && item.freshness == WorkEvidenceFreshness::Fresh
            && item.trust == RecordTrust::Verified
    }) {
        WorkTrustVerdict::Verified
    } else if evidence
        .iter()
        .any(|item| item.status == WorkEvidenceStatus::ObservedPass)
    {
        WorkTrustVerdict::Partial
    } else {
        work.trust_verdict
    }
}

fn trust_summary(work: &WorkRecord, evidence: &[WorkEvidence]) -> WorkspaceWorkTrustRouteSummary {
    let verdict = computed_trust_verdict(work, evidence);
    let reason = match verdict {
        WorkTrustVerdict::Verified => {
            "Fresh verified-provenance evidence is present for this Work record."
        }
        WorkTrustVerdict::Stale => "Some evidence no longer matches the current Work fingerprint.",
        WorkTrustVerdict::MissingEvidence => "No evidence has been recorded for this Work record.",
        WorkTrustVerdict::Partial => {
            "Some evidence is local, incomplete, imported, or lacks verified provenance."
        }
        WorkTrustVerdict::UntrustedLocalCapture => {
            "This record includes user-space local capture; treat it as context, not proof."
        }
        WorkTrustVerdict::Failed => "At least one linked evidence item failed.",
    }
    .to_string();
    let recommended_next_action = match verdict {
        WorkTrustVerdict::Verified => "Review the diff and citations.",
        WorkTrustVerdict::Stale => "Rerun the stale evidence commands before review.",
        WorkTrustVerdict::MissingEvidence => {
            "Add evidence with `ctx work evidence <work-id> run -- <command>`."
        }
        WorkTrustVerdict::Partial => {
            "Add verified provenance, fingerprints, artifacts, or citations."
        }
        WorkTrustVerdict::UntrustedLocalCapture => "Link a PR/commit and add fresh evidence.",
        WorkTrustVerdict::Failed => "Fix the failing evidence before marking this ready.",
    }
    .to_string();
    let open_risks = if verdict == WorkTrustVerdict::Verified {
        Vec::new()
    } else {
        vec![reason.clone()]
    };
    WorkspaceWorkTrustRouteSummary {
        verdict,
        reason,
        recommended_next_action,
        open_risks,
    }
}

fn aggregate_summary_freshness(
    summaries: &[WorkSummary],
    material_key: &str,
) -> WorkSummaryFreshness {
    let summary_refs = summaries.iter().collect::<Vec<_>>();
    aggregate_summary_freshness_refs(&summary_refs, material_key)
}

fn aggregate_summary_freshness_refs(
    summaries: &[&WorkSummary],
    material_key: &str,
) -> WorkSummaryFreshness {
    if summaries.is_empty() {
        return WorkSummaryFreshness::Missing;
    }
    let mut saw_partial = false;
    for summary in summaries {
        match effective_summary_freshness(
            summary.freshness,
            summary.source_revision_key.as_deref(),
            material_key,
        ) {
            WorkSummaryFreshness::Stale => return WorkSummaryFreshness::Stale,
            WorkSummaryFreshness::Missing | WorkSummaryFreshness::Partial => saw_partial = true,
            WorkSummaryFreshness::Fresh | WorkSummaryFreshness::Locked => {}
        }
    }
    if saw_partial {
        WorkSummaryFreshness::Partial
    } else {
        WorkSummaryFreshness::Fresh
    }
}

fn effective_summary_freshness(
    stored: WorkSummaryFreshness,
    source_revision_key: Option<&str>,
    material_key: &str,
) -> WorkSummaryFreshness {
    match stored {
        WorkSummaryFreshness::Locked => WorkSummaryFreshness::Locked,
        WorkSummaryFreshness::Fresh if source_revision_key == Some(material_key) => {
            WorkSummaryFreshness::Fresh
        }
        WorkSummaryFreshness::Fresh => WorkSummaryFreshness::Stale,
        other => other,
    }
}

fn material_revision_key(
    work: &WorkRecord,
    links: &[WorkRecordLink],
    events: &[WorkEvent],
    evidence: &[WorkEvidence],
    change_sets: &[ChangeSet],
    contributions: &[Contribution],
) -> String {
    let material_events: Vec<&WorkEvent> = events
        .iter()
        .filter(|event| {
            !matches!(
                event.event_type,
                WorkEventType::EvidenceObserved | WorkEventType::SummaryGenerated
            )
        })
        .collect();
    let value = json!({
        "work": {
            "work_id": work.work_id,
            "lifecycle": work.lifecycle,
            "head_commit": work.head_commit,
        },
        "links": links,
        "events": material_events,
        "evidence": evidence,
        "change_sets": change_sets,
        "contributions": contributions,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let digest = sha2::Sha256::digest(&bytes);
    hex::encode(digest)
}

fn pull_request_links(links: &[WorkRecordLink]) -> Vec<Value> {
    links
        .iter()
        .filter(|link| link.target_kind == ctx_core::models::WorkLinkTargetKind::PullRequest)
        .filter_map(inspector_pull_request_work_link_value)
        .collect()
}

fn inspector_pull_request_work_link_value(link: &WorkRecordLink) -> Option<Value> {
    let target = link.target_json.as_ref()?.as_object()?;
    let nested = target
        .get("pull_request")
        .and_then(Value::as_object)
        .unwrap_or(target);
    let provider = nested
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| target.get("provider").and_then(Value::as_str))
        .unwrap_or("unknown");
    let owner = nested
        .get("owner")
        .and_then(Value::as_str)
        .or_else(|| target.get("owner").and_then(Value::as_str))
        .unwrap_or("unknown");
    let repo = nested
        .get("repo")
        .and_then(Value::as_str)
        .or_else(|| target.get("repo").and_then(Value::as_str))
        .unwrap_or("unknown");
    let number = nested
        .get("number")
        .or_else(|| target.get("number"))
        .and_then(value_i64)
        .or_else(|| {
            link.target_id
                .as_deref()
                .and_then(|text| text.rsplit('#').next())
                .and_then(|text| text.parse::<i64>().ok())
        })
        .unwrap_or_default();
    Some(inspector_pull_request_ref_value(
        provider,
        owner,
        repo,
        number,
        nested
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| target.get("id").and_then(Value::as_str)),
        nested
            .get("url")
            .and_then(Value::as_str)
            .or_else(|| target.get("url").and_then(Value::as_str))
            .or_else(|| nested.get("html_url").and_then(Value::as_str))
            .or_else(|| target.get("html_url").and_then(Value::as_str)),
        nested
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| target.get("title").and_then(Value::as_str)),
        nested
            .get("state")
            .and_then(Value::as_str)
            .or_else(|| target.get("state").and_then(Value::as_str)),
    ))
}

fn commit_links(links: &[WorkRecordLink]) -> Vec<String> {
    links
        .iter()
        .filter(|link| link.target_kind == ctx_core::models::WorkLinkTargetKind::Commit)
        .filter_map(|link| {
            link.target_id
                .as_deref()
                .map(|text| bounded_redacted_text(text, 200))
        })
        .collect()
}

fn redact_route_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(bounded_redacted_text(text, 16 * 1024)),
        Value::Array(items) => Value::Array(items.iter().map(redact_route_value).collect()),
        Value::Object(object) => {
            let mut redacted = Map::new();
            for (key, value) in object {
                let key_lc = key.to_ascii_lowercase();
                if matches!(
                    key_lc.as_str(),
                    "payload_json"
                        | "absolute_path"
                        | "repo_root"
                        | "root_path"
                        | "primary_repo_root"
                        | "fingerprint_json"
                        | "current_fingerprint_json"
                ) {
                    redacted.insert(key.clone(), Value::String("[redacted:local_detail]".into()));
                } else if is_sensitive_key(key) {
                    redacted.insert(key.clone(), Value::String("[redacted:secret]".into()));
                } else if key_lc == "relative_path" || key_lc == "path" || key_lc == "cwd" {
                    redacted.insert(
                        key.clone(),
                        Value::String(bounded_redacted_text(value.as_str().unwrap_or(""), 1_000)),
                    );
                } else {
                    redacted.insert(key.clone(), redact_route_value(value));
                }
            }
            Value::Object(redacted)
        }
        other => other.clone(),
    }
}

fn bounded_redacted_text(value: &str, limit: usize) -> String {
    let redacted = redact_route_text(value);
    if redacted.len() <= limit {
        return redacted;
    }
    let mut end = 0;
    for (idx, _) in redacted.char_indices() {
        if idx > limit {
            break;
        }
        end = idx;
    }
    format!("{}\n[truncated]", &redacted[..end])
}

fn redact_route_text(value: &str) -> String {
    let mut redacted = ctx_core::redaction::redact_sensitive(value);
    for marker in [
        "/home/",
        "/Users/",
        "/tmp/",
        "/var/folders/",
        "/private/var/",
    ] {
        redacted = redact_path_segments(redacted, marker);
    }
    for marker in ["C:\\Users\\", "C:/Users/"] {
        redacted = redact_path_segments(redacted, marker);
    }
    redacted
}

fn redact_path_segments(input: String, marker: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input.as_str();
    while let Some(start) = rest.find(marker) {
        output.push_str(&rest[..start]);
        output.push_str("[redacted:local_path]");
        let matched = &rest[start..];
        let end = matched
            .find(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '"' | '\'' | ')' | ']' | '}' | '<' | '>' | ',' | ';')
            })
            .unwrap_or(matched.len());
        rest = &matched[end..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use ctx_core::ids::{WorkEventId, WorkRecordId, WorkRecordLinkId, WorkspaceId};
    use ctx_core::models::{
        RecordFidelity, RecordSource, RecordTrust, WorkActorKind, WorkEventType, WorkLifecycle,
        WorkLinkRole, WorkLinkTargetKind, WorkRedactionClass,
    };

    #[test]
    fn route_work_event_omits_payload_json_by_default() {
        let now = Utc::now();
        let event = WorkEvent {
            event_id: WorkEventId::new(),
            work_id: WorkRecordId::new(),
            workspace_id: WorkspaceId::new(),
            sequence: 1,
            source_kind: Some("session".to_string()),
            source_id: Some("session-1".to_string()),
            event_type: WorkEventType::AssistantMessage,
            event_time: now,
            actor_kind: WorkActorKind::Agent,
            provider: Some("provider".to_string()),
            harness: Some("harness".to_string()),
            model: Some("model".to_string()),
            redaction_class: WorkRedactionClass::LocalRedacted,
            source: RecordSource::Session,
            fidelity: RecordFidelity::Summary,
            trust: RecordTrust::Low,
            payload_json: Some(json!({
                "content": "sk-test-raw-secret",
                "absolute_path": "/home/daddy/private/repo/file.rs"
            })),
            redacted_text: Some("safe redacted event".to_string()),
            artifact_ref: Some(json!({"absolute_path": "/home/daddy/private/output.log"})),
            created_at: now,
            schema_version: 1,
        };

        let value = serde_json::to_value(route_work_event(&event)).unwrap();
        assert!(value.get("payload_json").is_none());
        assert!(value.get("artifact_ref").is_none());
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("sk-test-raw-secret"));
        assert!(!serialized.contains("/home/daddy/private"));
        assert!(serialized.contains("safe redacted event"));
    }

    #[test]
    fn summary_create_rejects_provider_backed_request() {
        let request = WorkspaceWorkSummaryCreateRouteRequest {
            source_material_left_machine: true,
            generation_method: WorkSummaryGenerationMethod::ProviderLlm,
            provider: Some("external".to_string()),
            ..WorkspaceWorkSummaryCreateRouteRequest::default()
        };

        let error = validate_summary_create_request(&request).unwrap_err();
        assert_eq!(
            error.kind(),
            ctx_route_contracts::workspaces::WorkspaceRouteErrorKind::BadRequest
        );
    }

    #[test]
    fn route_graph_redaction_omits_local_paths_and_secret_metadata() {
        let value = redact_route_serializable(&json!({
            "fingerprint": {
                "repo_root": "/home/daddy/private/repo",
            },
            "metadata_json": {
                "token": "sk-test-raw-secret",
                "safe": "kept",
            },
            "description": "uses openai_api_key=sk-test-raw-secret at /home/daddy/private/repo",
        }));
        let serialized = serde_json::to_string(&value).unwrap();

        assert!(!serialized.contains("sk-test-raw-secret"));
        assert!(!serialized.contains("/home/daddy/private"));
        assert!(serialized.contains("[redacted"));
    }

    #[test]
    fn fresh_local_evidence_is_partial_without_verified_provenance() {
        let workspace_id = WorkspaceId::new();
        let work_id = WorkRecordId::new();
        let now = Utc::now();
        let work = test_route_work_record(workspace_id, work_id.clone(), now);
        let mut evidence = test_route_evidence(workspace_id, work_id, now);

        evidence.trust = RecordTrust::Medium;
        assert_eq!(
            computed_trust_verdict(&work, &[evidence.clone()]),
            WorkTrustVerdict::Partial
        );

        evidence.trust = RecordTrust::Verified;
        assert_eq!(
            computed_trust_verdict(&work, &[evidence]),
            WorkTrustVerdict::Verified
        );
    }

    #[test]
    fn route_evidence_trust_downgrades_client_verified_claims() {
        assert_eq!(
            route_evidence_trust_or(RecordTrust::Verified),
            RecordTrust::Medium
        );
        assert_eq!(
            route_evidence_trust_or(RecordTrust::High),
            RecordTrust::High
        );
        assert_eq!(
            route_evidence_trust_or(RecordTrust::Unknown),
            RecordTrust::Medium
        );
    }

    #[test]
    fn pull_request_links_include_target_id_fallback_when_json_is_missing() {
        let workspace_id = WorkspaceId::new();
        let work_id = WorkRecordId::new();
        let now = Utc::now();
        let links = vec![WorkRecordLink {
            link_id: WorkRecordLinkId::new(),
            work_id,
            workspace_id,
            target_kind: WorkLinkTargetKind::PullRequest,
            target_id: Some("github:ctxrs/ctx#123".to_string()),
            target_json: None,
            role: WorkLinkRole::Result,
            source: RecordSource::Manual,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Medium,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        }];

        let pull_requests = pull_request_links(&links);

        assert_eq!(pull_requests.len(), 1);
        assert_eq!(pull_requests[0]["target_id"], "github:ctxrs/ctx#123");
    }

    #[test]
    fn material_revision_key_ignores_bookkeeping_timestamps_and_derived_events() {
        let workspace_id = WorkspaceId::new();
        let work_id = WorkRecordId::new();
        let now = Utc::now();
        let mut work = test_route_work_record(workspace_id, work_id.clone(), now);
        let base = material_revision_key(&work, &[], &[], &[], &[], &[]);

        work.updated_at = now + Duration::seconds(30);
        assert_eq!(material_revision_key(&work, &[], &[], &[], &[], &[]), base);

        let derived_event = test_route_event(
            workspace_id,
            work_id.clone(),
            WorkEventType::SummaryGenerated,
            now + Duration::seconds(60),
        );
        assert_eq!(
            material_revision_key(&work, &[], &[derived_event], &[], &[], &[]),
            base
        );

        let source_event = test_route_event(
            workspace_id,
            work_id,
            WorkEventType::AssistantMessage,
            now + Duration::seconds(90),
        );
        assert_ne!(
            material_revision_key(&work, &[], &[source_event], &[], &[], &[]),
            base
        );
    }

    fn test_route_work_record(
        workspace_id: WorkspaceId,
        work_id: WorkRecordId,
        now: chrono::DateTime<Utc>,
    ) -> WorkRecord {
        WorkRecord {
            work_id,
            workspace_id,
            title: Some("Route Work".to_string()),
            objective: None,
            lifecycle: WorkLifecycle::Active,
            primary_repo_root: None,
            primary_branch: Some("main".to_string()),
            base_commit: None,
            head_commit: Some("abc123".to_string()),
            current_diff_fingerprint: None,
            trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
            summary_freshness: WorkSummaryFreshness::Missing,
            metadata_json: None,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        }
    }

    fn test_route_event(
        workspace_id: WorkspaceId,
        work_id: WorkRecordId,
        event_type: WorkEventType,
        now: chrono::DateTime<Utc>,
    ) -> WorkEvent {
        WorkEvent {
            event_id: WorkEventId::new(),
            work_id,
            workspace_id,
            sequence: 1,
            source_kind: Some("test".to_string()),
            source_id: Some("test-1".to_string()),
            event_type,
            event_time: now,
            actor_kind: WorkActorKind::Agent,
            provider: None,
            harness: None,
            model: None,
            redaction_class: WorkRedactionClass::LocalRedacted,
            source: RecordSource::Session,
            fidelity: RecordFidelity::Summary,
            trust: RecordTrust::Low,
            payload_json: None,
            redacted_text: Some("source event".to_string()),
            artifact_ref: None,
            created_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        }
    }

    fn test_route_evidence(
        workspace_id: WorkspaceId,
        work_id: WorkRecordId,
        now: chrono::DateTime<Utc>,
    ) -> WorkEvidence {
        WorkEvidence {
            evidence_id: WorkEvidenceId::new(),
            work_id,
            workspace_id,
            kind: ctx_core::models::WorkEvidenceKind::Test,
            status: WorkEvidenceStatus::ObservedPass,
            freshness: WorkEvidenceFreshness::Fresh,
            claim: Some("Observed test passed".to_string()),
            command: Some("cargo test".to_string()),
            argv: vec!["cargo".to_string(), "test".to_string()],
            cwd: None,
            exit_code: Some(0),
            repo_root: None,
            head_sha: None,
            branch: None,
            fingerprint: None,
            current_fingerprint: None,
            output_ref: None,
            artifact_ref: None,
            source: RecordSource::Worktree,
            fidelity: RecordFidelity::Exact,
            trust: RecordTrust::Medium,
            started_at: now,
            finished_at: now,
            created_at: now,
            updated_at: now,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        }
    }
}
