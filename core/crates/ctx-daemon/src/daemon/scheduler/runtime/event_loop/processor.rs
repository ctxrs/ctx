mod effects;

use super::super::super::persistence::append_session_event_with_retry;
use super::failure::fail_turn;
use super::provider_events::{
    claim_init_provider_session_ref, enrich_done_payload, record_first_provider_event_metric,
};
use super::state::{
    should_check_store_terminal_status, should_drop_post_terminal_event,
    should_process_post_terminal_assistant_complete, EventLoopRuntimeState,
};
use super::tools::prepare_tool_event_payload;
use super::TurnEventLoop;
use ctx_core::models::SessionEventType;
use ctx_providers::events::NormalizedEvent;
use ctx_session_tools::{normalize_tool_event, order_seq::attach_order_seq};

use self::effects::handle_persisted_provider_event_effects;

pub(super) enum ProviderEventProcessingOutcome {
    Continue,
    Stop,
}

pub(super) async fn process_provider_event(
    ctx: &mut TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
    ev: NormalizedEvent,
) -> ProviderEventProcessingOutcome {
    let Some(host) = ctx.host() else {
        return ProviderEventProcessingOutcome::Stop;
    };
    let event_type = ev.event_type.clone();
    let raw_payload = ev.payload_json.clone();
    let mut payload = raw_payload.clone();

    if runtime.mark_first_event_seen() {
        record_first_provider_event_metric(ctx, host.as_ref()).await;
    }

    if matches!(&ev.event_type, SessionEventType::Init) {
        if let Some(failure) =
            claim_init_provider_session_ref(ctx, host.as_ref(), &mut payload).await
        {
            fail_turn(ctx, runtime, failure).await;
            return ProviderEventProcessingOutcome::Continue;
        }
    }

    if matches!(&ev.event_type, SessionEventType::Done) {
        enrich_done_payload(ctx, &mut payload);
    }

    let dropped_by_store_terminal_status = should_check_store_terminal_status(&event_type)
        && should_drop_post_terminal_event(ctx, runtime).await
        && !should_process_post_terminal_assistant_complete(
            &event_type,
            runtime.terminal_status.as_ref(),
        );
    let allow_post_terminal_assistant_complete = should_process_post_terminal_assistant_complete(
        &event_type,
        runtime.terminal_status.as_ref(),
    );
    if (runtime.terminal_status.is_some() && !allow_post_terminal_assistant_complete)
        || dropped_by_store_terminal_status
    {
        tracing::debug!(
            session_id = %ctx.session_id.0,
            run_id = %ctx.run_id.0,
            turn_id = %ctx.turn_id.0,
            event_type = ?event_type,
            "dropping provider event after turn terminalization"
        );
        return ProviderEventProcessingOutcome::Continue;
    }

    let normalized_tool_event = if matches!(
        &event_type,
        SessionEventType::ToolCall
            | SessionEventType::ToolCallUpdate
            | SessionEventType::ToolResult
    ) {
        Some(normalize_tool_event(&event_type, &raw_payload))
    } else {
        None
    };

    if let Some(tool_event) = normalized_tool_event.as_ref() {
        payload = prepare_tool_event_payload(ctx, host.as_ref(), &event_type, tool_event).await;
    }

    {
        let mut order_seq_state = ctx.order_seq_state.lock().await;
        attach_order_seq(
            &mut order_seq_state,
            &event_type,
            &mut payload,
            Some(&ctx.turn_id),
            runtime.assistant_sequence,
        );
    }

    let event = match append_session_event_with_retry(
        &ctx.store,
        ctx.session_id,
        Some(ctx.run_id),
        Some(ctx.turn_id),
        event_type.clone(),
        payload,
    )
    .await
    {
        Ok(event) => event,
        Err(err) => {
            tracing::warn!(
                session_id = %ctx.session_id.0,
                run_id = %ctx.run_id.0,
                turn_id = %ctx.turn_id.0,
                event_type = ?event_type,
                "failed to append session event: {err:#}"
            );
            return ProviderEventProcessingOutcome::Continue;
        }
    };

    handle_persisted_provider_event_effects(
        ctx,
        &host,
        runtime,
        event,
        raw_payload,
        normalized_tool_event.as_ref(),
    )
    .await;

    ProviderEventProcessingOutcome::Continue
}
