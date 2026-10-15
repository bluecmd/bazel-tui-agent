//! Scrolling and searching in the dashboard's log panels.
//!
//! The log is the list of lines Bazel printed so far (`Terminal::log_lines`). By default the
//! panel follows the tail; the first scroll key pins a window, End or a PageDown past the end
//! follows again. `/` types a query; matches are kept as line indices and extended as lines
//! arrive, so a long build does not re-scan the log on every frame. Lines longer than the panel
//! are cut, not wrapped; `←`/`→` shift the window sideways. Each line carries the time it
//! was captured, shown greyed on the left of the panels only (the replay into scrollback at
//! the end is Bazel's bytes as they were).

use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use crate::pty::Key;
use crate::render::{bold, st};
use crate::text::{fmt_clock, MUTED, TITLE, WARN};

/// One captured line of Bazel's output and when it arrived.
pub struct LogLine {
    pub at_ms: i64,
    pub text: String,
}

impl LogLine {
    pub fn at(at_ms: i64, text: impl Into<String>) -> LogLine {
        LogLine { at_ms, text: text.into() }
    }
}

/// Width of the stamp column in the panels: `HH:MM:SS` and a space.
pub const STAMP_W: u16 = 9;

#[derive(Default)]
pub struct LogView {
    /// First line shown; `None` follows the tail.
    pub scroll: Option<usize>,
    /// First column shown, for lines wider than the panel.
    pub hscroll: usize,
    /// Columns the panel gives the text, set by the last draw: how far sideways makes sense.
    pub text_width: usize,
    pub query: String,
    /// `/` was pressed: keys edit the query until Enter or Esc.
    pub typing: bool,
    /// Indices of lines matching the query, ascending.
    pub matches: Vec<usize>,
    /// The match `n`/`N` moved to last (index into `matches`).
    pub current: Option<usize>,
    /// Log length the matches cover; lines beyond it are scanned on the next frame.
    scanned: usize,
    /// Lower-cased query, what lines are matched against.
    needle: String,
}

/// Where the visible lines come from and what to highlight: computed once per frame.
pub struct Window {
    pub start: usize,
    pub end: usize,
}

/// How far `←`/`→` move.
const HSTEP: usize = 8;

