//! The client's terminal, found through /proc from the pid the Bazel client reports.
//!
//! The server has no terminal of its own, but the client process that asked for this command has
//! one on fd 0/2 (when it is interactive). Opening that pty gives the server what the gRPC stream
//! cannot: the real window size (polled; SIGWINCH goes to the client, not to us) and keystrokes.
//! Keys need the tty out of canonical mode; that is a terminal-wide setting, so the original
//! termios is restored when the engine ends (and ISIG stays on so Ctrl-C still reaches Bazel).
//! Output keeps going through the gRPC stream, in order with Bazel's own messages.

use std::ffi::CString;
use std::os::unix::io::RawFd;

pub struct Pty {
    input: Option<(RawFd, libc::termios)>,
    size_fd: RawFd,
    stdout_tty: bool,
}

/// A decoded keystroke from the client's terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Backspace,
    Delete,
    Esc,
    Tab,
    /// Control-letter, as the lower-case letter ('c' for Ctrl-C).
    Ctrl(char),
}

/// Turns raw tty bytes into keys: CSI/SS3 sequences for cursor and editing keys, UTF-8 text,
/// a lone ESC as `Esc`. Incomplete escape sequences at the end of the buffer are dropped.
pub fn decode_keys(bytes: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            if bytes.get(i + 1) == Some(&b'[') {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                if j >= bytes.len() {
                    break;
                }
                let params = &bytes[i + 2..j];
                let key = match bytes[j] {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'C' => Some(Key::Right),
                    b'D' => Some(Key::Left),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    b'~' => match params {
                        b"1" | b"7" => Some(Key::Home),
                        b"4" | b"8" => Some(Key::End),
                        b"3" => Some(Key::Delete),
                        b"5" => Some(Key::PageUp),
                        b"6" => Some(Key::PageDown),
                        _ => None,
                    },
                    _ => None,
                };
                out.extend(key);
                i = j + 1;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'O') {
                let key = match bytes.get(i + 2) {
                    Some(b'A') => Some(Key::Up),
                    Some(b'B') => Some(Key::Down),
                    Some(b'C') => Some(Key::Right),
                    Some(b'D') => Some(Key::Left),
                    Some(b'H') => Some(Key::Home),
                    Some(b'F') => Some(Key::End),
                    _ => None,
                };
                out.extend(key);
                i += 3;
                continue;
            }
            out.push(Key::Esc);
            i += 1;
            continue;
        }
        match b {
            b'\r' | b'\n' => out.push(Key::Enter),
            0x7f | 0x08 => out.push(Key::Backspace),
            b'\t' => out.push(Key::Tab),
            0x01..=0x1a => out.push(Key::Ctrl((b'a' + b - 1) as char)),
            _ if b < 0x80 => out.push(Key::Char(b as char)),
            _ => {
                let len = if b >= 0xf0 { 4 } else if b >= 0xe0 { 3 } else if b >= 0xc0 { 2 } else { 1 };
                let end = (i + len).min(bytes.len());
                if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
                    out.extend(s.chars().map(Key::Char));
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

impl Pty {
    pub fn open(client_pid: i64) -> Option<Pty> {
        if client_pid <= 0 {
            return None;
        }
        let tty = |fd: i32| -> Option<String> {
            let p = std::fs::read_link(format!("/proc/{}/fd/{}", client_pid, fd)).ok()?;
            let s = p.to_string_lossy().into_owned();
            if s.starts_with("/dev/pts/") || s.starts_with("/dev/tty") { Some(s) } else { None }
        };
        let err_tty = tty(2)?;
        let size_fd = open(&err_tty)?;
        let input = tty(0).and_then(|p| open(&p)).and_then(|fd| {
            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            // SAFETY: fd is an open tty; termios is a plain C struct.
            if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
                unsafe { libc::close(fd) };
                return None;
            }
            let mut raw = saved;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ECHONL);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                unsafe { libc::close(fd) };
                return None;
            }
            Some((fd, saved))
        });
        let stdout_tty = tty(1).is_some();
        Some(Pty { input, size_fd, stdout_tty })
    }

    /// Keys can be read and both output streams go to the terminal: a full-screen interaction
    /// (the help finder) is possible without breaking a pipeline.
    pub fn interactive(&self) -> bool {
        self.input.is_some() && self.stdout_tty
    }

    /// (columns, rows) of the client's terminal right now.
    pub fn size(&self) -> Option<(u16, u16)> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ fills a winsize struct.
        if unsafe { libc::ioctl(self.size_fd, libc::TIOCGWINSZ, &mut ws) } != 0 || ws.ws_col == 0 {
            return None;
        }
        Some((ws.ws_col, ws.ws_row))
    }

    /// Whatever the user typed since the last call (empty when nothing or no input tty).
    pub fn read_keys(&self) -> Vec<Key> {
        let Some((fd, _)) = self.input else { return Vec::new() };
        let mut buf = [0u8; 256];
        // SAFETY: non-blocking read into a stack buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 { Vec::new() } else { decode_keys(&buf[..n as usize]) }
    }

}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some((fd, saved)) = self.input.take() {
            // SAFETY: restore the terminal exactly as found, then close our handles.
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &saved);
                libc::close(fd);
            }
        }
        unsafe { libc::close(self.size_fd) };
    }
}

fn open(path: &str) -> Option<RawFd> {
    let c = CString::new(path).ok()?;
    // SAFETY: plain open(2); O_NOCTTY so the server never acquires a controlling terminal.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { None } else { Some(fd) }
}
