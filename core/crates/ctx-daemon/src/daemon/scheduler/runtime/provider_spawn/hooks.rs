use std::sync::Arc;

use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_providers::adapters::{ProviderRunHooks, ProviderSessionRefClaimHook};
use ctx_store::Store;

use crate::daemon::scheduler::host::ProviderTurnLaunchHost;

pub(super) fn build_provider_run_hooks(
    provider_launch: &ProviderTurnLaunchHost,
    store: &Store,
    session: &Session,
    execution_environment: ExecutionEnvironment,
    session_root_kind: &str,
) -> ProviderRunHooks {
    let claim_store = store.clone();
    let claim_session_id = session.id;
    let provider_session_ref_claim: ProviderSessionRefClaimHook = Arc::new(move |claim| {
        let claim_store = claim_store.clone();
        Box::pin(async move {
            if let Some(returned_ref) = claim.returned_provider_session_ref {
                claim_store
                    .claim_session_provider_session_ref(
                        claim_session_id,
                        returned_ref,
                        "provider.session_opened",
                    )
                    .await?;
            }
            Ok(())
        })
    });
    let provider_unknown_event = provider_launch.provider_unknown_event_hook(
        session,
        execution_environment,
        session_root_kind,
    );
    ProviderRunHooks {
        provider_session_ref_claim: Some(provider_session_ref_claim),
        provider_unknown_event: Some(provider_unknown_event),
    }
}
