#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn docs_show_out_creates_parent_directories() {
    let temp = tempdir();
    let out = temp.path().join("nested").join("doc.txt");

    ctx(&temp)
        .args([
            "docs",
            "show",
            "cli-reference",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        out.exists(),
        "docs show --out should write the requested file"
    );
    let body = fs::read_to_string(&out).unwrap();
    assert!(body.contains("CLI Reference"), "{body}");
}
