use super::*;
use std::fs;

fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support selector fixtures")
}

#[test]
fn fixed_limits_match_the_reviewed_discovery_contract() {
    assert_eq!(MAX_SELECTOR_FILE_BYTES, 1024 * 1024);
    assert_eq!(MAX_SELECTOR_FILES_PER_PROVIDER, 64);
    assert_eq!(MAX_CONFIG_INCLUDE_DEPTH, 4);
    assert_eq!(MAX_CONFIG_INCLUDE_FILES, 16);
    assert_eq!(MAX_PARSED_NESTING_DEPTH, 32);
    assert_eq!(MAX_FINITE_SELECTOR_ENTRIES, 128);
    assert_eq!(MAX_DIRECT_DIRECTORY_ENTRIES, 1024);
    assert_eq!(MAX_PROJECT_ANCESTORS, 64);
    assert_eq!(MAX_SOURCE_CANDIDATES_PER_PROVIDER, 256);
    assert_eq!(
        ctx_history_capture_model::MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES,
        16 * 1024
    );
    assert_eq!(MAX_RENDERED_DIAGNOSTIC_BYTES, 512);
}

#[test]
fn structured_helpers_require_exact_scalar_and_list_shapes() {
    let document = SelectorDocument::Structured(serde_json::json!({
        "options": {"root": "/tmp/history", "profiles": ["one", "two"]}
    }));
    assert_eq!(document.string(&["options", "root"]), Some("/tmp/history"));
    assert_eq!(
        document.strings(&["options", "profiles"]),
        Some(vec!["one", "two"])
    );
    assert_eq!(document.string(&["root"]), None);
}

#[test]
fn bounded_reader_parses_each_allowlisted_selector_format() {
    let temp = tempdir();
    let fixtures = [
        (
            SelectorFormat::Json,
            "selector.json",
            r#"{"root":"json"}"#,
            "json",
        ),
        (
            SelectorFormat::Jsonc,
            "selector.jsonc",
            "{/* comment */\"root\":\"jsonc\"}",
            "jsonc",
        ),
        (
            SelectorFormat::Json5,
            "selector.json5",
            "{root: 'json5',}",
            "json5",
        ),
        (
            SelectorFormat::Toml,
            "selector.toml",
            "root = \"toml\"\n",
            "toml",
        ),
        (
            SelectorFormat::Yaml,
            "selector.yaml",
            "root: yaml\n",
            "yaml",
        ),
    ];
    for (format, name, body, expected) in fixtures {
        let path = temp.path().join(name);
        std::fs::write(&path, body).unwrap();
        let document = SelectorReader::default().read(&path, format).unwrap();
        assert_eq!(document.string(&["root"]), Some(expected));
    }

    let xml = temp.path().join("selector.xml");
    std::fs::write(
        &xml,
        r#"<application><component><option name="root" value="xml"/></component></application>"#,
    )
    .unwrap();
    let document = SelectorReader::default()
        .read(&xml, SelectorFormat::Xml)
        .unwrap();
    assert_eq!(
        document
            .xml()
            .unwrap()
            .values(&["application", "component", "option"], Some("value")),
        vec!["xml"]
    );
}

#[test]
fn bounded_reader_enforces_byte_file_and_depth_limits() {
    let temp = tempdir();
    let valid = temp.path().join("valid.json");
    std::fs::write(&valid, "{}").unwrap();
    let mut reader = SelectorReader::default();
    for _ in 0..MAX_SELECTOR_FILES_PER_PROVIDER {
        reader.read(&valid, SelectorFormat::Json).unwrap();
    }
    assert_eq!(reader.files_read(), MAX_SELECTOR_FILES_PER_PROVIDER);
    assert_eq!(
        reader.read(&valid, SelectorFormat::Json),
        Err(SelectorReadError::FileLimit)
    );

    let oversized = temp.path().join("oversized.json");
    std::fs::write(&oversized, vec![b' '; MAX_SELECTOR_FILE_BYTES + 1]).unwrap();
    assert_eq!(
        SelectorReader::default().read(&oversized, SelectorFormat::Json),
        Err(SelectorReadError::FileTooLarge)
    );

    let deep = temp.path().join("deep.json");
    let body = format!(
        "{}null{}",
        "[".repeat(MAX_PARSED_NESTING_DEPTH),
        "]".repeat(MAX_PARSED_NESTING_DEPTH)
    );
    std::fs::write(&deep, body).unwrap();
    assert_eq!(
        SelectorReader::default().read(&deep, SelectorFormat::Json),
        Err(SelectorReadError::NestingDepth)
    );

    let adversarial = temp.path().join("adversarial.json5");
    std::fs::write(
        &adversarial,
        format!("{}null{}", "[".repeat(4096), "]".repeat(4096)),
    )
    .unwrap();
    assert_eq!(
        SelectorReader::default().read(&adversarial, SelectorFormat::Json5),
        Err(SelectorReadError::NestingDepth)
    );
}

