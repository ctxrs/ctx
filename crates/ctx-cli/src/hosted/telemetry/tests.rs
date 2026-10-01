use super::*;
use clap::Parser;

#[test]
fn finite_commands_have_one_closed_operation_and_server_run_uses_lifecycle() {
    use super::super::HostedOperation as Operation;
    for (arguments, expected) in [
        (
            vec!["ctx", "archive", "verify", "synthetic-archive"],
            Some(Operation::ArchiveVerify),
        ),
        (
            vec!["ctx", "server", "status"],
            Some(Operation::ServerStatus),
        ),
        (
            vec!["ctx", "server", "publications"],
            Some(Operation::ServerPublications),
        ),
        (
            vec!["ctx", "server", "collection", "create", "synthetic-name"],
            Some(Operation::ServerCollectionCreate),
        ),
        (
            vec!["ctx", "server", "user", "list"],
            Some(Operation::ServerUserList),
        ),
        (
            vec!["ctx", "remote", "pause", "synthetic-name"],
            Some(Operation::RemotePause),
        ),
        (
            vec!["ctx", "remote", "pause", "synthetic-name", "--resume"],
            Some(Operation::RemoteResume),
        ),
        (
            vec!["ctx", "remote", "status", "synthetic-name"],
            Some(Operation::RemoteStatus),
        ),
        (
            vec!["ctx", "remote", "remove", "synthetic-name"],
            Some(Operation::RemoteRemove),
        ),
        (vec!["ctx", "server", "run"], None),
    ] {
        let cli = crate::Cli::try_parse_from(arguments).unwrap();
        let command = match cli.command {
            crate::cli::CommandRoot::Hosted(command) => command,
            _ => panic!("hosted command expected"),
        };
        assert_eq!(operation(&command), expected);
    }
}

#[test]
fn failure_classification_uses_types_never_error_text() {
    let secret = "private-token-example https://private.example/alice /private/history";
    assert_eq!(
        classify(&anyhow::anyhow!("{secret}")),
        Failure::Operation(FailureType::Other)
    );
    assert_eq!(
        classify(&anyhow::Error::new(ctx_history_sharing::Error::Forbidden).context(secret)),
        Failure::Operation(FailureType::Forbidden)
    );
    assert_eq!(
        classify(&anyhow::Error::new(std::io::Error::other(secret))),
        Failure::Operation(FailureType::Io)
    );
    assert_eq!(
        classify(&anyhow::anyhow!("{secret}").context(OutputFailure)),
        Failure::Output
    );
}

#[test]
fn optional_completion_replaces_legacy_terminal_without_initializing_identity() {
    let root = tempfile::tempdir().unwrap();
    let cli = crate::Cli::try_parse_from(["ctx", "server", "status"]).unwrap();
    let crate::cli::CommandRoot::Hosted(command) = cli.command else {
        panic!("hosted command")
    };
    let data = root.path().join("synthetic-data");
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = events.clone();
    let observers = super::super::HostedObservers {
        completion: Some(std::sync::Arc::new(move |event| {
            capture.lock().unwrap().push(event)
        })),
        ..Default::default()
    };
    assert!(super::super::run_with_observers(
        &command,
        Some(&data),
        crate::ui::ColorMode::Never,
        observers
    )
    .is_err());
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].operation,
        super::super::HostedOperation::ServerStatus
    );
    assert!(events[0].result.is_err());
    assert!(!data.exists());
}
