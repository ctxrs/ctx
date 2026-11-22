#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod windows;

#[cfg(windows)]
fn main() {
    let code = windows::run().unwrap_or_else(|e| e.raw_os_error().unwrap_or(1) as u32);
    std::process::exit(code as i32);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ctx-task-launcher requires Windows Task Scheduler");
    std::process::exit(1);
}