impl LogView {
    /// Keys while the log has the keyboard. Returns false for keys it does not use, so the
    /// caller can apply its own bindings.
    pub fn key(&mut self, k: Key, log: &[LogLine], height: usize) -> bool {
        let log_len = log.len();
        let page = height.max(1);
        if self.typing {
            match k {
                Key::Char(c) => self.query.push(c),
                Key::Backspace => {
                    self.query.pop();
                }
                Key::Ctrl('u') => self.query.clear(),
                Key::Enter | Key::Tab => self.typing = false,
                Key::Esc | Key::Ctrl('c') | Key::Ctrl('g') => {
                    self.typing = false;
                    self.query.clear();
                }
                _ => return false,
            }
            self.rescan();
            self.update(log);
            // The newest match is the one of interest in a log; jump there as the query changes.
            if self.typing || k == Key::Enter {
                self.current = self.matches.len().checked_sub(1);
                self.show_current(log_len, height);
            }
            return true;
        }
        match k {
            Key::Char('/') => {
                self.typing = true;
                self.rescan();
            }
            Key::Up | Key::Ctrl('p') | Key::Char('k') => self.scroll_by(-1, log_len, height),
            Key::Down | Key::Ctrl('n') | Key::Char('j') => self.scroll_by(1, log_len, height),
            Key::PageUp | Key::Ctrl('b') => self.scroll_by(-(page as i64), log_len, height),
            Key::PageDown | Key::Ctrl('f') => self.scroll_by(page as i64, log_len, height),
            Key::Left => self.hscroll = self.hscroll.saturating_sub(HSTEP),
            Key::Right => {
                // No further than brings the end of the widest line on screen to the panel's
                // right edge; a short window (the panel height) bounds the scan.
                let w = self.window(log_len, height);
                let widest = log[w.start..w.end].iter().map(|l| plain(&l.text).width()).max().unwrap_or(0);
                self.hscroll = (self.hscroll + HSTEP).min(widest.saturating_sub(self.text_width));
            }
            Key::Home => self.scroll = Some(0),
            Key::End => self.scroll = None,
            Key::Char('n') if !self.query.is_empty() => self.step(1, log_len, height),
            Key::Char('N') if !self.query.is_empty() => self.step(-1, log_len, height),
            Key::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.rescan();
                self.update(log);
            }
            Key::Esc => {
                self.scroll = None;
                self.hscroll = 0;
            }
            _ => return false,
        }
        true
    }

    fn scroll_by(&mut self, delta: i64, log_len: usize, height: usize) {
        let max_start = log_len.saturating_sub(height);
        let cur = self.scroll.unwrap_or(max_start) as i64;
        let next = (cur + delta).clamp(0, max_start as i64) as usize;
        self.scroll = if next >= max_start { None } else { Some(next) };
    }

    /// `n` (newer) and `N` (older) over the matches, starting from the newest one.
    fn step(&mut self, dir: i64, log_len: usize, height: usize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.current = Some(match self.current {
            None => last,
            Some(i) => (i as i64 + dir).clamp(0, last as i64) as usize,
        });
        self.show_current(log_len, height);
    }

    /// Scrolls so the current match sits in the middle of the window.
    fn show_current(&mut self, log_len: usize, height: usize) {
        let Some(line) = self.current.and_then(|i| self.matches.get(i)).copied() else { return };
        let max_start = log_len.saturating_sub(height);
        let start = line.saturating_sub(height / 2).min(max_start);
        self.scroll = if start >= max_start { None } else { Some(start) };
    }

    fn rescan(&mut self) {
        self.needle = self.query.to_lowercase();
        self.matches.clear();
        self.scanned = 0;
        self.current = None;
    }

    /// Brings the matches up to date with lines that arrived since the last frame.
    pub fn update(&mut self, log: &[LogLine]) {
        if self.needle.is_empty() {
            self.matches.clear();
            self.scanned = log.len();
            return;
        }
        if log.len() < self.scanned {
            // The log was trimmed from the front; indices are stale.
            self.matches.clear();
            self.scanned = 0;
            self.current = None;
        }
        for (i, line) in log.iter().enumerate().skip(self.scanned) {
            if plain(&line.text).to_lowercase().contains(&self.needle) {
                self.matches.push(i);
            }
        }
        self.scanned = log.len();
    }

    /// The lines to show in a panel of `height` rows.
    pub fn window(&mut self, log_len: usize, height: usize) -> Window {
        let max_start = log_len.saturating_sub(height);
        if let Some(s) = self.scroll {
            if s >= max_start {
                self.scroll = None;
            }
        }
        let start = self.scroll.unwrap_or(max_start).min(max_start);
        Window { start, end: (start + height).min(log_len) }
    }

    /// The panel's right-hand title: position when scrolled, the match count when searching.
    pub fn title(&self, log_len: usize, w: &Window) -> String {
        let mut t = String::new();
        if !self.query.is_empty() {
            let pos = self.current.map_or(String::new(), |i| format!("{}/", i + 1));
            t.push_str(&format!("/{}  {}{} matches  ", self.query, pos, self.matches.len()));
        }
        if self.scroll.is_some() && log_len > 0 {
            t.push_str(&format!("{}-{} of {} lines", w.start + 1, w.end, log_len));
        } else {
            t.push_str(&format!("{} lines", log_len));
        }
        if self.hscroll > 0 {
            t.push_str(&format!("  col {}+", self.hscroll + 1));
        }
        t.push_str("  [l]");
        t
    }

    /// Styles one log line: the base style for its kind, matches of the query highlighted.
    pub fn spans(&self, line: &str, base: Style) -> Vec<Span<'static>> {
        let text = plain(line);
        if self.needle.is_empty() {
            return vec![Span::styled(text, base)];
        }
        let lower = text.to_lowercase();
        // Byte offsets in `lower` map to `text` only when lower-casing kept the lengths; fall
        // back to an unhighlighted line otherwise (rare non-ASCII case folds).
        if lower.len() != text.len() {
            return vec![Span::styled(text, base)];
        }
        let hl = bold(TITLE).bg(ratatui::style::Color::Indexed(crate::text::cube(WARN.0, WARN.1, WARN.2))).fg(ratatui::style::Color::Black);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(i) = lower[at..].find(&self.needle) {
            let s = at + i;
            let e = s + self.needle.len();
            if s > at {
                out.push(Span::styled(text[at..s].to_string(), base));
            }
            out.push(Span::styled(text[s..e].to_string(), hl));
            at = e;
        }
        if at < text.len() {
            out.push(Span::styled(text[at..].to_string(), base));
        }
        out
    }

    /// The greyed capture time of a line, for the stamp column.
    pub fn stamp(line: &LogLine) -> Span<'static> {
        Span::styled(format!("{} ", fmt_clock(line.at_ms)), st(MUTED))
    }

    /// The hint row while the log has the keyboard.
    pub fn hint(&self) -> Vec<Span<'static>> {
        let mut v = Vec::new();
        if self.typing {
            v.push(Span::styled(" / ", bold(crate::text::ACCENT)));
            v.push(Span::styled(format!("{}▏", self.query), bold(TITLE)));
            v.push(Span::styled("  Enter done  Esc cancel  ", st(crate::text::MUTED)));
            return v;
        }
        let items: &[(&str, &str)] = if self.query.is_empty() {
            &[("↑↓ PgUp PgDn", "scroll  "), ("←→", "sideways  "), ("Home End", "top / follow  "), ("/", "search  "), ("l", "close  "), ("^C", "cancel")]
        } else {
            &[("n N", "newer / older match  "), ("↑↓ PgUp PgDn", "scroll  "), ("←→", "sideways  "), ("/", "edit  "), ("Esc", "clear  "), ("l", "close  "), ("^C", "cancel")]
        };
        for (k, what) in items {
            v.push(Span::styled(format!(" {} ", k), bold(crate::text::ACCENT)));
            v.push(Span::styled((*what).to_string(), st(crate::text::MUTED)));
        }
        v
    }
}

