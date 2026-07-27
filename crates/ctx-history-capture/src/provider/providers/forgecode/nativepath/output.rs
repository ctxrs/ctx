use crate::{
    provider::native_ingestion::{
        process_pro_replay_only, NativePageAccounting, NativeProOutputPage, NativeProReplayPage,
        NativeSafeFrontier, NativeSourceIdentity,
    },
    CaptureError, OutputNativeCursor, OutputSourceIdentity, ProOutputProgress, ProOutputSink,
    ProOutputSourceDisposition, Result,
};

use super::source::{
    frontier_bytes, ForgeCodeFrontier, ForgeCodePage, FORGECODE_NATIVE_FRONTIER_VERSION,
    FORGECODE_NATIVE_PARSER_REVISION, FORGECODE_NATIVE_POLICY_REVISION,
};

const FORGECODE_OUTPUT_ACCOUNTING_RESERVE_BYTES: usize = 128 * 1024;

pub(super) struct ForgeCodeOutputStart {
    pub(super) frontier: ForgeCodeFrontier,
    pub(super) terminal: bool,
}

pub(super) struct ForgeCodeOutputReplay<'a> {
    sink: &'a dyn ProOutputSink,
    source: OutputSourceIdentity,
    routing: NativeSourceIdentity,
    observed_revision: String,
    parser_revision: String,
    materializer_revision: String,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl<'a> ForgeCodeOutputReplay<'a> {
    pub(super) fn new(
        sink: &'a dyn ProOutputSink,
        machine_id: &str,
        source_identity: &str,
        observed_revision: &str,
    ) -> Result<(Self, ForgeCodeOutputStart)> {
        let source = OutputSourceIdentity {
            provider: "forgecode".to_owned(),
            namespace_id: machine_id.to_owned(),
            source_id: source_identity.to_owned(),
        };
        let routing = NativeSourceIdentity::new("forgecode", source_identity);
        let progress = sink
            .observe_source(&source)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let parser_revision = format!(
            "forgecode-nativepath:{FORGECODE_NATIVE_PARSER_REVISION}:{FORGECODE_NATIVE_POLICY_REVISION}"
        );
        let materializer_revision = sink.materializer_revision().to_owned();
        let plan = output_plan(
            progress,
            observed_revision,
            &parser_revision,
            &materializer_revision,
        )?;
        Ok((
            Self {
                sink,
                source,
                routing,
                observed_revision: observed_revision.to_owned(),
                parser_revision,
                materializer_revision,
                source_epoch: plan.source_epoch,
                expected_source_epoch: plan.expected_source_epoch,
                expected_sink_frontier: plan.expected_sink_frontier,
                disposition: plan.disposition,
            },
            ForgeCodeOutputStart {
                frontier: plan.scan_frontier,
                terminal: plan.scan_terminal,
            },
        ))
    }

    pub(super) fn materialize(&mut self, page: &mut ForgeCodePage) -> Result<()> {
        let expected_frontier = safe_frontier(&page.expected_frontier)?;
        let next_safe_frontier = safe_frontier(&page.next_frontier)?;
        let observations = std::mem::take(&mut page.outputs);
        let logical_units = observations.len().max(1);
        let conservative_serialized_bytes = page
            .retained_bytes
            .saturating_add(FORGECODE_OUTPUT_ACCOUNTING_RESERVE_BYTES);
        let output = NativeProOutputPage {
            inventory_generation: self.sink.inventory_generation(),
            source: self.source.clone(),
            source_epoch: self.source_epoch,
            observed_revision: self.observed_revision.clone(),
            parser_revision: self.parser_revision.clone(),
            materializer_revision: self.materializer_revision.clone(),
            disposition: self.disposition,
            expected_prior_source_epoch: self.expected_source_epoch,
            expected_prior_frontier: self.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            self.routing.clone(),
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units,
                conservative_serialized_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        process_pro_replay_only(replay, self.sink).map_err(|failure| {
            CaptureError::InvalidPayload(format!(
                "ForgeCode NativePath output lane failed after Core commit: {:?}",
                failure.output_error
            ))
        })?;
        self.expected_source_epoch = Some(self.source_epoch);
        self.expected_sink_frontier = Some(next_safe_frontier);
        self.disposition = ProOutputSourceDisposition::AppendOrResume;
        Ok(())
    }
}

struct OutputPlan {
    scan_frontier: ForgeCodeFrontier,
    scan_terminal: bool,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

fn output_plan(
    progress: Option<ProOutputProgress>,
    observed_revision: &str,
    parser_revision: &str,
    materializer_revision: &str,
) -> Result<OutputPlan> {
    let Some(progress) = progress else {
        return Ok(OutputPlan {
            scan_frontier: ForgeCodeFrontier::initial(),
            scan_terminal: false,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let prior_safe = progress
        .cursor
        .as_ref()
        .map(raw_safe_frontier)
        .transpose()?;
    let decoded = progress
        .cursor
        .as_ref()
        .map(decode_output_frontier)
        .transpose();
    let exact = progress.observed_revision == observed_revision
        && progress.parser_revision == parser_revision
        && progress.materializer_revision == materializer_revision
        && decoded.is_ok();
    if exact {
        return Ok(OutputPlan {
            scan_frontier: decoded?.unwrap_or_else(ForgeCodeFrontier::initial),
            scan_terminal: progress.terminal,
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_safe,
            disposition: ProOutputSourceDisposition::AppendOrResume,
        });
    }
    Ok(OutputPlan {
        scan_frontier: ForgeCodeFrontier::initial(),
        scan_terminal: false,
        source_epoch: progress.source_epoch.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("ForgeCode output source epoch overflowed".to_owned())
        })?,
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: prior_safe,
        disposition: ProOutputSourceDisposition::Rewrite,
    })
}

fn raw_safe_frontier(cursor: &OutputNativeCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(cursor.version, cursor.payload.clone())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn decode_output_frontier(cursor: &OutputNativeCursor) -> Result<ForgeCodeFrontier> {
    if cursor.version != FORGECODE_NATIVE_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode output cursor has an unsupported version".to_owned(),
        ));
    }
    serde_json::from_slice(&cursor.payload).map_err(CaptureError::from)
}

pub(super) fn safe_frontier(frontier: &ForgeCodeFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(FORGECODE_NATIVE_FRONTIER_VERSION, frontier_bytes(frontier)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}
