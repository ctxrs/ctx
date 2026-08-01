use super::*;

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(
        crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, width)
            .color(crate::ui::ColorMode::Never),
    )
}

#[test]
fn audited_pro_failures_use_only_trusted_human_copy_and_recovery() {
    for (raw, title, detail, action) in [
        (
            "authentication_denied: WorkOS sign-in was denied at /private/auth",
            "ctx Pro sign-in was denied",
            "No sign-in session was accepted",
            Some("ctx pro"),
        ),
        (
            "pro_not_installed: no Pro helper at /private/helper",
            "ctx Pro is not set up",
            "signed Pro helper is not installed",
            Some("ctx pro"),
        ),
        (
            "entitlement_expired: private entitlement detail at /private/grant",
            "ctx Pro is locked",
            "Local Pro data is preserved",
            Some("ctx pro manage"),
        ),
        (
            "key_store_unavailable: selected store failed at /private/vault",
            "The secure key store is unavailable",
            "Repair the selected persistent key store",
            Some("ctx pro"),
        ),
        (
            "key_store_unavailable: interrupted Pro deletion must be completed with `ctx pro uninstall --delete-data`; /private/cleanup",
            "A previous ctx Pro deletion is incomplete",
            "secure local deletion is completed",
            Some("ctx pro uninstall --delete-data"),
        ),
        (
            "cancelled: uninstall confirmation was not provided at /private/prompt",
            "ctx Pro uninstall was cancelled",
            "local Pro data were left unchanged",
            Some("ctx pro uninstall"),
        ),
        (
            "invalid_request: qualification helpers are unsupported on this platform at /private/target",
            "ctx Pro is not available on this platform",
            "No compatible signed helper is published",
            None,
        ),
        (
            "helper_upgrade_required: incompatible helper at /private/helper",
            "The ctx Pro helper needs repair",
            "compatible signed helper",
            Some("ctx pro"),
        ),
        (
            "invalid_response: malformed helper frame at /private/helper",
            "ctx Pro returned an invalid response",
            "No untrusted service or helper result was accepted",
            Some("ctx pro"),
        ),
    ] {
        let error = anyhow::anyhow!(raw).context("outer failure at /private/context");
        let rendered = human_actionable_error_document(&context(80), &error, "ctx pro")
            .unwrap_or_else(|| panic!("missing trusted human presentation for {raw}"))
            .render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        let code = raw.split(':').next().unwrap();
        let headline = rendered.lines().next().unwrap_or_default();

        assert!(headline.starts_with(&format!("✗ {title}")), "{raw}: {rendered}");
        assert!(normalized.contains(detail), "{raw}: {rendered}");
        assert!(!headline.starts_with(&format!("✗ {code}:")), "{raw}: {rendered}");
        assert!(!rendered.contains("/private"), "{raw}: {rendered}");
        assert!(!rendered.contains(raw), "{raw}: {rendered}");
        match action {
            Some(action) => assert!(
                rendered.contains(&format!("Next\n  {action}\n")),
                "{raw}: {rendered}"
            ),
            None => assert!(!rendered.contains("\nNext\n"), "{raw}: {rendered}"),
        }
    }
}