/// A captured line without its ANSI escapes.
pub fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut in_esc = false;
    for c in line.chars() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_esc = true;
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(n: usize) -> Vec<LogLine> {
        (0..n).map(|i| LogLine::at(1_000 * i as i64, if i % 10 == 3 { format!("WARNING: line {} has a Warning", i) } else { format!("INFO: line {}", i) })).collect()
    }

    #[test]
    fn follows_the_tail_until_scrolled_and_again_at_the_end() {
        let mut v = LogView::default();
        let w = v.window(100, 10);
        assert_eq!((w.start, w.end), (90, 100));
        v.key(Key::Up, &log(100), 10);
        assert_eq!(v.window(100, 10).start, 89);
        v.key(Key::PageUp, &log(100), 10);
        assert_eq!(v.window(100, 10).start, 79);
        v.key(Key::Home, &log(100), 10);
        assert_eq!(v.window(100, 10).start, 0);
        // The log grows while pinned: the window stays put.
        assert_eq!(v.window(150, 10).start, 0);
        v.key(Key::PageDown, &log(150), 10);
        v.key(Key::PageDown, &log(150), 10);
        assert_eq!(v.window(150, 10).start, 20);
        v.key(Key::End, &log(150), 10);
        assert!(v.scroll.is_none());
        assert_eq!(v.window(150, 10).start, 140);
        // Scrolling to the end is the same as following.
        v.key(Key::Up, &log(150), 10);
        v.key(Key::Down, &log(150), 10);
        assert!(v.scroll.is_none());
    }

    #[test]
    fn search_finds_matches_incrementally_and_steps_through_them() {
        let mut v = LogView::default();
        let mut l = log(50);
        assert!(v.key(Key::Char('/'), &l, 10));
        for c in "warn".chars() {
            v.key(Key::Char(c), &l, 10);
            v.update(&l);
        }
        assert_eq!(v.matches, vec![3, 13, 23, 33, 43]);
        assert!(v.typing);
        v.key(Key::Enter, &l, 10);
        v.update(&l);
        assert!(!v.typing);
        assert_eq!(v.current, Some(4));
        // The newest match is centred in the window.
        assert_eq!(v.window(l.len(), 10).start, 38);
        v.key(Key::Char('N'), &l, 10);
        assert_eq!(v.current, Some(3));
        assert_eq!(v.window(l.len(), 10).start, 28);
        v.key(Key::Char('n'), &l, 10);
        v.key(Key::Char('n'), &l, 10);
        assert_eq!(v.current, Some(4));
        // New lines extend the matches without a rescan of the old ones.
        l.extend(log(20));
        v.update(&l);
        assert_eq!(v.matches.len(), 7);
        assert_eq!(v.scanned, 70);
        let w = v.window(l.len(), 10);
        assert!(v.title(l.len(), &w).contains("5/7 matches"), "{}", v.title(l.len(), &w));
        // Typing keys do not reach the dashboard's own bindings; others do once typing is over.
        assert!(v.key(Key::Char('/'), &l, 10));
        assert!(v.key(Key::Char('l'), &l, 10));
        assert_eq!(v.query, "warnl");
        v.key(Key::Esc, &l, 10);
        assert!(v.query.is_empty() && !v.typing);
        assert!(!v.key(Key::Char('l'), &l, 10));
    }

    #[test]
    fn sideways_scroll_is_bounded_by_the_widest_visible_line_and_reset_by_esc() {
        let mut v = LogView { text_width: 20, ..LogView::default() };
        let mut l = log(20);
        l[19].text = format!("INFO: {}", "x".repeat(30));
        v.key(Key::Left, &l, 10);
        assert_eq!(v.hscroll, 0);
        v.key(Key::Right, &l, 10);
        assert_eq!(v.hscroll, 8);
        for _ in 0..10 {
            v.key(Key::Right, &l, 10);
        }
        // "INFO: " + 30 = 36 wide in a 20-column panel: the end of the line meets the edge.
        assert_eq!(v.hscroll, 16);
        let w = v.window(l.len(), 10);
        assert!(v.title(l.len(), &w).ends_with("col 17+  [l]"), "{}", v.title(l.len(), &w));
        v.key(Key::Left, &l, 10);
        assert_eq!(v.hscroll, 8);
        v.key(Key::Esc, &l, 10);
        assert_eq!(v.hscroll, 0);
    }

    #[test]
    fn highlights_every_occurrence_case_insensitively() {
        let mut v = LogView::default();
        v.key(Key::Char('/'), &[], 10);
        for c in "warn".chars() {
            v.key(Key::Char(c), &[], 10);
        }
        let spans = v.spans("\x1b[35mWARNING:\x1b[0m a warning, no caps", Style::new());
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["WARN", "ING: a ", "warn", "ing, no caps"]);
        assert_eq!(plain("\x1b[31;1mERROR:\x1b[0m x"), "ERROR: x");
    }
}
