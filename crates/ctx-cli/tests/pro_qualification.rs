#![cfg(unix)]

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

mod support;
use support::{initialize_current_query_store, write_blame_helper};

const PATH_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_PATH";
const SHA256_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_SHA256";
const CHANNEL_ENV: &str = "CTX_PRO_QUALIFICATION_HELPER_CHANNEL";

fn digest(path: &std::path::Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn qualification_command(
    root: &std::path::Path,
    helper: &std::path::Path,
    helper_digest: &str,
    helper_channel: &str,
) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    command
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_PRO_CHANNEL", "stable")
        .env(PATH_ENV, helper)
        .env(SHA256_ENV, helper_digest)
        .env(CHANNEL_ENV, helper_channel)
        .args([
            "--data-root",
            root.to_str().unwrap(),
            "blame",
            "commit",
            "0123456789abcdef",
            "--json",
        ]);
    command
}

#[test]
fn exact_local_helper_is_selected_by_the_qualification_binary() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-qualification");
    write_blame_helper(&helper);

    qualification_command(root.path(), &helper, &digest(&helper), "stable")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"commit\""));
}

#[test]
fn qualification_configuration_is_atomic_and_channel_bound() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-qualification");
    write_blame_helper(&helper);
    let helper_digest = digest(&helper);

    qualification_command(root.path(), &helper, &helper_digest, "staging")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "qualification helper channel does not match",
        ));

    let mut partial = Command::cargo_bin("ctx").unwrap();
    partial
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_PRO_CHANNEL", "stable")
        .env(PATH_ENV, &helper)
        .env(CHANNEL_ENV, "stable")
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "blame",
            "commit",
            "0123456789abcdef",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be configured together"));
}

#[test]
fn helper_tampering_is_rejected_before_execution() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-qualification");
    write_blame_helper(&helper);
    let expected_digest = digest(&helper);
    fs::write(&helper, b"#!/bin/sh\nexit 91\n").unwrap();

    qualification_command(root.path(), &helper, &expected_digest, "stable")
        .assert()
        .failure()
        .stderr(predicate::str::contains("SHA-256 does not match"));
}
