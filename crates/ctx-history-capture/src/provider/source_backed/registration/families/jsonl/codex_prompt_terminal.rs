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
) -> SourceBackedRouteResult<bool> {
    let state = state.lock().map_err(|_| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Codex prompt-history terminal evidence lock was poisoned",
        )
    })?;
    let Some(expected) = state.as_ref() else {
        return Ok(false);
    };
    Ok(match target {
        SourceBackedRevalidationTarget::Source(source) => expected.certificate == *source,
        SourceBackedRevalidationTarget::Deletion(deletion) => {
            deletion.verifies(&expected.inventory)
                && !expected
                    .certificate
                    .observation()
                    .source()
                    .exact_descriptor_eq(deletion.source())
        }
    })
}

pub(super) fn revalidate_codex_prompt_inventory(
    state: &Mutex<Option<CodexPromptTerminalEvidence>>,
    capture: &CodexPromptTerminalCapture,
    expected_inventory: &CertifiedSourceInventory,
) -> SourceBackedRouteResult<bool> {
    let state = state.lock().map_err(|_| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Codex prompt-history terminal evidence lock was poisoned",
        )
    })?;
    let Some(expected) = state.as_ref() else {
        return Ok(false);
    };
    if expected.inventory != *expected_inventory {
        return Ok(false);
    }
    match capture(&expected.certificate) {
        Ok(inventory) => Ok(inventory == expected.inventory),
        Err(error) if error.kind == SourceBackedRouteErrorKind::SourceChanged => Ok(false),
        Err(error) => Err(error),
    }
}
