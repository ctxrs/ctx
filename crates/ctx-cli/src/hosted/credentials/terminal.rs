use std::io::{Read, Write};

use anyhow::{bail, Context, Result};

pub(super) fn read_hidden() -> Result<Vec<u8>> {
    let mode = Mode::hidden()
        .context("hide enrollment input; use --enrollment-file if the terminal is unavailable")?;
    let mut stderr = ctx_terminal::output::stderr_writer();
    write!(stderr, "Paste invitation JSON (hidden), then press Enter: ")?;
    stderr.flush()?;
    let result = read_line(&mut std::io::stdin().lock());
    drop(mode);
    writeln!(stderr)?;
    result
}

fn read_line(input: &mut impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut too_long = false;
    loop {
        let mut byte = [0];
        if input.read(&mut byte).context("read enrollment input")? == 0 {
            bail!("enrollment input was cancelled");
        }
        match byte[0] {
            b'\r' | b'\n' => {
                if too_long {
                    bail!("credential exceeds the supported size");
                }
                return Ok(bytes);
            }
            3 | 4 => bail!("enrollment input was cancelled"),
            8 | 127 => {
                // Remove one complete UTF-8 character during hidden editing.
                while bytes.pop().is_some_and(|b| b & 0xc0 == 0x80) {}
            }
            byte if !too_long => bytes.push(byte),
            _ => (),
        }
        if bytes.len() > super::MAX_CREDENTIAL_BYTES as usize {
            // Drain the rest of this paste while echo is still disabled.
            too_long = true;
        }
    }
}

#[cfg(unix)]
struct Mode(libc::termios);

#[cfg(unix)]
impl Mode {
    fn hidden() -> std::io::Result<Self> {
        let mut saved = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes the termios object on success.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, saved.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let saved = unsafe { saved.assume_init() };
        let mut hidden = saved;
        hidden.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
        hidden.c_cc[libc::VMIN] = 1;
        hidden.c_cc[libc::VTIME] = 0;
        // Read cancellation as input so the guard restores echo on Ctrl-C/D.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &hidden) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(saved))
    }
}

#[cfg(unix)]
impl Drop for Mode {
    fn drop(&mut self) {
        // Discard remaining pasted bytes before restoring terminal echo.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.0);
        }
    }
}

#[cfg(windows)]
struct Mode(windows_sys::Win32::Foundation::HANDLE, u32);

#[cfg(windows)]
impl Mode {
    fn hidden() -> std::io::Result<Self> {
        use windows_sys::Win32::System::Console::*;
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut saved = 0;
        if unsafe { GetConsoleMode(handle, &mut saved) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let hidden = saved & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
        if unsafe { SetConsoleMode(handle, hidden) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(handle, saved))
    }
}

#[cfg(windows)]
impl Drop for Mode {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{FlushConsoleInputBuffer, SetConsoleMode};
        unsafe {
            FlushConsoleInputBuffer(self.0);
            SetConsoleMode(self.0, self.1);
        }
    }
}

#[cfg(not(any(unix, windows)))]
struct Mode;

#[cfg(not(any(unix, windows)))]
impl Mode {
    fn hidden() -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "use --enrollment-file on this platform",
        ))
    }
}