#[test]
fn json5_depth_scan_ignores_tokens_in_strings_and_comments() {
    let decoys = "[{}]".repeat(MAX_PARSED_NESTING_DEPTH + 1);
    let text =
        format!("{{literal: '{decoys}', block: /* {decoys} */ true, // {decoys}\n value: 1}}");
    assert_eq!(validate_json5_nesting(&text), Ok(()));
    assert_eq!(
        validate_json5_nesting(&format!(
            "// decoy\u{2028}{}null",
            "[".repeat(MAX_PARSED_NESTING_DEPTH + 1)
        )),
        Err(SelectorReadError::NestingDepth)
    );
}

#[test]
fn include_budget_enforces_both_reviewed_include_limits() {
    let mut budget = SelectorIncludeBudget::default();
    for _ in 0..MAX_CONFIG_INCLUDE_FILES {
        budget.admit(MAX_CONFIG_INCLUDE_DEPTH).unwrap();
    }
    assert_eq!(budget.files(), MAX_CONFIG_INCLUDE_FILES);
    assert_eq!(
        budget.admit(MAX_CONFIG_INCLUDE_DEPTH),
        Err(SelectorReadError::FileLimit)
    );
    assert_eq!(
        SelectorIncludeBudget::default().admit(MAX_CONFIG_INCLUDE_DEPTH + 1),
        Err(SelectorReadError::NestingDepth)
    );
}

#[cfg(unix)]
#[test]
fn bounded_reader_does_not_follow_selector_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let target = temp.path().join("target.json");
    let link = temp.path().join("selector.json");
    std::fs::write(&target, "{}").unwrap();
    symlink(target, &link).unwrap();
    assert_eq!(
        SelectorReader::default().read(&link, SelectorFormat::Json),
        Err(SelectorReadError::UnsupportedRoot)
    );
}

#[test]
fn root_handle_discovery_selector_swap_fails_final_revalidation() {
    let temp = tempdir();
    let selector = temp.path().join("selector.json");
    let moved = temp.path().join("opened-selector.json");
    let replacement = temp.path().join("replacement.json");
    fs::write(&selector, r#"{"root":"opened"}"#).unwrap();
    fs::write(&replacement, r#"{"root":"replacement"}"#).unwrap();

    let selector_for_hook = selector.clone();
    let moved_for_hook = moved.clone();
    SELECTOR_FILE_OPEN_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&selector_for_hook, &moved_for_hook).unwrap();
            fs::rename(&replacement, &selector_for_hook).unwrap();
        }));
    });

    assert_eq!(
        SelectorReader::default().read(&selector, SelectorFormat::Json),
        Err(SelectorReadError::UnsupportedRoot)
    );
    assert_eq!(fs::read_to_string(moved).unwrap(), r#"{"root":"opened"}"#);
}

#[test]
fn root_handle_discovery_directory_swap_fails_final_revalidation() {
    let temp = tempdir();
    let root = temp.path().join("root");
    let moved = temp.path().join("opened-root");
    let replacement = temp.path().join("replacement");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(root.join("opened.json"), "{}").unwrap();
    fs::write(replacement.join("replacement.json"), "{}").unwrap();

    let root_for_hook = root.clone();
    let moved_for_hook = moved.clone();
    DIRECT_ENTRIES_ROOT_OPEN_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&root_for_hook, &moved_for_hook).unwrap();
            fs::rename(&replacement, &root_for_hook).unwrap();
        }));
    });

    assert_eq!(
        direct_entries(&root),
        Err(SelectorReadError::UnsupportedRoot)
    );
    assert!(moved.join("opened.json").is_file());
}

#[test]
fn direct_entries_rejects_same_name_child_replacement_between_bounded_passes() {
    let temp = tempdir();
    let root = temp.path().join("root");
    let child = root.join("child");
    let moved = root.join("opened-child");
    let replacement = root.join("replacement");
    fs::create_dir_all(&child).unwrap();
    fs::create_dir_all(&replacement).unwrap();

    let child_for_hook = child.clone();
    let moved_for_hook = moved.clone();
    let replacement_for_hook = replacement.clone();
    DIRECT_ENTRIES_FIRST_PASS_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&child_for_hook, &moved_for_hook).unwrap();
            fs::rename(&replacement_for_hook, &child_for_hook).unwrap();
        }));
    });

    assert_eq!(direct_entries(&root), Err(SelectorReadError::Unavailable));
    assert!(moved.is_dir());
}

