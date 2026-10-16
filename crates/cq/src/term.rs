//! Local terminal handling for terminal clients: raw mode, size, input capture and raw output.

use cq_protocol::Size;

/// Events produced by the local console.
pub enum TermEvent {
    Input(Vec<u8>),
    Resize(Size),
}

/// Encodings of Ctrl+\ that detach the local terminal without stopping the session: the plain
/// control byte, and the forms terminals send once the agent enables modifyOtherKeys or the kitty
/// keyboard protocol.
const DETACH_KEYS: &[&[u8]] = &[b"\x1c", b"\x1b[27;5;92~", b"\x1b[92;5u"];

/// Returns whether an input chunk contains the detach key.
pub fn is_detach(input: &[u8]) -> bool {
    DETACH_KEYS.iter().any(|k| input.windows(k.len()).any(|w| w == *k))
}

/// Escape sequences that undo modes an agent may have left enabled in the outer terminal.
pub const RESET_MODES: &[u8] = b"\x1b[<99u\x1b[>4m\x1b[?2004l\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b>\x1b[0m\x1b[?25h\x1b[999;1H\r\n";

/// Sequence sent for Shift+Enter. Claude Code reads it as "insert newline".
#[cfg(windows)]
const SHIFT_ENTER: &str = "\x1b[13;2u";

/// Tracks whether the input stream is inside a bracketed paste.
#[cfg(any(windows, test))]
#[derive(Default)]
struct PasteTracker {
    tail: Vec<u16>,
    active: bool,
}

#[cfg(any(windows, test))]
impl PasteTracker {
    fn feed(&mut self, c: u16) {
        self.tail.push(c);
        if self.tail.len() > 6 {
            self.tail.remove(0);
        }
        let start: Vec<u16> = "\x1b[200~".encode_utf16().collect();
        let end: Vec<u16> = "\x1b[201~".encode_utf16().collect();
        if self.tail == start {
            self.active = true;
        } else if self.tail == end {
            self.active = false;
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{PasteTracker, SHIFT_ENTER, TermEvent};
    use anyhow::{Result, bail};
    use cq_protocol::Size;
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
        pub fn enter() -> Result<Self> {
            unsafe {
                let stdin = GetStdHandle(STD_INPUT_HANDLE);
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                let (mut in_mode, mut out_mode) = (0u32, 0u32);
                if GetConsoleMode(stdin, &mut in_mode) == 0 {
                    bail!("stdin is not a console");
                }
                GetConsoleMode(stdout, &mut out_mode);
                let me = Self { stdin, stdout, in_mode, out_mode, in_cp: GetConsoleCP(), out_cp: GetConsoleOutputCP() };
                SetConsoleCP(65001);
                SetConsoleOutputCP(65001);
                if SetConsoleMode(stdin, ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS) == 0 {
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
                let ok = unsafe { WriteFile(self.stdout, buf.as_ptr(), chunk, &mut n, std::ptr::null_mut()) };
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
    pub fn spawn_reader(tx: Sender<TermEvent>) {
        std::thread::spawn(move || {
            let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            read_input(stdin, tx);
        });
    }

    /// Reads input records in VT input mode.
    ///
    /// The console translates keys into VT sequences delivered as synthetic key-down records
    /// (the same bytes `ReadConsoleW` would return, including IME text and bracketed pastes), but
    /// VT has no Shift+Enter. Modifier keys still arrive as their own records, so modifier state
    /// is tracked here and a bare Shift+Enter is re-encoded as `CSI 13;2u`.
    fn read_input(stdin: HANDLE, tx: Sender<TermEvent>) {
        const VK_SHIFT: u16 = 0x10;
        const VK_CONTROL: u16 = 0x11;
        const VK_MENU: u16 = 0x12;
        let mut records: [INPUT_RECORD; 128] = unsafe { std::mem::zeroed() };
        let (mut shift, mut ctrl, mut alt) = (false, false, false);
        let mut paste = PasteTracker::default();
        let mut pending_high: Option<u16> = None;
        loop {
            let mut n = 0u32;
            if unsafe { ReadConsoleInputW(stdin, records.as_mut_ptr(), 128, &mut n) } == 0 {
                return;
            }
            let mut units: Vec<u16> = pending_high.take().into_iter().collect();
            for r in &records[..n as usize] {
                if r.EventType as u32 == WINDOW_BUFFER_SIZE_EVENT {
                    if tx.send(TermEvent::Resize(size())).is_err() {
                        return;
                    }
                    continue;
                }
                if r.EventType as u32 != KEY_EVENT {
                    continue;
                }
                let k = unsafe { r.Event.KeyEvent };
                let down = k.bKeyDown != 0;
                match k.wVirtualKeyCode {
                    VK_SHIFT => shift = down,
                    VK_CONTROL => ctrl = down,
                    VK_MENU => alt = down,
                    _ => {}
                }
                let c = unsafe { k.uChar.UnicodeChar };
                if !down || c == 0 {
                    continue;
                }
                for _ in 0..k.wRepeatCount.max(1) {
                    if c == 0x0d && shift && !ctrl && !alt && !paste.active {
                        units.extend(SHIFT_ENTER.encode_utf16());
                    } else {
                        units.push(c);
                        paste.feed(c);
                    }
                }
            }
            if let Some(&last) = units.last() {
                if (0xD800..0xDC00).contains(&last) {
                    pending_high = units.pop();
                }
            }
            if units.is_empty() {
                continue;
            }
            let text = String::from_utf16_lossy(&units);
            if tx.send(TermEvent::Input(text.into_bytes())).is_err() {
                return;
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::TermEvent;
    use anyhow::{Result, bail};
    use cq_protocol::Size;
    use std::io::{Read, Write};
    use tokio::sync::mpsc::UnboundedSender as Sender;

    /// Restores the original termios settings on drop.
    pub struct RawTerminal {
        orig: libc::termios,
    }

    impl RawTerminal {
        /// Switches the terminal to raw mode.
        pub fn enter() -> Result<Self> {
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

    /// Reads stdin on a dedicated thread until the channel closes. Unix terminals pass key
    /// encodings (including Shift+Enter under the kitty protocol) through unchanged.
    pub fn spawn_reader(tx: Sender<TermEvent>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_detach_encodings() {
        assert!(is_detach(b"\x1c"));
        assert!(is_detach(b"ab\x1b[27;5;92~"));
        assert!(is_detach(b"\x1b[92;5u"));
        assert!(!is_detach(b"\x1b[27;5;99~"));
    }

    #[test]
    fn tracks_bracketed_paste() {
        let mut p = PasteTracker::default();
        "\x1b[200~x".encode_utf16().for_each(|c| p.feed(c));
        assert!(p.active);
        "y\x1b[201~".encode_utf16().for_each(|c| p.feed(c));
        assert!(!p.active);
    }
}
