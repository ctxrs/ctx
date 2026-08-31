use super::super::search::{
    render_semantic_fallback_warning, semantic_error_into_anyhow, HistorySemanticError,
    SemanticFallbackDiagnostics, SemanticReason,
};

#[test]
fn semantic_fallback_warning_is_structured_and_actionable_without_backend_codes() {
    let fallback = SemanticFallbackDiagnostics {
        code: Some("semantic_disabled"),
        reason: Some(ctx_history_read_application::SemanticReason::PolicyDisabled),
        detail: "backend_error at /private/model/cache".to_owned(),
        retryable: false,
    };
    for width in [40, 80, 120] {
        let context = crate::ui::RenderContext::for_test(
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, width)
                .color(crate::ui::ColorMode::Always),
        );
        let rendered = render_semantic_fallback_warning(&context, &fallback);
        let plain = rendered.render_plain();
        let styled = rendered.render(&context);
        assert!(plain.contains("Semantic search is unavailable"));
        assert!(plain.contains("Keyword search was used"));
        assert!(plain.contains("ctx semantic enable"));
        assert!(!plain.contains("semantic_disabled"));
        assert!(!plain.contains("backend_error"));
        assert!(!plain.contains("/private/model/cache"));
        assert!(styled.contains("\u{1b}["));
        assert!(plain.lines().all(|line| line.chars().count() <= width));
    }
}

#[test]
fn semantic_only_mapping_preserves_code_reason_and_retryability() {
    let error = semantic_error_into_anyhow(HistorySemanticError::not_ready(
        SemanticReason::ExecutorUnavailable,
        "passive cached backend is unavailable",
        true,
    ));
    let typed = error
        .downcast_ref::<crate::semantic::SemanticNotReady>()
        .expect("semantic-only mapping must retain the typed CLI error");
    assert_eq!(typed.code(), "semantic_executor_unavailable");
    assert_eq!(typed.detail(), "passive cached backend is unavailable");
    assert!(typed.retryable());
}
