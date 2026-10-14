//! Terminal output: Bazel-style message lines plus the status block, as bytes for the client's
//! stderr (and stdout). The Java side only writes what `poll` and `finish` return.

use crate::logview::LogLine;
use crate::dashboard::Dashboard;
use crate::events::Message;
use crate::help::{self, Finder};
use crate::pty::{Key, Pty};
use crate::render::Renderer;
use crate::state::State;
use crate::summary;
use crate::text::RESET;

const ENTER_ALT: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[H\x1b[2J";
const LEAVE_ALT: &[u8] = b"\x1b[?7h\x1b[?1049l\x1b[?25h";

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
    log_lines: Vec<LogLine>,
    partial: String,
    /// Size reported by Bazel's own resize RPC (9.3+), used when the pty is not available.
    reported: Option<(u16, u16)>,
    command: String,
    /// Inside one of Bazel's test statistics lines, which arrive in several stdout writes.
    swallow_stdout_line: bool,
    /// `bazel help` on an interactive terminal: Bazel's usage text is held back and the finder
    /// takes the screen until the user picks an entry or quits.
    help_mode: bool,
    finder: Option<Finder>,
    help_done: bool,
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
            command: String::new(),
            swallow_stdout_line: false,
            help_mode: false,
            finder: None,
            help_done: false,
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
            self.log_lines.push(LogLine::now(p.to_string()));
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

    /// The command just started: decide whether this is an interactive `help`.
    pub fn command_started(&mut self, state: &State) {
        self.command = state.command.clone().unwrap_or_default();
        self.help_mode = state.interactive_help() && self.pty.as_ref().map_or(false, |p| p.interactive()) && self.current_size().is_some();
    }

    /// True while the help finder owns the screen, or is about to (the index has arrived but no
    /// poll has run yet: `help` is over in milliseconds, before the timer's first tick); the
    /// Java side keeps polling until it is not.
    pub fn busy(&self, state: &State) -> bool {
        self.help_mode && !self.help_done && (self.finder.is_some() || !state.help.is_empty())
    }

    /// Keys typed at the client's terminal: `+`/`-` block height, `t` tree order, `l` big log.
    /// With the big log up, the log view takes scrolling and search keys first.
    fn handle_keys(&mut self, keys: Vec<Key>) {
        for k in keys {
            if let Some(d) = self.dashboard.as_mut() {
                if (d.show_log_big || d.log.typing) && d.log.key(k, &self.log_lines, d.log_height) {
                    continue;
                }
            }
            match k {
                Key::Char('+') | Key::Char('=') => self.renderer.adjust_rows(1),
                Key::Char('-') | Key::Char('_') => self.renderer.adjust_rows(-1),
                Key::Char('t') => {
                    self.renderer.toggle_tree_order();
                    if let Some(d) = self.dashboard.as_mut() {
                        d.tree_by_size = !d.tree_by_size;
                    }
                }
                Key::Char('l') => {
                    if let Some(d) = self.dashboard.as_mut() {
                        d.show_log_big = !d.show_log_big;
                    }
                }
                _ => {}
            }
        }
    }

    fn read_keys(&self) -> Vec<Key> {
        self.pty.as_ref().map(|p| p.read_keys()).unwrap_or_default()
    }

    /// Bazel's own test statistics lines ("Executed 3 out of 4 tests: ..." and the test-size
    /// hint), replaced by the overview in `summary.rs`.
    fn is_bazel_test_stats(&mut self, m: &Message) -> bool {
        if !matches!(self.command.as_str(), "test" | "coverage") || m.kind != "STDOUT" {
            return false;
        }
        if self.swallow_stdout_line {
            self.swallow_stdout_line = !m.text.contains('\n');
            return true;
        }
        let t = m.text.trim_start();
        if (t.starts_with("Executed ") && t.contains(" out of ")) || t.starts_with("There were tests whose specified size is too big") {
            self.swallow_stdout_line = !t.contains('\n');
            return true;
        }
        false
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
        if self.is_bazel_test_stats(m) {
            return;
        }
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

    /// Message bytes go to the screen now (inline mode) or into the capture (dashboard mode, and
    /// interactive help, where Bazel's usage text only comes out if the finder is quit).
    fn emit(&mut self, to_stdout: bool, bytes: Vec<u8>) {
        if self.on_alt_screen || (self.help_mode && !self.help_done) {
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
        let keys = self.read_keys();
        if self.help_mode {
            return self.poll_help(state, keys);
        }
        self.handle_keys(keys);
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
            out.stderr.extend_from_slice(ENTER_ALT);
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

    /// The help finder: starts once the index has arrived, then follows the keys until an entry
    /// is chosen (printed to stdout) or the user quits (Bazel's usage text is released).
    fn poll_help(&mut self, state: &State, keys: Vec<Key>) -> Output {
        let mut out = Output::default();
        if self.help_done {
            return out;
        }
        let Some((cols, rows)) = self.current_size() else { return out };
        if self.finder.is_none() {
            if state.help.is_empty() {
                return out;
            }
            out.stderr.extend_from_slice(ENTER_ALT);
            self.on_alt_screen = true;
            self.last_frame.clear();
            self.finder = Some(Finder::new(&state.help, cols, rows, self.color));
        }
        let color = self.color;
        let f = self.finder.as_mut().expect("finder");
        f.resize(cols, rows);
        for k in keys {
            f.key(k);
        }
        if f.finished() {
            let chosen = f.result().map(|it| help::print(it, color, cols as usize));
            if chosen.is_some() {
                self.captured.clear();
            }
            self.help_done = true;
            self.leave_alt_screen(&mut out);
            if let Some(text) = chosen {
                out.stdout.extend_from_slice(text.as_bytes());
            }
            return out;
        }
        let frame = f.render();
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
        out.stderr.extend_from_slice(LEAVE_ALT);
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
        if self.help_mode && !self.help_done {
            // Interrupted, or the index never arrived: hand out Bazel's own text.
            self.help_done = true;
        }
        self.leave_alt_screen(&mut out);
        if self.help_mode {
            for (to_stdout, bytes) in self.captured.drain(..) {
                if to_stdout { out.stdout.extend_from_slice(&bytes) } else { out.stderr.extend_from_slice(&bytes) }
            }
        }
        self.erase(&mut out.stderr);
        out.stderr.append(&mut self.pending.stderr);
        out.stdout.append(&mut self.pending.stdout);
        let width = self.current_size().map(|(c, _)| c as usize).unwrap_or_else(|| self.renderer.width());
        let rep = summary::report(state, now_ms, self.color, width);
        out.stderr.extend_from_slice(rep.stderr.as_bytes());
        out.stdout.extend_from_slice(rep.stdout.as_bytes());
        if let Some(n) = summary::stale_jar_notice(state, self.color) {
            out.stderr.extend_from_slice(n.as_bytes());
        }
        // Give the terminal back (termios) before Bazel's client runs anything else, e.g. `bazel run`.
        self.pty = None;
        out
    }
}
