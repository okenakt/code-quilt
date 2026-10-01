//! Local terminal handling for terminal clients: raw mode, size, input capture and raw output.

use cq_proto::Size;

/// How keyboard input is captured from the local console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Read VT sequences produced by the console and forward the bytes unchanged.
    Vt,
    /// Windows only: read native key events and re-encode them as win32-input-mode sequences,
    /// which ConPTY decodes back into full-fidelity key events for the agent.
    Win32,
}

impl InputMode {
    /// Reads the mode from `CQ_INPUT_MODE` (`vt` or `win32`), defaulting to `vt`.
    pub fn from_env() -> Self {
        match std::env::var("CQ_INPUT_MODE").as_deref() {
            Ok("win32") if cfg!(windows) => InputMode::Win32,
            _ => InputMode::Vt,
        }
    }
}

/// Events produced by the local console.
pub enum TermEvent {
    Input(Vec<u8>),
    Resize(Size),
}

/// Byte that detaches the local terminal without stopping the session (Ctrl+).
pub const DETACH_BYTE: u8 = 0x1c;

/// Escape sequences that undo modes an agent may have left enabled in the outer terminal.
pub const RESET_MODES: &[u8] =
    b"\x1b[<99u\x1b[?2004l\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b>\x1b[0m\x1b[?25h\x1b[999;1H\r\n";

#[cfg(windows)]
mod imp {
    use super::{InputMode, TermEvent};
    use anyhow::{Result, bail};
    use cq_proto::Size;
    use tokio::sync::mpsc::UnboundedSender as Sender;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::*;

    /// Restores the console's original modes and code pages on drop.
    pub struct RawTerminal {
        stdin: HANDLE,
        stdout: HANDLE,
        in_mode: u32,
        out_mode: u32,
        in_cp: u32,
        out_cp: u32,
    }

    // SAFETY: console handles are process-global and usable from any thread.
    unsafe impl Send for RawTerminal {}

    impl RawTerminal {
        /// Switches the console to raw VT mode.
        pub fn enter(mode: InputMode) -> Result<Self> {
            unsafe {
                let stdin = GetStdHandle(STD_INPUT_HANDLE);
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                let (mut in_mode, mut out_mode) = (0u32, 0u32);
                if GetConsoleMode(stdin, &mut in_mode) == 0 {
                    bail!("stdin is not a console");
                }
                GetConsoleMode(stdout, &mut out_mode);
                let me = Self {
                    stdin,
                    stdout,
                    in_mode,
                    out_mode,
                    in_cp: GetConsoleCP(),
                    out_cp: GetConsoleOutputCP(),
                };
                SetConsoleCP(65001);
                SetConsoleOutputCP(65001);
                let raw_in = match mode {
                    InputMode::Vt => ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS,
                    InputMode::Win32 => ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS,
                };
                if SetConsoleMode(stdin, raw_in) == 0 {
                    bail!("SetConsoleMode(stdin): {}", std::io::Error::last_os_error());
                }
                // Autowrap must stay on: agents draw full-width lines and rely on deferred wrap.
                SetConsoleMode(
                    stdout,
                    ENABLE_PROCESSED_OUTPUT
                        | ENABLE_WRAP_AT_EOL_OUTPUT
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                        | DISABLE_NEWLINE_AUTO_RETURN,
                );
                Ok(me)
            }
        }

        /// Writes raw bytes to the console without UTF-8 validation.
        pub fn write(&self, mut buf: &[u8]) {
            while !buf.is_empty() {
                let mut n = 0u32;
                let chunk = buf.len().min(u32::MAX as usize) as u32;
                let ok = unsafe {
                    WriteFile(self.stdout, buf.as_ptr(), chunk, &mut n, std::ptr::null_mut())
                };
                if ok == 0 || n == 0 {
                    return;
                }
                buf = &buf[n as usize..];
            }
        }
    }

    impl Drop for RawTerminal {
        fn drop(&mut self) {
            self.write(super::RESET_MODES);
            unsafe {
                SetConsoleMode(self.stdin, self.in_mode);
                SetConsoleMode(self.stdout, self.out_mode);
                SetConsoleCP(self.in_cp);
                SetConsoleOutputCP(self.out_cp);
            }
        }
    }

