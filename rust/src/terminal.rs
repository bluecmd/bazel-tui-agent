//! Terminal output: Bazel-style message lines plus the status block, as bytes for the client's
//! stderr (and stdout). The Java side only writes what `poll` and `finish` return.

use crate::dashboard::Dashboard;
use crate::events::Message;
use crate::pty::Pty;
use crate::render::Renderer;
use crate::state::State;
use crate::text::RESET;

/// Bytes destined for the two client streams.
#[derive(Default)]
pub struct Output {
    pub stderr: Vec<u8>,
    pub stdout: Vec<u8>,
}

impl Output {
    /// `[u32 LE stderr len][stderr][u32 LE stdout len][stdout]`, what Java unpacks.
    pub fn framed(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.stderr.len() + self.stdout.len() + 8);
        v.extend_from_slice(&(self.stderr.len() as u32).to_le_bytes());
        v.extend_from_slice(&self.stderr);
        v.extend_from_slice(&(self.stdout.len() as u32).to_le_bytes());
        v.extend_from_slice(&self.stdout);
        v
    }
}

pub struct Terminal {
    renderer: Renderer,
    pty: Option<Pty>,
    color: bool,
    /// Lines of the block currently on screen (0 = nothing drawn).
    drawn: usize,
    last_block: String,
    /// Message bytes waiting to be flushed above the block.
    pending: Output,
    /// Full-screen mode: known terminal size and a build in progress.
    dashboard: Option<Dashboard>,
    on_alt_screen: bool,
    last_frame: Vec<String>,
    /// Everything printed while on the alternate screen, replayed afterwards: (to stdout?, text).
    captured: Vec<(bool, Vec<u8>)>,
    log_lines: Vec<String>,
    partial: String,
    /// Size reported by Bazel's own resize RPC (9.3+), used when the pty is not available.
    reported: Option<(u16, u16)>,
}

impl Terminal {
    pub fn new(width: usize, color: bool, rows: usize, pty: Option<Pty>) -> Terminal {
        let mut t = Terminal {
            renderer: Renderer::new(width, color, rows),
            pty,
            color,
            drawn: 0,
            last_block: String::new(),
            pending: Output::default(),
            dashboard: None,
            on_alt_screen: false,
            last_frame: Vec::new(),
            captured: Vec::new(),
            log_lines: Vec::new(),
            partial: String::new(),
            reported: None,
        };
        t.sync_size();
        t
    }

    /// The dashboard needs the real terminal size (from the pty) and room for its panels.
    fn dashboard_possible(&self) -> Option<(u16, u16)> {
        let (cols, rows) = self.current_size()?;
        if cols >= 80 && rows >= 20 { Some((cols, rows)) } else { None }
    }

    /// Remember text for the log panel and for the replay when the alternate screen is left.
    fn capture(&mut self, to_stdout: bool, bytes: &[u8]) {
        self.captured.push((to_stdout, bytes.to_vec()));
        self.record_log(bytes);
    }

    /// Every line Bazel prints feeds the log panel, whether or not the dashboard is up yet.
    fn record_log(&mut self, bytes: &[u8]) {
        let text = format!("{}{}", self.partial, String::from_utf8_lossy(bytes));
        let mut parts: Vec<&str> = text.split('\n').collect();
        self.partial = parts.pop().unwrap_or("").to_string();
        for p in parts {
            let p = p.trim_end_matches('\r');
            let p = p.rsplit('\r').next().unwrap_or(p);
            self.log_lines.push(p.to_string());
        }
        if self.log_lines.len() > 50_000 {
            self.log_lines.drain(..10_000);
        }
    }

    /// Bazel 9.3+ reports the client's columns on SIGWINCH (rows arrive as 0).
    pub fn reported_size(&mut self, columns: u16, rows: u16) {
        if columns > 0 {
            self.reported = Some((columns, rows));
        }
    }

    /// The client's current window size: from its pty when we have it, else as reported by Bazel.
    fn current_size(&self) -> Option<(u16, u16)> {
        self.pty.as_ref().and_then(|p| p.size()).or(self.reported)
    }

    /// Picks up the client's current window size (resizes are invisible to the server otherwise).
    fn sync_size(&mut self) {
        if let Some((cols, rows)) = self.current_size() {
            self.renderer.resize(cols as usize, rows as usize);
        }
    }

    /// Keys typed at the client's terminal: `+`/`-` block height, `t` tree order, `l` big log.
    fn handle_keys(&mut self) {
        let Some(keys) = self.pty.as_ref().map(|p| p.read_keys()) else { return };
        for k in keys {
            match k {
                b'+' | b'=' => self.renderer.adjust_rows(1),
                b'-' | b'_' => self.renderer.adjust_rows(-1),
                b't' => {
                    self.renderer.toggle_tree_order();
                    if let Some(d) = self.dashboard.as_mut() {
                        d.tree_by_size = !d.tree_by_size;
                    }
                }
                b'l' => {
                    if let Some(d) = self.dashboard.as_mut() {
                        d.show_log_big = !d.show_log_big;
                    }
                }
                _ => {}
            }
        }
    }

    fn erase(&mut self, out: &mut Vec<u8>) {
        if self.drawn > 0 {
            out.push(b'\r');
            if self.drawn > 1 {
                out.extend_from_slice(format!("\x1b[{}A", self.drawn - 1).as_bytes());
            }
            out.extend_from_slice(b"\x1b[J");
            self.drawn = 0;
        }
        self.last_block.clear();
    }

