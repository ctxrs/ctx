use super::*;
use std::{cell::RefCell, process::Child};

#[test]
fn office_zip_reads_documents_and_rejects_traversal() {
    use std::io::Write;
    use zip::{ZipWriter, write::SimpleFileOptions};

    for extra_name in ["notes.txt", "../outside.txt"] {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(b"<document><body><p><r><t>Office text</t></r></p></body></document>")
            .unwrap();
        writer
            .start_file(extra_name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"extra member").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let result = office(&bytes, "docx", 4096);
        if extra_name == "notes.txt" {
            assert_eq!(result.unwrap().text.trim(), "Office text");
        } else {
            assert!(
                result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("unsafe Office archive member path")
            );
        }
    }
}

type SampleHook = Box<dyn FnOnce(&mut Child)>;
thread_local! {
    static AFTER_SAMPLE: RefCell<Option<SampleHook>> = const { RefCell::new(None) };
}

pub(super) fn after_output_sample(child: &mut Child) {
    AFTER_SAMPLE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(child);
        }
    });
}

#[cfg(unix)]
#[test]
fn final_capture_limits_reject_writes_between_sample_and_successful_exit() {
    let temp = tempfile::tempdir().unwrap();
    for (stream, output_file) in [("stderr", false), ("stdout", true), ("quiet", true)] {
        let gate = temp.path().join(stream);
        let script = temp.path().join(format!("{stream}.py"));
        std::fs::write(
            &script,
            r#"
import pathlib, sys, time
gate, output, stream = sys.argv[1:]
deadline = time.monotonic() + 5
while not pathlib.Path(gate).exists():
    assert time.monotonic() < deadline
    time.sleep(0.001)
pathlib.Path(output).write_text('bounded output')
if stream != 'quiet':
    getattr(sys, stream).write('x' * 4097)
    getattr(sys, stream).flush()
"#,
        )
        .unwrap();
        let adapter = CommandAdapter {
            program: "python3".into(),
            args: vec![
                script.to_string_lossy().into_owned(),
                gate.to_string_lossy().into_owned(),
                "{output}".into(),
                stream.into(),
            ],
            output_file,
        };
        AFTER_SAMPLE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |child| {
                // The first size sample sees empty streams. Only then let
                // the child write, and observe success before try_wait.
                std::fs::write(&gate, b"release").unwrap();
                assert!(child.wait().unwrap().success());
            }));
        });
        let result = run_bytes(&adapter, None, None, 5, 4096);
        if stream == "quiet" {
            assert_eq!(result.unwrap(), b"bounded output");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("output exceeds byte limit")
            );
        }
    }
}
