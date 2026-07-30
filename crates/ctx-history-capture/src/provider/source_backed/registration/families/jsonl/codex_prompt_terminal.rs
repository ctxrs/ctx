use super::*;

pub(super) type CodexPromptTerminalCapture = dyn Fn(&CertifiedSource) -> Result<CertifiedSourceInventory, SourceBackedRouteError>
    + Send
    + Sync;

pub(super) struct CodexPromptTerminalEvidence {
    pub(super) certificate: CertifiedSource,
    pub(super) inventory: CertifiedSourceInventory,
}

pub(super) fn remember_codex_prompt_terminal(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    certificate: CertifiedSource,
    inventory: CertifiedSourceInventory,
) -> SourceBackedRouteResult<()> {
    let mut state = state.lock().map_err(|_| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Codex prompt-history terminal evidence lock was poisoned",
        )
    })?;
    *state = Some(CodexPromptTerminalEvidence {
        certificate,
        inventory,
    });
    Ok(())
}

pub(super) fn bind_codex_prompt_target(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(expected) = state.as_ref() else {
        return false;
    };
    match target {
        SourceBackedRevalidationTarget::Source(source) => expected.certificate == *source,
        SourceBackedRevalidationTarget::Deletion(deletion) => {
            deletion.verifies(&expected.inventory)
                && !expected
                    .certificate
                    .observation()
                    .source()
                    .exact_descriptor_eq(deletion.source())
        }
    }
}

pub(super) fn revalidate_codex_prompt_inventory(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    capture: &CodexPromptTerminalCapture,
    expected_inventory: &CertifiedSourceInventory,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(expected) = state.as_ref() else {
        return false;
    };
    if expected.inventory != *expected_inventory {
        return false;
    }
    capture(&expected.certificate).is_ok_and(|inventory| inventory == expected.inventory)
}
