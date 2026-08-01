use clap::{error::ErrorKind, Parser as _, ValueEnum as _};
use sha2::{Digest as _, Sha256};

use super::super::IndexWatchOutput;
use super::*;
use crate::{
    commands::index_dashboard::IndexDashboard,
    ui::{ColorMode, StreamKind, TestContext},
};

fn args(case: FixtureCase, columns: usize) -> IndexDashboardFixtureArgs {
    IndexDashboardFixtureArgs {
        case,
        columns,
        rows: FIXTURE_ROWS,
        clock: FIXTURE_CLOCK.to_owned(),
        random_seed: FIXTURE_RANDOM_SEED.to_owned(),
        color: ColorMode::Never,
    }
}

fn render(case: FixtureCase, columns: usize) -> String {
    let context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, columns).color(ColorMode::Never),
    );
    let mut dashboard = IndexDashboard::default();
    dashboard
        .render(&case.status().unwrap(), &context)
        .render_plain()
}

fn redraw(case: FixtureCase, columns: usize) -> Vec<u8> {
    let mut output = IndexWatchOutput::for_test(Vec::new(), true, columns);
    for status in case.status_sequence().unwrap() {
        output.dashboard = Default::default();
        output.print_human(&status).unwrap();
    }
    output.writer
}

#[test]
fn fixture_parser_has_the_exact_closed_case_and_geometry_roster() {
    let cases = FixtureCase::value_variants()
        .iter()
        .map(|case| case.to_possible_value().unwrap().get_name().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        cases,
        [
            "discovering",
            "active-progress",
            "finalizing",
            "ready",
            "ready-partial-warning",
            "terminal-failure",
            "stopped-daemon",
            "semantic-disabled",
            "semantic-progress",
            "semantic-ready",
            "semantic-failure",
        ]
    );

    for case in &cases {
        for columns in FIXTURE_COLUMNS {
            let parsed = IndexDashboardFixtureArgs::try_parse_from([
                COMMAND_NAME,
                "--case",
                case,
                "--columns",
                &columns.to_string(),
                "--rows",
                "24",
                "--clock",
                FIXTURE_CLOCK,
                "--random-seed",
                FIXTURE_RANDOM_SEED,
            ])
            .unwrap();
            assert_eq!(parsed.columns, *columns);
            assert_eq!(parsed.rows, FIXTURE_ROWS);
        }
    }
}

#[test]
fn fixture_parser_rejects_unknown_cases_and_non_roster_geometry() {
    let unknown = IndexDashboardFixtureArgs::try_parse_from([
        COMMAND_NAME,
        "--case",
        "unknown",
        "--columns",
        "80",
        "--rows",
        "24",
        "--clock",
        FIXTURE_CLOCK,
        "--random-seed",
        FIXTURE_RANDOM_SEED,
    ])
    .unwrap_err();
    assert_eq!(unknown.kind(), ErrorKind::InvalidValue);

    assert_eq!(
        parse_columns("48"),
        Err("unsupported fixture column count 48; expected 32 or 80".to_owned())
    );
    assert_eq!(
        parse_rows("25"),
        Err("unsupported fixture row count 25; expected 24".to_owned())
    );
}

#[test]
fn typed_cases_construct_the_expected_production_status_shapes() {
    for case in FixtureCase::value_variants() {
        let status = case.status().unwrap();
        assert!(status["lexical"]["status"].is_string());
        assert!(status["lexical"]["indexed_items"].is_u64());
        assert!(status["semantic"]["enabled"].is_boolean());
        assert!(status["semantic"]["coverage"]["embedded_items"].is_u64());
        assert!(status["daemon"]["running"].is_boolean());
        assert!(status["daemon"]["jobs"]["semantic_index"]["status"].is_string());
    }

    assert_eq!(FixtureCase::Discovering.status_sequence().unwrap().len(), 1);
    for case in FixtureCase::value_variants()
        .iter()
        .copied()
        .filter(|case| *case != FixtureCase::Discovering)
    {
        assert_eq!(case.status_sequence().unwrap().len(), 2);
    }
}

#[test]
fn fixture_parameters_and_machine_capabilities_fail_closed() {
    let mut wrong_clock = args(FixtureCase::Ready, 80);
    wrong_clock.clock = "2026-06-23T12:00:01Z".to_owned();
    assert_eq!(
        validate_parameters(&wrong_clock).unwrap_err().to_string(),
        "unsupported fixture clock \"2026-06-23T12:00:01Z\"; expected 2026-06-23T12:00:00Z"
    );

    let pipe = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    assert_eq!(
        validate_terminal(&pipe, 80, 24, None)
            .unwrap_err()
            .to_string(),
        "index dashboard fixture requires stdout to be a terminal"
    );
    let tty = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    assert_eq!(
        validate_terminal(&tty, 80, 24, Some((32, 24)))
            .unwrap_err()
            .to_string(),
        "index dashboard fixture detected inconsistent stdout terminal widths"
    );
    assert_eq!(
        validate_terminal(&tty, 80, 24, Some((80, 25)))
            .unwrap_err()
            .to_string(),
        "index dashboard fixture expected a 80x24 terminal, detected 80x25"
    );
}

#[test]
fn all_22_fixture_screens_and_redraws_match_the_exact_golden_digest() {
    let mut golden = Vec::new();
    for columns in FIXTURE_COLUMNS {
        for case in FixtureCase::value_variants() {
            let name = case.to_possible_value().unwrap();
            golden.extend_from_slice(name.get_name().as_bytes());
            golden.extend_from_slice(&(*columns as u64).to_le_bytes());
            let screen = render(*case, *columns);
            golden.extend_from_slice(&(screen.len() as u64).to_le_bytes());
            golden.extend_from_slice(screen.as_bytes());
            let redraw = redraw(*case, *columns);
            golden.extend_from_slice(&(redraw.len() as u64).to_le_bytes());
            golden.extend_from_slice(&redraw);

            if *case == FixtureCase::Discovering {
                assert!(!redraw.windows(4).any(|window| window == b"\x1b[2K"));
            } else {
                assert!(redraw.windows(4).any(|window| window == b"\x1b[2K"));
            }
        }
    }
    let digest = format!("{:x}", Sha256::digest(golden));
    assert_eq!(
        digest,
        "7cca48422da3ae42a7afb5cda74c456a69eb081d41b0a49aef6455cbe48838b2"
    );
}