#[cfg(unix)]
#[test]
fn direct_entries_rejects_link_roots_and_children() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let outside = temp.path().join("outside");
    let entries = temp.path().join("entries");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&entries).unwrap();
    std::fs::write(outside.join("external.json"), "{}").unwrap();
    std::fs::write(entries.join("ordinary.json"), "{}").unwrap();
    symlink(&outside, entries.join("linked-child")).unwrap();
    let linked_root = temp.path().join("linked-root");
    symlink(&entries, &linked_root).unwrap();

    assert_eq!(
        direct_entries(&entries),
        Err(SelectorReadError::UnsupportedRoot)
    );
    assert_eq!(
        direct_entries(&linked_root),
        Err(SelectorReadError::UnsupportedRoot)
    );
}

#[cfg(unix)]
#[test]
fn direct_regular_file_filter_ignores_unmatched_links_but_rejects_matching_links() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let entries = temp.path().join("entries");
    let outside = temp.path().join("outside.service");
    std::fs::create_dir_all(&entries).unwrap();
    std::fs::write(&outside, "outside").unwrap();
    let official = entries.join("nanoclaw-v2-aaaaaaaa.service");
    std::fs::write(&official, "official").unwrap();
    symlink(&outside, entries.join("unrelated.service")).unwrap();

    let nanoclaw_name = |name: &OsStr| {
        name.to_str()
            .is_some_and(|name| name.starts_with("nanoclaw-v2-"))
    };
    assert_eq!(
        direct_regular_files_matching(&entries, nanoclaw_name),
        Ok(vec![official])
    );

    symlink(&outside, entries.join("nanoclaw-v2-bbbbbbbb.service")).unwrap();
    assert_eq!(
        direct_regular_files_matching(&entries, nanoclaw_name),
        Err(SelectorReadError::UnsupportedRoot)
    );
}

#[cfg(target_os = "windows")]
#[test]
fn direct_entries_rejects_windows_reparse_roots_and_children() {
    use std::{io::ErrorKind, os::windows::fs::symlink_dir};

    let temp = tempdir();
    let outside = temp.path().join("outside");
    let entries = temp.path().join("entries");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&entries).unwrap();
    std::fs::write(entries.join("ordinary.json"), "{}").unwrap();
    if let Err(error) = symlink_dir(&outside, entries.join("linked-child")) {
        if error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
            return;
        }
        panic!("failed to create Windows child reparse point: {error}");
    }
    let linked_root = temp.path().join("linked-root");
    symlink_dir(&entries, &linked_root)
        .unwrap_or_else(|error| panic!("failed to create Windows root reparse point: {error}"));

    assert_eq!(
        direct_entries(&entries),
        Err(SelectorReadError::UnsupportedRoot)
    );
    assert_eq!(
        direct_entries(&linked_root),
        Err(SelectorReadError::UnsupportedRoot)
    );
}

#[test]
fn xml_reader_rejects_document_types_and_entity_references() {
    let temp = tempdir();
    let xml = temp.path().join("selector.xml");
    std::fs::write(
        &xml,
        r#"<!DOCTYPE config [<!ENTITY external SYSTEM "file:///etc/passwd">]><config>&external;</config>"#,
    )
    .unwrap();
    assert_eq!(
        SelectorReader::default().read(&xml, SelectorFormat::Xml),
        Err(SelectorReadError::XmlDoctype)
    );
}

#[test]
fn xml_reader_unescapes_attribute_entity_paths() {
    let temp = tempdir();
    let xml = temp.path().join("selector.xml");
    std::fs::write(
        &xml,
        r#"<application><component><option value="/tmp/A&amp;B/&#x8DEF;&#24452;"/></component></application>"#,
    )
    .unwrap();
    let document = SelectorReader::default()
        .read(&xml, SelectorFormat::Xml)
        .unwrap();
    assert_eq!(
        document
            .xml()
            .unwrap()
            .values(&["application", "component", "option"], Some("value")),
        vec!["/tmp/A&B/路径"]
    );
}

#[test]
fn xml_reader_rejects_duplicate_attributes_after_many_unique_names() {
    let temp = tempdir();
    let xml = temp.path().join("selector.xml");
    let attributes = (0..32)
        .map(|index| format!(r#" key{index}="{index}""#))
        .collect::<String>();
    std::fs::write(&xml, format!(r#"<option{attributes} key0="duplicate"/>"#)).unwrap();

    assert_eq!(
        SelectorReader::default().read(&xml, SelectorFormat::Xml),
        Err(SelectorReadError::Parse)
    );
}
