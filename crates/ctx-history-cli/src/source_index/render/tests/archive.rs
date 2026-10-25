use super::*;

#[test]
fn archive_declarations_are_visible_in_plain_styled_and_narrow_search() {
    let mut value = compact_search_value();
    value["results"][0]["archive_provenance"] = json!({
        "origin":"café laptop", "view":"work\u{001b}[31m notes",
        "label_authority":"archive_creator"
    });
    let original = value.clone();
    for width in [32, 48, 80, 120] {
        let plain = context(width, ColorMode::Never);
        let styled = context(width, ColorMode::Always);
        let document = render_search_document(&value, false, &plain);
        assert_fits(&document, &plain);
        let text = document.render_plain();
        assert!(text.contains("café laptop"));
        assert!(text.contains("Creator-supplied"));
        assert!(!text.contains('\u{001b}'));
        assert_eq!(
            strip_ansi(&render_search_document(&value, false, &styled).render(&styled)),
            text
        );
    }
    assert_eq!(value, original);
}
