const COMMANDS: &[&str] = &["resolve"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build()
}
