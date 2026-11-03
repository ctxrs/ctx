use serde_json::Value;

use crate::ui::{fields, section, Document, Field, RenderContext};

pub(super) fn append_storage(context: &RenderContext, document: &mut Document, report: &Value) {
    let Some(storage) = report.pointer("/lexical/storage") else {
        if report.pointer("/lexical/storage_error").is_some() {
            document.push_blank();
            document.append(section(
                "Active index storage",
                fields(context, &[Field::new("Size", "unavailable")]),
            ));
        }
        return;
    };
    let values = [
        ("Total", "total_bytes"),
        ("Docstore (Core)", "docstore_bytes"),
        ("Terms", "terms_bytes"),
        ("Postings", "postings_bytes"),
        ("Positions", "positions_bytes"),
        ("Fast fields", "fast_fields_bytes"),
        ("Other", "other_bytes"),
    ]
    .into_iter()
    .filter_map(|(label, key)| {
        storage[key]
            .as_u64()
            .map(|bytes| (label, ctx_history_cli::format_bytes(bytes)))
    })
    .collect::<Vec<_>>();
    let storage_fields = values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section(
        "Active index storage",
        fields(context, &storage_fields),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_support::{assert_fits, strip_ansi},
        ui::{ColorMode, StreamKind, TestContext},
    };
    use serde_json::json;

    #[test]
    fn human_storage_shows_every_component_with_units_and_fits_the_terminal() {
        let report = json!({"lexical": {"storage": {
            "total_bytes": 7340032,
            "docstore_bytes": 2097152,
            "terms_bytes": 1048576,
            "postings_bytes": 1048576,
            "positions_bytes": 1048576,
            "fast_fields_bytes": 1048576,
            "other_bytes": 1048576,
        }}});
        for width in [32, 48, 80, 120] {
            let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width));
            let usage = ctx_client_observability::local_usage::UsageReport {
                schema_version: 3,
                local_only: true,
                read_only: true,
                enabled: false,
                state: "disabled",
                retention_days: 400,
                definition_version: 3,
                definitions: None,
                estimates: None,
                error: None,
            };
            let document = crate::commands::render_status_human(
                &context,
                &report,
                None,
                std::path::Path::new("/tmp/ctx"),
                std::path::Path::new("/tmp/ctx/config.toml"),
                &json!({}),
                &usage,
            );
            let plain = document.render_plain();
            let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");
            for expected in [
                "Active index storage",
                "Total 7.0 MiB",
                "Docstore (Core) 2.0 MiB",
                "Terms 1.0 MiB",
                "Postings 1.0 MiB",
                "Positions 1.0 MiB",
                "Fast fields 1.0 MiB",
                "Other 1.0 MiB",
            ] {
                assert!(normalized.contains(expected), "{plain}");
            }
            assert_fits(&document, &context);
            let styled = RenderContext::for_test(
                TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Always),
            );
            assert_eq!(strip_ansi(&document.render(&styled)), plain);
        }
    }

    #[test]
    fn missing_generation_omits_storage_and_measurement_failure_is_not_zero() {
        let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
        let mut document = Document::default();
        append_storage(&context, &mut document, &json!({"lexical": {}}));
        assert!(document.render_plain().is_empty());
        append_storage(
            &context,
            &mut document,
            &json!({
                "lexical": {"storage_error": "size_unavailable"}
            }),
        );
        let plain = document.render_plain();
        assert!(plain.contains("unavailable"));
        assert!(!plain.contains("0 B"));
    }
}