    /// Returns the visible window size of the console.
    pub fn size() -> Size {
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) == 0 {
                return Size { cols: 80, rows: 24 };
            }
            let w = info.srWindow;
            Size { cols: (w.Right - w.Left + 1) as u16, rows: (w.Bottom - w.Top + 1) as u16 }
        }
    }

    /// Reads console input on a dedicated thread until the channel closes.
    pub fn spawn_reader(mode: InputMode, tx: Sender<TermEvent>) {
        std::thread::spawn(move || {
            let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            match mode {
                InputMode::Vt => read_vt(stdin, tx),
                InputMode::Win32 => read_win32(stdin, tx),
            }
        });
    }

    fn read_vt(stdin: HANDLE, tx: Sender<TermEvent>) {
        // ReadConsoleW (UTF-16) is used instead of ReadFile because UTF-8 console input is
        // unreliable for non-ASCII text such as IME-composed Japanese.
        let mut buf = [0u16; 4096];
        let mut pending: Option<u16> = None;
        loop {
            let mut n = 0u32;
            let ok = unsafe {
                ReadConsoleW(stdin, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n, std::ptr::null())
            };
            if ok == 0 {
                return;
            }
            let mut units: Vec<u16> = pending.take().into_iter().collect();
            units.extend_from_slice(&buf[..n as usize]);
            if let Some(&last) = units.last() {
                if (0xD800..0xDC00).contains(&last) {
                    pending = units.pop();
                }
            }
            let text = String::from_utf16_lossy(&units);
            if !text.is_empty() && tx.send(TermEvent::Input(text.into_bytes())).is_err() {
                return;
            }
        }
    }

    fn read_win32(stdin: HANDLE, tx: Sender<TermEvent>) {
        let mut records: [INPUT_RECORD; 128] = unsafe { std::mem::zeroed() };
        loop {
            let mut n = 0u32;
            let ok = unsafe { ReadConsoleInputW(stdin, records.as_mut_ptr(), 128, &mut n) };
            if ok == 0 {
                return;
            }
            let mut out = Vec::new();
            for r in &records[..n as usize] {
                match r.EventType as u32 {
                    KEY_EVENT => {
                        let k = unsafe { r.Event.KeyEvent };
                        let uc = unsafe { k.uChar.UnicodeChar };
                        if uc == u16::from(super::DETACH_BYTE) {
                            if k.bKeyDown != 0 {
                                out.push(super::DETACH_BYTE);
                            }
                            continue;
                        }
                        // win32-input-mode: CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _
                        out.extend_from_slice(
                            format!(
                                "\x1b[{};{};{};{};{};{}_",
                                k.wVirtualKeyCode,
                                k.wVirtualScanCode,
                                uc,
                                k.bKeyDown,
                                k.dwControlKeyState,
                                k.wRepeatCount
                            )
                            .as_bytes(),
                        );
                    }
                    WINDOW_BUFFER_SIZE_EVENT => {
                        if tx.send(TermEvent::Resize(size())).is_err() {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            if !out.is_empty() && tx.send(TermEvent::Input(out)).is_err() {
                return;
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::{InputMode, TermEvent};
    use anyhow::{Result, bail};
    use cq_proto::Size;
    use std::io::{Read, Write};
    use tokio::sync::mpsc::UnboundedSender as Sender;

    /// Restores the original termios settings on drop.
    pub struct RawTerminal {
        orig: libc::termios,
    }

    impl RawTerminal {
        /// Switches the terminal to raw mode.
        pub fn enter(_mode: InputMode) -> Result<Self> {
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) != 0 {
                    bail!("stdin is not a terminal");
                }
                let orig = t;
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(0, libc::TCSANOW, &t);
                Ok(Self { orig })
            }
        }

        /// Writes raw bytes to stdout.
        pub fn write(&self, buf: &[u8]) {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(buf);
            let _ = out.flush();
        }
    }

    impl Drop for RawTerminal {
        fn drop(&mut self) {
            self.write(super::RESET_MODES);
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.orig) };
        }
    }

    /// Returns the terminal size.
    pub fn size() -> Size {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) != 0 || ws.ws_col == 0 {
                return Size { cols: 80, rows: 24 };
            }
            Size { cols: ws.ws_col, rows: ws.ws_row }
        }
    }

    /// Reads stdin on a dedicated thread until the channel closes.
    pub fn spawn_reader(_mode: InputMode, tx: Sender<TermEvent>) {
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut stdin = std::io::stdin().lock();
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(TermEvent::Input(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
        });
    }
}

pub use imp::{RawTerminal, size, spawn_reader};
