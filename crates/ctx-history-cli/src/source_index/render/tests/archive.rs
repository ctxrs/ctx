use super::*;

#[test]
fn archive_declarations_escape_controls_on_every_human_surface_before_layout() {
    for control in ['\u{001b}', '\u{2028}', '\u{2029}']
        .into_iter()
        .chain('\u{202a}'..='\u{202e}')
        .chain('\u{2066}'..='\u{2069}')
    {
        let provenance = json!({
            "origin": format!("café laptop {control}"),
            "view": format!("作業 العربية {control}"),
            "label_authority": "archive_creator"
        });
        let escape = if control == '\u{001b}' {
            "\\x1b".to_owned()
        } else {
            format!("\\u{{{:04x}}}", u32::from(control))
        };
        let mut search = compact_search_value();
        search["results"][0]["archive_provenance"] = provenance.clone();
        let mut session = show_value();
        session["archive_provenance"] = provenance.clone();
        let mut event = event_show_value(None, SESSION_ID);
        event["event"]["archive_provenance"] = provenance.clone();
        event["events"][0]["archive_provenance"] = provenance.clone();
        let mut locate_event = locate_event_value();
        locate_event["archive_provenance"] = provenance.clone();
        let mut locate_session = locate_session_value();
        locate_session["archive_provenance"] = provenance;
        let values = [search, session, event, locate_event, locate_session];
        let original = values.clone();
        let render = |context: &RenderContext| {
            [
                render_search_document(&values[0], false, context),
                render_search_document(&values[0], true, context),
                render_show_document(&values[1], context),
                render_show_document(&values[2], context),
                render_locate_document(&values[3], context),
                render_locate_document(&values[4], context),
            ]
        };
        let assert_safe = |text: &str| {
            assert!(!text.contains(control), "{text:?}");
            assert!(text.matches(&escape).count() >= 2, "{text:?}");
            for preserved in ["café", "作業", "العربية"] {
                assert!(text.contains(preserved), "{text:?}");
            }
            assert!(text.to_lowercase().contains("creator-supplied"), "{text:?}");
        };
        for width in [32, 48, 80, 120] {
            let plain = context(width, ColorMode::Never);
            let styled = context(width, ColorMode::Always);
            for (document, styled_document) in render(&plain).iter().zip(render(&styled)) {
                assert_fits(document, &plain);
                let text = document.render_plain();
                assert_safe(&text);
                assert_eq!(strip_ansi(&styled_document.render(&styled)), text);
            }
        }
        for value in &values[1..=2] {
            assert_safe(&render_show_text(value));
            assert_safe(&render_show_markdown(value));
        }
        assert_eq!(values, original);
    }
}
