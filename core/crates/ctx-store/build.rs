use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=migrations");
    emit_rerun_for_dir(Path::new("migrations"));
}

fn emit_rerun_for_dir(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            emit_rerun_for_dir(&path);
            continue;
        }
        if let Some(display) = path.to_str() {
            println!("cargo:rerun-if-changed={display}");
        }
    }
}
