#[test]
fn named_julia_preserves_extensionless_script_path_and_locations() {
    let facts = super::parse_named(
        "scripts/run",
        "#!/usr/bin/env julia\nfunction helper()\n  1\nend\nhelper()\n",
        "hash",
        "julia",
    )
    .unwrap()
    .unwrap();
    assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
    assert_eq!(facts.path, "scripts/run");
    assert_eq!(
        facts
            .nodes
            .iter()
            .find(|n| n.label == "helper")
            .unwrap()
            .line,
        Some(2)
    );
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.relation == "calls" && r.label == "helper")
    );
    assert!(
        super::parse_named("scripts/run", "", "h", "unknown")
            .unwrap()
            .is_none()
    );
}
