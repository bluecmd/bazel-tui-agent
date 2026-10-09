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
        Some(Pty { input, size_fd })
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

    /// Whatever the user typed since the last call (raw bytes; empty when nothing or no input tty).
    pub fn read_keys(&self) -> Vec<u8> {
        let Some((fd, _)) = self.input else { return Vec::new() };
        let mut buf = [0u8; 64];
        // SAFETY: non-blocking read into a stack buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 { Vec::new() } else { buf[..n as usize].to_vec() }
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
