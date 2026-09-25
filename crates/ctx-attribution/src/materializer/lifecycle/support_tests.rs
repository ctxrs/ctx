use super::*;

#[cfg(test)]
#[test]
fn prepared_core_bound_keeps_its_reason() {
    let error = protocol_error(crate::protocol::ProtocolError::new(
        ErrorClass::Bounds,
        "Core prepared unit exceeds its worst-case byte credit (source core_source_abc)",
    ));
    assert_eq!(
        error.to_string(),
        "segment materializer bound exceeded: Core prepared unit exceeds its worst-case byte credit (source core_source_abc)"
    );
}
