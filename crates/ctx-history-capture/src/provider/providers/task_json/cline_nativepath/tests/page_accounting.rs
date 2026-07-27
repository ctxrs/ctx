use super::*;

#[test]
fn accounting_admits_exact_output_boundary_and_counts_four_kib_identifiers() {
    let identifier = "i".repeat(4 * 1024);
    let fixture = Fixture::new(
        json!([{
            "id": "tool-call",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "tool_use_id": identifier.clone(),
                "name": identifier
            }]
        }]),
        json!([{
            "id": "exact-output",
            "type": "command_output",
            "text": "",
            "exitCode": 0
        }]),
    );
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let call_page = component_pages(&baseline.pages, ClineComponent::ApiHistory)
        .pop()
        .expect("tool-call page");
    let call = call_page
        .core
        .events
        .iter()
        .find_map(|event| event.tool_call.as_ref().map(|call| (event, call)))
        .expect("retained 4 KiB tool call");
    assert_eq!(call.1.call_id.as_deref().map(str::len), Some(4 * 1024));
    assert_eq!(call.1.name.as_deref().map(str::len), Some(4 * 1024));
    assert!(
        super::normalize::estimated_event_bytes(call.0)
            >= call.1.call_id.as_deref().unwrap().len() + call.1.name.as_deref().unwrap().len()
    );

    let empty_output = component_pages(&baseline.pages, ClineComponent::UiMessages)
        .pop()
        .expect("empty output page")
        .transient
        .as_ref()
        .expect("transient output")
        .observations
        .first()
        .expect("empty output observation");
    let empty_encoded = super::normalize::estimated_output_bytes(empty_output);
    let exact_content_bytes = super::normalize::CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES
        .checked_sub(16)
        .expect("transient payload wrapper fits lane")
        .checked_sub(empty_encoded)
        .expect("output envelope fits transient lane");
    write_json(
        &fixture.ui,
        &json!([{
            "id": "exact-output",
            "type": "command_output",
            "text": "e".repeat(exact_content_bytes),
            "exitCode": 0
        }]),
    );
    let exact = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let exact_page = component_pages(&exact.pages, ClineComponent::UiMessages)
        .pop()
        .expect("exact output page");
    let exact_output = exact_page
        .transient
        .as_ref()
        .expect("exact transient lane")
        .observations
        .first()
        .expect("exact-boundary output");
    assert_eq!(
        super::normalize::estimated_output_bytes(exact_output),
        super::normalize::CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES - 16
    );
    assert_eq!(
        exact_page.accounting.transient_output_bytes,
        super::normalize::CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES
    );
    assert!(exact_page.accounting.conservative_serialized_bytes <= CLINE_NATIVE_PAGE_MAX_BYTES);

    write_json(
        &fixture.ui,
        &json!([{
            "id": "exact-output",
            "type": "command_output",
            "text": "e".repeat(exact_content_bytes + 1),
            "exitCode": 0
        }]),
    );
    let over = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let over_page = component_pages(&over.pages, ClineComponent::UiMessages)
        .pop()
        .expect("over-boundary page");
    let transient = over_page.transient.as_ref().expect("over transient lane");
    assert!(transient.observations.is_empty());
    assert_eq!(transient.rejected_outputs.len(), 1);
}

#[test]
fn final_owned_page_bounds_accept_exact_limits_and_reject_plus_one() {
    let core_limit = super::normalize::CLINE_NATIVE_CORE_PAGE_MAX_BYTES;
    let total_limit = super::normalize::CLINE_NATIVE_PAGE_MAX_BYTES;
    let transient_at_total = total_limit - core_limit;
    assert!(super::reader::owned_page_bounds_are_valid(
        core_limit,
        transient_at_total,
        CLINE_NATIVE_PAGE_MAX_UNITS,
    ));
    assert!(!super::reader::owned_page_bounds_are_valid(
        core_limit + 1,
        0,
        CLINE_NATIVE_PAGE_MAX_UNITS,
    ));
    assert!(!super::reader::owned_page_bounds_are_valid(
        core_limit,
        transient_at_total + 1,
        CLINE_NATIVE_PAGE_MAX_UNITS,
    ));
    assert!(!super::reader::owned_page_bounds_are_valid(
        0,
        0,
        CLINE_NATIVE_PAGE_MAX_UNITS + 1,
    ));
}

#[test]
fn page_accounting_reserves_source_session_route_and_cursor_overhead() {
    let fixture = Fixture::new(
        json!([{"id": "one", "role": "user", "content": "one"}]),
        json!([{
            "id": "command",
            "type": "command_output",
            "text": "failure",
            "exitCode": 2
        }]),
    );
    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let metadata = component_pages(&result.pages, ClineComponent::TaskMetadata)
        .pop()
        .expect("metadata page");
    assert_eq!(
        metadata.accounting.core_units,
        super::normalize::CLINE_NATIVE_FIXED_PAGE_UNITS
            + super::normalize::CLINE_NATIVE_SESSION_PAGE_UNITS
    );
    let api = component_pages(&result.pages, ClineComponent::ApiHistory)
        .pop()
        .expect("API page");
    assert_eq!(
        api.accounting.core_units,
        super::normalize::CLINE_NATIVE_FIXED_PAGE_UNITS + 1
    );
    let command = component_pages(&result.pages, ClineComponent::UiMessages)
        .pop()
        .expect("command page");
    assert_eq!(
        command.accounting.core_units,
        super::normalize::CLINE_NATIVE_FIXED_PAGE_UNITS + 2
    );
    assert_eq!(command.accounting.potential_output_units, 1);
    assert_eq!(
        command.accounting.logical_units,
        command.accounting.core_units + command.accounting.potential_output_units
    );
    assert!(result
        .pages
        .iter()
        .all(|page| page.accounting.logical_units <= CLINE_NATIVE_PAGE_MAX_UNITS));
}

#[test]
fn source_overhead_and_item_mutations_admit_exactly_sixty_four_units() {
    let patch_with_files = |count: usize| {
        (0..count)
            .map(|index| {
                format!(
                    "*** Begin Patch\n*** Add File: src/file-{index}.rs\n+{index}\n*** End Patch"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let fixture = Fixture::new(
        json!([{
            "id": "boundary-tool",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "tool_use_id": "boundary-call",
                "name": "apply_patch",
                "input": {"patch": patch_with_files(59)}
            }]
        }]),
        json!([]),
    );
    let exact = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let exact_page = component_pages(&exact.pages, ClineComponent::ApiHistory)
        .pop()
        .expect("exact 64-unit page");
    assert_eq!(exact_page.core.events.len(), 1);
    assert_eq!(exact_page.core.events[0].file_touches.len(), 59);
    assert_eq!(
        exact_page.accounting.logical_units,
        CLINE_NATIVE_PAGE_MAX_UNITS
    );

    write_json(
        &fixture.api,
        &json!([{
            "id": "boundary-tool",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "tool_use_id": "boundary-call",
                "name": "apply_patch",
                "input": {"patch": patch_with_files(60)}
            }]
        }]),
    );
    let over = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let over_page = component_pages(&over.pages, ClineComponent::ApiHistory)
        .pop()
        .expect("over-bound rejection page");
    assert!(over_page.core.events.is_empty());
    assert_eq!(over_page.core.rejections.len(), 1);
    assert_eq!(
        over_page.accounting.logical_units,
        super::normalize::CLINE_NATIVE_FIXED_PAGE_UNITS
    );
}