    /// Formats a message the way Bazel's UiEventHandler prints it.
    pub fn message(&mut self, m: &Message) {
        match m.kind.as_str() {
            "START" | "FINISH" | "PASS" | "TIMEOUT" | "DEPCHECKER" | "PROGRESS" => return,
            "STDOUT" => {
                self.emit(true, m.text.as_bytes().to_vec());
                return;
            }
            "STDERR" => {
                self.emit(false, m.text.as_bytes().to_vec());
                return;
            }
            _ => {}
        }
        let color = if self.color {
            match m.kind.as_str() {
                "ERROR" | "FATAL" | "FAIL" => "\x1b[31;1m",
                "WARNING" => "\x1b[35m",
                "INFO" => "\x1b[32m",
                "DEBUG" => "\x1b[33m",
                "SUBCOMMAND" => "\x1b[34m",
                _ => "",
            }
        } else {
            ""
        };
        let mut line = String::new();
        line.push_str(color);
        line.push_str(&m.kind);
        line.push_str(": ");
        if self.color {
            line.push_str(RESET);
        }
        if !m.location.is_empty() {
            line.push_str(&m.location);
            line.push_str(": ");
        }
        line.push_str(&m.text);
        if !m.text.ends_with('\n') {
            line.push('\n');
        }
        self.emit(false, line.into_bytes());
        if !m.stderr.is_empty() {
            self.emit(false, m.stderr.as_bytes().to_vec());
        }
        if !m.stdout.is_empty() {
            self.emit(true, m.stdout.as_bytes().to_vec());
        }
    }

    /// Message bytes go to the screen now (inline mode) or into the capture (dashboard mode).
    fn emit(&mut self, to_stdout: bool, bytes: Vec<u8>) {
        if self.on_alt_screen {
            self.capture(to_stdout, &bytes);
        } else {
            self.record_log(&bytes);
            if to_stdout {
                self.pending.stdout.extend_from_slice(&bytes);
            } else {
                self.pending.stderr.extend_from_slice(&bytes);
            }
        }
    }

    /// Flushes pending messages above the block and redraws the block if it changed; or, once a
    /// build is running on a terminal of known size, drives the full-screen dashboard.
    pub fn poll(&mut self, state: &State, now_ms: i64) -> Output {
        self.sync_size();
        self.handle_keys();
        if state.command_builds() && !state.finished {
            if let Some((cols, rows)) = self.dashboard_possible() {
                return self.poll_dashboard(state, now_ms, cols, rows);
            }
        }
        if self.on_alt_screen {
            // Terminal shrank below the dashboard's needs: fall back to the inline block.
            let mut out = Output::default();
            self.leave_alt_screen(&mut out);
            return out;
        }
        let block = self.renderer.render(state, now_ms);
        let mut out = Output::default();
        if self.pending.stderr.is_empty() && self.pending.stdout.is_empty() && block == self.last_block {
            return out;
        }
        self.erase(&mut out.stderr);
        out.stderr.append(&mut self.pending.stderr);
        out.stdout.append(&mut self.pending.stdout);
        if !block.is_empty() {
            out.stderr.extend_from_slice(block.as_bytes());
            self.drawn = block.lines().count().max(1);
        }
        self.last_block = block;
        out
    }

    fn poll_dashboard(&mut self, state: &State, now_ms: i64, cols: u16, rows: u16) -> Output {
        let mut out = Output::default();
        if !self.on_alt_screen {
            // Flush whatever was pending inline, erase the block, then switch screens.
            self.erase(&mut out.stderr);
            out.stderr.append(&mut self.pending.stderr);
            out.stdout.append(&mut self.pending.stdout);
            out.stderr.extend_from_slice(b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[H\x1b[2J");
            self.on_alt_screen = true;
            self.last_frame.clear();
        }
        let color = self.color;
        let d = self.dashboard.get_or_insert_with(|| Dashboard::new(cols, rows, color));
        d.resize(cols, rows);
        let frame = d.render(state, &self.log_lines, now_ms);
        let full = self.last_frame.len() != frame.len();
        for (y, line) in frame.iter().enumerate() {
            if full || self.last_frame.get(y) != Some(line) {
                out.stderr.extend_from_slice(format!("\x1b[{};1H", y + 1).as_bytes());
                out.stderr.extend_from_slice(line.as_bytes());
            }
        }
        self.last_frame = frame;
        out
    }

    /// Back to the normal screen, with everything that was printed meanwhile replayed in order.
    fn leave_alt_screen(&mut self, out: &mut Output) {
        if !self.on_alt_screen {
            return;
        }
        out.stderr.extend_from_slice(b"\x1b[?7h\x1b[?1049l\x1b[?25h");
        for (to_stdout, bytes) in self.captured.drain(..) {
            if to_stdout { out.stdout.extend_from_slice(&bytes) } else { out.stderr.extend_from_slice(&bytes) }
        }
        self.on_alt_screen = false;
        self.dashboard = None;
        self.last_frame.clear();
        self.drawn = 0;
        self.last_block.clear();
    }

    /// End of command: leave the dashboard or clear the block, flush, add Bazel's completion line
    /// and the summary.
    pub fn finish(&mut self, state: &State, now_ms: i64) -> Output {
        let mut out = Output::default();
        self.leave_alt_screen(&mut out);
        self.erase(&mut out.stderr);
        out.stderr.append(&mut self.pending.stderr);
        out.stdout.append(&mut self.pending.stdout);
        let summary = self.renderer.summary(state, now_ms);
        if !summary.is_empty() {
            out.stderr.extend_from_slice(summary.as_bytes());
            out.stderr.push(b'\n');
        }
        // Give the terminal back (termios) before Bazel's client runs anything else, e.g. `bazel run`.
        self.pty = None;
        out
    }
}
