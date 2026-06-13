use super::processor::{process_provider_event, ProviderEventProcessingOutcome};
use super::state::EventLoopRuntimeState;
use super::TurnEventLoop;

pub(super) async fn run_turn_event_loop(mut ctx: TurnEventLoop) {
    let mut runtime = EventLoopRuntimeState::default();

    while let Some(ev) = ctx.ev_rx.recv().await {
        if matches!(
            process_provider_event(&mut ctx, &mut runtime, ev).await,
            ProviderEventProcessingOutcome::Stop
        ) {
            break;
        }
    }
    let _ = ctx.events_done_tx.send(());
}
