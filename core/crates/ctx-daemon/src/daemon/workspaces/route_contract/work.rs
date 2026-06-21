use ctx_core::ids::{ChangeSetId, ContributionId, WorkRecordId};
use ctx_core::models::{
    ChangeSet, Contribution, WorkEvent, WorkEvidence, WorkEvidenceFreshness, WorkEvidenceStatus,
    WorkRecord, WorkRecordLink, WorkSummary, WorkSummaryClaim, WorkSummaryFreshness,
    WorkTrustVerdict,
};
use ctx_route_contracts::workspaces::{
    WorkspaceRouteParams, WorkspaceWorkChangeSummaryRouteResponse, WorkspaceWorkContextRouteQuery,
    WorkspaceWorkContextRouteResponse, WorkspaceWorkDetailRouteResponse,
    WorkspaceWorkDuplicateStrongLinkRouteItem, WorkspaceWorkEventRouteItem,
    WorkspaceWorkEvidenceRouteItem, WorkspaceWorkEvidenceRouteResponse,
    WorkspaceWorkEvidenceSummaryRouteResponse, WorkspaceWorkLinkRouteItem,
    WorkspaceWorkListRouteQuery, WorkspaceWorkListRouteResponse, WorkspaceWorkRecordRouteItem,
    WorkspaceWorkReportRouteResponse, WorkspaceWorkSummaryClaimRouteItem,
    WorkspaceWorkSummaryRouteItem, WorkspaceWorkTimelineRouteQuery,
    WorkspaceWorkTimelineRouteResponse, WorkspaceWorkTrustRouteSummary,
};
use serde_json::{json, Map, Value};
use sha2::Digest;

use super::super::{workspace_store_route_error, WorkspaceRouteError};
use crate::daemon::WorkspaceWorkHandle;

const REPORT_TEXT_LIMIT: usize = 16 * 1024;
const CONTEXT_TEXT_LIMIT: usize = 6 * 1024;
const EVENT_TEXT_LIMIT: usize = 8 * 1024;

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
        let material_key = material_revision_key(
            &raw.work,
            &raw.links,
            &raw.events,
            &raw.evidence,
            &raw.change_sets,
            &raw.contributions,
        );
        let summary_freshness = aggregate_summary_freshness(&raw.summaries, &material_key);
        let trust = computed_trust_verdict(&raw.work, &raw.evidence);
        Ok(WorkspaceWorkDetailRouteResponse {
            work: route_work_record(&raw.work, Some(trust), Some(summary_freshness)),
            links: raw.links.iter().map(route_work_link).collect(),
            evidence: raw.evidence.iter().map(route_work_evidence).collect(),
            summaries: raw
                .summaries
                .iter()
                .map(|summary| route_work_summary(summary, &material_key, REPORT_TEXT_LIMIT))
                .collect(),
            summary_claims: raw
                .summary_claims
                .iter()
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
        build_report(&store, workspace_id, WorkRecordId::from_id(work_id)).await
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

async fn build_report(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    work_id: WorkRecordId,
) -> Result<WorkspaceWorkReportRouteResponse, WorkspaceRouteError> {
    let raw = load_work_detail(store, workspace_id, work_id).await?;
    let material_key = material_revision_key(
        &raw.work,
        &raw.links,
        &raw.events,
        &raw.evidence,
        &raw.change_sets,
        &raw.contributions,
    );
    let summary_freshness = aggregate_summary_freshness(&raw.summaries, &material_key);
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
        change_sets: raw.change_sets,
        contributions: raw.contributions,
        summaries: raw
            .summaries
            .iter()
            .map(|summary| route_work_summary(summary, &material_key, REPORT_TEXT_LIMIT))
            .collect(),
        summary_claims: raw
            .summary_claims
            .iter()
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
        raw_transcript_available: false,
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
        WorkTrustVerdict::Verified => "Fresh evidence is present for this Work record.",
        WorkTrustVerdict::Stale => "Some evidence no longer matches the current Work fingerprint.",
        WorkTrustVerdict::MissingEvidence => "No evidence has been recorded for this Work record.",
        WorkTrustVerdict::Partial => "Some evidence or source material is incomplete.",
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
        WorkTrustVerdict::Partial => "Add missing fingerprints, artifacts, or citations.",
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
    let value = json!({
        "work": {
            "work_id": work.work_id,
            "updated_at": work.updated_at,
            "lifecycle": work.lifecycle,
            "head_commit": work.head_commit,
        },
        "links": links,
        "events": events,
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
        .filter_map(|link| link.target_json.as_ref().map(redact_route_value))
        .collect()
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
    let redacted = ctx_core::redaction::redact_sensitive(value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::ids::{WorkEventId, WorkRecordId, WorkspaceId};
    use ctx_core::models::{
        RecordFidelity, RecordSource, RecordTrust, WorkActorKind, WorkEventType, WorkRedactionClass,
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
}
