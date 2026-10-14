//! The status block, built from ratatui widgets rendered into an in-memory buffer.
//!
//! There is no real terminal here (the server writes a byte stream the Bazel client copies to
//! the user's terminal), so the frame goes through ratatui's `TestBackend` and the buffer is
//! converted to ANSI lines, which `terminal.rs` erases and redraws in place.

use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::state::{ActState, Fetch, Finish, Finished, Source, State, FADE_MS};
use crate::summary::fmt_size;
use crate::text::*;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// Left-aligned eighth blocks for the cell a bar is filling.
const LEFT_BLOCKS: &[char] = &[' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Gradient progress bar as spans, one cell each: a strip growing to the right over the
/// terminal's own background, like the graphs, with no track behind it. The cell being filled
/// is a left-aligned eighth block.
pub(crate) fn bar_spans(frac: f64, width: usize, stops: &[(f32, (u8, u8, u8))]) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    let cells = frac * width as f64;
    let full = cells.floor() as usize;
    (0..width)
        .map(|i| {
            let t = i as f32 / (width.max(2) - 1) as f32;
            if i < full {
                Span::styled("█", st(gradient(stops, t)))
            } else if i == full && cells - full as f64 > 0.0 {
                Span::styled(LEFT_BLOCKS[(((cells - full as f64) * 8.0) as usize).clamp(1, 8)].to_string(), st(gradient(stops, t)))
            } else {
                Span::raw(" ")
            }
        })
        .collect()
}

/// Indeterminate bar: a short bright run sliding along, for a download without a known size.
pub(crate) fn pulse_spans(width: usize, now: i64, stops: &[(f32, (u8, u8, u8))]) -> Vec<Span<'static>> {
    const RUN: i64 = 4;
    let w = width.max(1) as i64;
    let head = (now / 100) % (w + RUN);
    (0..w)
        .map(|i| {
            let d = head - i;
            if (0..RUN).contains(&d) {
                Span::styled("█", st(gradient(stops, 1.0 - d as f32 / RUN as f32)))
            } else {
                Span::raw(" ")
            }
        })
        .collect()
}

/// A download's bar: progress when the size (or Bazel's percentage) is known, a pulse otherwise.
pub(crate) fn fetch_bar(f: &Fetch, width: usize, now: i64) -> Vec<Span<'static>> {
    match f.frac() {
        Some(frac) => bar_spans(frac, width, DOWNLOAD),
        None => pulse_spans(width, now, DOWNLOAD),
    }
}

/// A download's resource for display: URLs become "host …/file" so the file name survives
/// truncation; anything else goes through `short_label`.
pub(crate) fn fetch_label(res: &str, width: usize) -> String {
    let Some((_, rest)) = res.split_once("://") else { return short_label(res, width) };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let file = path.rsplit('/').next().unwrap_or("");
    let full = format!("{}/{}", host, path);
    if full.chars().count() <= width || file.is_empty() {
        return short_label(&full, width);
    }
    let short = format!("{} …/{}", host, file);
    if short.chars().count() <= width {
        return short;
    }
    if file.chars().count() <= width {
        return file.to_string();
    }
    // File names say what they are at the start (rustc-…, rust-std-…), so cut the end.
    let head: String = file.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", head)
}

/// The download to feature in a one-line view (the furthest along) with its bar and numbers:
/// "↓ host …/file ████   61%  37/61 MiB  6.0 MiB/s  +1 more". None without a download.
/// The downloads in flight, the one to feature first: a known size beats an unknown one, then
/// the most bytes so far.
pub(crate) fn downloads(s: &State) -> Vec<(&String, &Fetch)> {
    let mut v: Vec<(&String, &Fetch)> = s.fetches.iter().filter(|(_, f)| f.is_download()).collect();
    v.sort_by(|a, b| b.1.frac().is_some().cmp(&a.1.frac().is_some()).then(b.1.bytes.cmp(&a.1.bytes)));
    v
}

pub(crate) fn download_spans(s: &State, labelw: usize, barw: usize, now: i64) -> Option<Vec<Span<'static>>> {
    let downloads = downloads(s);
    let (res, f) = downloads.first()?;
    let mut out = vec![Span::styled("↓ ", st(ACCENT))];
    if labelw > 0 {
        out.push(Span::raw(fetch_label(res, labelw)));
        out.push(Span::raw(" "));
    }
    out.extend(fetch_bar(f, barw, now));
    out.extend(fetch_detail(f));
    if downloads.len() > 1 {
        out.push(Span::styled(format!("  +{} more", downloads.len() - 1), dim()));
    }
    Some(out)
}

/// `download_spans` sized to fit in `avail` columns: the bar and numbers first, the label with
/// whatever is left. None when even a short bar would not fit.
pub(crate) fn download_spans_fitting(s: &State, avail: usize, now: i64) -> Option<Vec<Span<'static>>> {
    const NUMBERS: usize = 30; // "↓ " + " 62%  30/48 MiB  6.0 MiB/s" + "  +1 more"
    if avail < NUMBERS + 6 {
        return None;
    }
    let barw = ((avail - NUMBERS) / 3).clamp(6, 24);
    let labelw = avail.saturating_sub(NUMBERS + barw + 1);
    download_spans(s, if labelw >= 8 { labelw.min(40) } else { 0 }, barw, now)
}

/// The numbers next to a download's bar: "31%  37.2/120 MiB  4.1 MiB/s", each part only when
/// known. Repository fetches (no byte counts) get Bazel's own text instead.
pub(crate) fn fetch_detail(f: &Fetch) -> Vec<Span<'static>> {
    fetch_detail_with(f, false)
}

/// `fetch_detail` with every column padded to a fixed width, so the bars of several downloads
/// listed one per line start at the same column.
pub(crate) fn fetch_detail_aligned(f: &Fetch) -> Vec<Span<'static>> {
    fetch_detail_with(f, true)
}

fn fetch_detail_with(f: &Fetch, aligned: bool) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    if !f.is_download() {
        out.push(Span::styled(f.progress.chars().take(40).collect::<String>(), st(MUTED)));
        return out;
    }
    match f.frac() {
        Some(frac) => out.push(Span::styled(format!("{:3}%", (frac * 100.0) as u32), bold(TITLE))),
        None if aligned => out.push(Span::raw("    ")),
        None => {}
    }
    let size = if f.total > 0 {
        // Same unit for both numbers so "37.2/120 MiB" reads as one quantity.
        let total = fmt_size(f.total);
        let unit = total.split_once(' ').map(|(_, u)| u).unwrap_or("B");
        let done = fmt_size(f.bytes);
        match done.split_once(' ') {
            Some((n, u)) if u == unit => format!("{}/{}", n, total),
            _ => format!("{} / {}", done, total),
        }
    } else {
        fmt_size(f.bytes)
    };
    let rate = f.rate().map(|r| format!("{}/s", fmt_size(r as u64))).unwrap_or_default();
    if aligned {
        out.push(Span::raw(format!("  {:>13}", size)));
        out.push(Span::styled(format!("  {:>10}", rate), st(MUTED)));
    } else {
        out.push(Span::raw(format!("  {}", size)));
        if !rate.is_empty() {
            out.push(Span::styled(format!("  {}", rate), st(MUTED)));
        }
    }
    out
}

/// Colour a finished action's text fades towards (a dark terminal's background, roughly).
const FADE_TO: (u8, u8, u8) = (60, 80, 62);

/// Milliseconds under a second, seconds with a decimal under ten, Bazel-style minutes above.
fn fmt_took(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        fmt_duration(ms as f64 / 1000.0)
    }
}

/// One row of a running panel for an action that just finished: mark, mnemonic and description
/// on the left, how it ended and how long it ran on the right, every colour faded by its age.
pub(crate) fn finished_row(f: &Finished, now: i64, indent: bool) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let t = ((now - f.done_ms).max(0) as f32 / FADE_MS as f32).clamp(0.0, 1.0);
    let fade = |c: (u8, u8, u8)| st(lerp(c, FADE_TO, t));
    let (mark, how) = match f.how {
        Finish::Done => ("✓ ", if f.strategy.is_empty() { "done".to_string() } else { f.strategy.clone() }),
        Finish::Cached => ("≡ ", "cached".to_string()),
        Finish::Failed => ("✗ ", "FAILED".to_string()),
    };
    let mark_rgb = match f.how { Finish::Failed => BAD, Finish::Cached => MUTED, Finish::Done => OK };
    let desc = f.desc.strip_prefix(&format!("{} ", f.mnemonic)).unwrap_or(&f.desc);
    let body = if !f.label.is_empty() && desc.contains(&f.label) {
        desc.replacen(&f.label, &short_label(&f.label, 48), 1)
    } else {
        format!("{:<30}  {}", short_label(&f.label, 30), desc)
    };
    let mut left = Vec::new();
    if indent {
        left.push(Span::raw(" "));
    }
    left.push(Span::styled(mark, fade(mark_rgb)));
    left.push(Span::styled(format!("{:<11} ", f.mnemonic.chars().take(11).collect::<String>()), fade(mnemonic_rgb(&f.mnemonic))));
    left.push(Span::styled(body, fade((200, 205, 215))));
    let right = vec![
        Span::styled(format!("{:>16} ", how.chars().take(16).collect::<String>()), fade(if f.how == Finish::Failed { BAD } else { MUTED })),
        Span::styled(format!("{:>6}", f.took_ms.map(fmt_took).unwrap_or_default()), fade(MUTED)),
    ];
    (left, right)
}

pub(crate) fn st(c: (u8, u8, u8)) -> Style {
    Style::new().fg(rgb(c))
}

pub(crate) fn bold(c: (u8, u8, u8)) -> Style {
    st(c).add_modifier(Modifier::BOLD)
}

pub(crate) fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

pub struct Renderer {
    width: u16,
    rows: u16,
    /// Terminal height when known (from the client's pty); caps the block height.
    term_rows: u16,
    color: bool,
    tick: usize,
    tree_by_size: bool,
    term: Terminal<TestBackend>,
}

impl Renderer {
    pub fn new(width: usize, color: bool, rows: usize) -> Renderer {
        let width = width.clamp(40, 500) as u16 - 1; // one column of margin against autowrap
        let rows = rows.clamp(4, 40) as u16;
        Renderer { width, rows, term_rows: 0, color, tick: 0, tree_by_size: false, term: Terminal::new(TestBackend::new(width, rows)).expect("test backend") }
    }

    /// Follows the client's terminal; the block keeps at most half the screen.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let width = cols.clamp(40, 500) as u16 - 1;
        self.term_rows = rows as u16;
        // rows == 0: width known but not height (Bazel's own resize RPC); keep the block height.
        let max_rows = if rows == 0 { 40 } else { (rows as u16 / 2).max(4) };
        let block_rows = self.rows.min(max_rows).max(4);
        if width != self.width || block_rows != self.term.backend().size().map(|s| s.height).unwrap_or(0) {
            self.width = width;
            self.term.backend_mut().resize(width, block_rows);
            self.term.clear().ok();
        }
    }

    pub fn adjust_rows(&mut self, delta: i32) {
        let max = if self.term_rows > 0 { (self.term_rows / 2).max(4) } else { 40 };
        self.rows = (self.rows as i32 + delta).clamp(4, max as i32) as u16;
        self.term.backend_mut().resize(self.width, self.rows);
        self.term.clear().ok();
    }

    pub fn width(&self) -> usize {
        self.width as usize + 1
    }

    pub fn toggle_tree_order(&mut self) {
        self.tree_by_size = !self.tree_by_size;
    }

    fn block_rows(&self) -> usize {
        self.term.backend().size().map(|s| s.height as usize).unwrap_or(self.rows as usize)
    }

    /// Gradient progress bar as spans, one cell each.
    fn bar(&self, frac: f64, width: usize) -> Vec<Span<'static>> {
        bar_spans(frac, width, HEAT)
    }

    pub fn render(&mut self, s: &State, now: i64) -> String {
        if s.command.is_none() || !s.command_builds() || s.phase == "finished" || s.finished {
            return String::new();
        }
        self.tick += 1;
        let width = self.width as usize;

        // ---- header (goes on the top border as titles)
        let spin = SPINNER[self.tick % SPINNER.len()];
        let targets: Vec<&str> = s.args.iter().map(|x| x.as_str()).take(3).collect();
        let aborting = s.cancelling();
        let mut title = vec![
            Span::styled(format!(" {} ", spin), st(if aborting { ABORT } else { ACCENT })),
            Span::styled(s.command.clone().unwrap_or_else(|| "bazel".into()), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}{} ", targets.join(" "), if s.args.len() > 3 { " …" } else { "" }), st(TITLE)),
        ];
        for p in ["loading", "analysis", "execution"] {
            match s.phase_t.get(p) {
                None => title.push(Span::styled(format!(" {} ·", p), st(MUTED))),
                Some(&t0) => {
                    let later = ["loading", "analysis", "execution", "finished"].iter().skip_while(|x| **x != p).skip(1).filter_map(|q| s.phase_t.get(*q)).max().copied();
                    let done_at = if p == "analysis" && s.analysis_done { later.or(Some(now)) } else { later };
                    match done_at {
                        Some(te) if te >= t0 && p != "execution" => title.push(Span::styled(format!(" {} ✓{}", p, fmt_duration((te - t0) as f64 / 1000.0)), st(OK))),
                        _ => title.push(Span::styled(format!(" {} ▸{}", p, fmt_duration((now - t0) as f64 / 1000.0)), bold(WARN))),
                    }
                }
            }
        }
        title.push(Span::raw(" "));
        let mut badges: Vec<Span> = vec![Span::raw(" ")];
        if s.warnings > 0 {
            badges.push(Span::styled(format!("⚠ {}  ", s.warnings), st(WARN)));
        }
        if s.errors > 0 || s.failed_actions > 0 {
            badges.push(Span::styled(format!("✗ {}  ", s.errors.max(s.failed_actions)), bold(BAD)));
        }
        if !s.tests.is_empty() {
            let passed = s.tests.values().filter(|t| t.status == "PASSED").count();
            let failed = s.tests.len() - passed;
            badges.push(Span::styled(format!("tests {}✓", passed), st(OK)));
            if failed > 0 {
                badges.push(Span::styled(format!(" {}✗", failed), st(BAD)));
            }
            badges.push(Span::raw("  "));
        }
        badges.push(Span::styled(format!("{} ", fmt_elapsed((now - s.t0_ms) as f64 / 1000.0)), bold(TITLE)));

        // ---- progress line
        let counts = s.exec_counts();
        let mut progress: Vec<Span> = Vec::new();
        // Set when a branch above already drew the featured download, so the server line does not repeat it.
        let mut dl_shown = false;
        let mut progress_right: Vec<Span> = Vec::new();
        if aborting {
            let secs = (now - s.cancelled_ms.unwrap_or(now)) / 1000;
            progress.push(Span::styled("ABORTING ", bold(ABORT)));
            progress.push(Span::styled(format!("Ctrl-C received · stopping {} running action(s), {}s so far · a third Ctrl-C kills the server", s.running_count(), secs), st(ABORT)));
        } else if s.analysis_live() {
            // The execution count sits at the right edge; a download's bar comes before the
            // counter's long text and that count when the width is short.
            let exec = counts.filter(|(_, total)| *total > 1).map(|(done, total)| format!("executing {}/{}", with_commas(done), with_commas(total)));
            let need = if downloads(s).is_empty() { 0 } else { 46 };
            progress.push(Span::styled("analysis ", st(MUTED)));
            let a = s.analysis.clone().unwrap_or_default();
            let a_max = width.saturating_sub(9 + need + exec.as_ref().map_or(0, |e| e.chars().count() + 3)).max(16);
            progress.push(Span::raw(if a.chars().count() > a_max { format!("{}…", a.chars().take(a_max - 1).collect::<String>()) } else { a }));
            let used = progress.iter().map(|sp| sp.content.width()).sum::<usize>();
            let right_w = exec.as_ref().map_or(0, |e| e.chars().count() + 3);
            if let Some(dl) = download_spans_fitting(s, width.saturating_sub(used + right_w + 2), now) {
                progress.push(Span::raw("  "));
                progress.extend(dl);
                dl_shown = true;
            }
            if let Some(e) = exec {
                progress_right.push(Span::styled(e, st(MUTED)));
            }
        } else if let Some((done, total)) = counts {
            let frac = if total > 0 { done as f64 / total as f64 } else { 0.0 };
            let barw = width.saturating_sub(48).clamp(10, 40);
            progress.extend(self.bar(frac, barw));
            progress.push(Span::raw(format!("  {}/{} ", with_commas(done), with_commas(total))));
            progress.push(Span::styled(format!("{:3}%", (frac * 100.0) as u32), bold(TITLE)));
            if let Some(eta) = s.eta_secs(now) {
                if eta >= 5.0 {
                    progress.push(Span::styled(format!("  ~{} left", fmt_duration(eta)), st(MUTED)));
                }
            }
            let mut right = format!("{} running", s.running_count());
            if s.peak_running > 0 {
                right.push_str(&format!(" · peak {}", s.peak_running));
            }
            let mut detail = String::new();
            if let Some(rate) = s.hit_rate() {
                right.push_str(&format!(" · {}% cached", (rate * 100.0).round() as u32));
                // Which caches, briefly: "(449 local, 3 remote)".
                let mut hits = Vec::new();
                for (src, word) in [(Source::ActionCache, "local"), (Source::RemoteCache, "remote"), (Source::DiskCache, "disk")] {
                    let n = s.source_count(src);
                    if n > 0 {
                        hits.push(format!("{} {}", with_commas(n), word));
                    }
                }
                if hits.len() > 1 || (hits.len() == 1 && !hits[0].ends_with("local")) {
                    detail.push_str(&format!(" ({})", hits.join(", ")));
                }
            }
            if s.source_count(Source::RemoteExec) > 0 {
                detail.push_str(&format!(" · {} remote exec", with_commas(s.source_count(Source::RemoteExec))));
            }
            // The breakdown only when it leaves room for the bar and the counters.
            if right.width() + detail.width() + barw + 30 <= width {
                right.push_str(&detail);
            }
            progress_right.push(Span::styled(right, st(MUTED)));
        } else if let (Some(l), false) = (&s.loading, s.analysis_done) {
            progress.push(Span::styled("loading ", st(MUTED)));
            progress.push(Span::raw(l.clone()));
            if let Some(dl) = download_spans_fitting(s, width.saturating_sub(progress.iter().map(|sp| sp.content.width()).sum::<usize>() + 2), now) {
                progress.push(Span::raw("  "));
                progress.extend(dl);
                dl_shown = true;
            } else if let Some(d) = &s.loading_detail {
                progress.push(Span::styled(format!("  {}", d), dim()));
            }
        } else if let Some(a) = &s.analysis {
            progress.push(Span::styled("analysis ", st(MUTED)));
            progress.push(Span::raw(a.clone()));
            if let Some(dl) = download_spans_fitting(s, width.saturating_sub(progress.iter().map(|sp| sp.content.width()).sum::<usize>() + 2), now) {
                progress.push(Span::raw("  "));
                progress.extend(dl);
                dl_shown = true;
            }
        } else if let Some((res, f)) = downloads(s).first().copied().or_else(|| s.fetches.iter().next()) {
            dl_shown = true;
            progress.push(Span::styled(format!("fetching {} ", s.fetches.len()), st(MUTED)));
            progress.push(Span::raw(fetch_label(res, 40)));
            progress.push(Span::raw("  "));
            if f.is_download() {
                progress.extend(fetch_bar(f, 12, now));
                progress.push(Span::raw(" "));
            }
            progress.extend(fetch_detail(f));
        } else if !s.activity.is_empty() {
            progress.push(Span::raw(s.activity.clone()));
        } else {
            progress.push(Span::styled("starting…", st(MUTED)));
        }

        // ---- graphs line: label, sparkline, value, label, sparkline, value, server stats
        let running_max = s.peak_running.max(s.cpus).max(1) as f32;
        let running_hist = sparkline_levels(&s.history_running.iter().map(|x| *x as f32).collect::<Vec<_>>(), 20, running_max);
        let tp_scale = {
            let mut v: Vec<f32> = s.history_tp.iter().rev().take(120).copied().collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.is_empty() { 1.0 } else { (v[((v.len() as f32 * 0.95) as usize).min(v.len() - 1)] * 1.15).max(1.0) }
        };
        let tp_hist = sparkline_levels(&s.history_tp, 20, tp_scale);
        let tp_now = s.history_tp.last().copied().unwrap_or(0.0);
        let mut server: Vec<Span> = Vec::new();
        if s.heap_max_mb > 0 {
            server.push(Span::styled("  heap ", st(MUTED)));
            server.push(Span::raw(format!("{:.1}/{}G", s.heap_used_mb as f64 / 1024.0, s.heap_max_mb / 1024)));
        }
        if s.cpus > 0 {
            let f = (s.load / s.cpus as f64) as f32;
            server.push(Span::styled("  load ", st(MUTED)));
            server.push(Span::styled(format!("{:.1}/{}", s.load, s.cpus), st(gradient(HEAT, f))));
        }
        let dls = if dl_shown { Vec::new() } else { downloads(s) };
        if let Some((res, f)) = dls.first() {
            server.push(Span::styled("  ↓ ", st(ACCENT)));
            server.push(Span::raw(fetch_label(res, 24)));
            server.push(Span::raw(" "));
            server.extend(fetch_bar(f, 8, now));
            if let Some(frac) = f.frac() {
                server.push(Span::styled(format!(" {}%", (frac * 100.0) as u32), dim()));
            }
            if dls.len() > 1 {
                server.push(Span::styled(format!(" +{}", dls.len() - 1), dim()));
            }
        } else if let Some((res, f)) = s.fetches.iter().next().filter(|_| !dl_shown) {
            server.push(Span::styled("  ↓ ", st(ACCENT)));
            server.push(Span::raw(fetch_label(res, 24)));
            server.push(Span::styled(format!(" {}", f.progress.chars().take(30).collect::<String>()), dim()));
            if s.fetches.len() > 1 {
                server.push(Span::styled(format!(" +{}", s.fetches.len() - 1), dim()));
            }
        }

        // ---- mnemonics line
        let mut items: Vec<(&String, &(u64, u64))> = s.mnemonic_stats.iter().collect();
        items.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        let mut mnemonics: Vec<Span> = vec![Span::styled("done ", st(MUTED))];
        for (i, (m, (c, ms))) in items.iter().take(7).enumerate() {
            if i > 0 {
                mnemonics.push(Span::styled(" · ", dim()));
            }
            mnemonics.push(Span::styled((*m).clone(), bold(mnemonic_rgb(m))));
            mnemonics.push(Span::raw(format!(" {}", c)));
            let avg = if *c > 0 { *ms as f64 / *c as f64 / 1000.0 } else { 0.0 };
            if avg >= 0.05 {
                mnemonics.push(Span::styled(format!(" {:.1}s", avg), dim()));
            }
        }

        // ---- running actions and tree
        let rows = self.block_rows();
        let fixed = 1 + 1 + (rows >= 6) as usize + (rows >= 7 && !items.is_empty()) as usize;
        let tree_lines = if rows >= 9 { 2 } else if rows >= 8 { 1 } else { 0 };
        let acts = s.running_sorted();
        let budget = rows.saturating_sub(fixed + tree_lines);
        let shown = acts.len().min(budget);
        let mut running_rows: Vec<(Vec<Span>, Vec<Span>)> = Vec::new();
        for a in acts.iter().take(shown) {
            let mark = match a.state {
                ActState::Run => Span::styled("▸ ", st(OK)),
                ActState::Cache => Span::styled("≡ ", st(MUTED)),
                ActState::Sched => Span::styled("… ", st(WARN)),
                ActState::Queued => Span::styled("· ", st(MUTED)),
            };
            let desc = a.desc.strip_prefix(&format!("{} ", a.mnemonic)).unwrap_or(&a.desc);
            let body = if !a.label.is_empty() && desc.contains(&a.label) {
                desc.replacen(&a.label, &short_label(&a.label, 48), 1)
            } else {
                format!("{:<30}  {}", short_label(&a.label, 30), desc)
            };
            let age = (now - a.t_ms).max(0) as f64 / 1000.0;
            let left = vec![mark, Span::styled(format!("{:<11} ", a.mnemonic.chars().take(11).collect::<String>()), bold(mnemonic_rgb(&a.mnemonic))), Span::raw(body)];
            let extra = a.progress.clone().unwrap_or_else(|| if a.state == ActState::Run { a.strategy.clone() } else { String::new() });
            let dur = if a.state == ActState::Run { fmt_duration(age) } else { String::new() };
            let right = vec![
                Span::styled(format!("{:>16} ", extra.chars().take(16).collect::<String>()), st(MUTED)),
                Span::styled(format!("{:>6}", dur), st(gradient(HEAT, (age / 60.0).min(1.0) as f32))),
            ];
            running_rows.push((left, right));
        }
        let more = acts.len().saturating_sub(shown);
        if more > 0 && !running_rows.is_empty() {
            let last = running_rows.len() - 1;
            running_rows[last] = (vec![Span::styled(format!("  … and {} more in flight", more + 1), st(MUTED))], vec![]);
        }
        // Rows left over go to what just finished, fading out.
        for f in s.recent_finished(now).into_iter().take(budget.saturating_sub(running_rows.len())) {
            running_rows.push(finished_row(f, now, false));
        }

        let mut tree: Vec<Line> = Vec::new();
        if tree_lines > 0 {
            let mut tops: Vec<(&String, &crate::state::Node)> = s.tree.children.iter().collect();
            if self.tree_by_size {
                tops.sort_by(|a, b| (b.1.done + b.1.running as u64).cmp(&(a.1.done + a.1.running as u64)).then(b.1.last_ms.cmp(&a.1.last_ms)));
            } else {
                tops.sort_by(|a, b| b.1.running.cmp(&a.1.running).then(b.1.last_ms.cmp(&a.1.last_ms)));
            }
            let mut cur: Vec<Span> = vec![Span::styled(if self.tree_by_size { "tree⇅" } else { "tree " }, st(MUTED))];
            let mut cur_w = 5;
            for (name, n) in tops.iter().take(40) {
                let mut item: Vec<Span> = Vec::new();
                let style = if n.running > 0 { bold(TITLE) } else if n.failed > 0 { st(BAD) } else if now - n.last_ms < 4000 { st(OK) } else { st(MUTED) };
                item.push(Span::styled((*name).clone(), style));
                if n.running > 0 {
                    item.push(Span::styled(format!(" ▸{}", n.running), bold(OK)));
                }
                if n.done > 0 {
                    item.push(Span::styled(format!(" ✓{}", n.done), st(MUTED)));
                }
                if n.failed > 0 {
                    item.push(Span::styled(format!(" ✗{}", n.failed), bold(BAD)));
                }
                let w: usize = item.iter().map(|sp| sp.content.width()).sum::<usize>() + 3;
                if cur_w + w > width {
                    if tree.len() + 1 >= tree_lines {
                        break;
                    }
                    tree.push(Line::from(std::mem::take(&mut cur)));
                    cur.push(Span::raw("     "));
                    cur_w = 5;
                }
                if cur_w > 5 {
                    cur.push(Span::styled(" │ ", dim()));
                }
                cur.extend(item);
                cur_w += w;
            }
            if cur.len() > 1 {
                tree.push(Line::from(cur));
            }
        }

        // ---- lay it out
        let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
        if rows >= 6 {
            constraints.push(Constraint::Length(1));
        }
        if rows >= 7 && !items.is_empty() {
            constraints.push(Constraint::Length(1));
        }
        for _ in &running_rows {
            constraints.push(Constraint::Length(1));
        }
        for _ in &tree {
            constraints.push(Constraint::Length(1));
        }
        let used: usize = constraints.len().min(rows);
        let mut title_line = Line::from(title);
        if !self.color {
            title_line = title_line.patch_style(Style::reset());
        }
        self.term
            .draw(|f| {
                let area = Rect::new(0, 0, f.area().width, used as u16);
                let block = Block::new()
                    .borders(Borders::TOP)
                    .border_style(st(MUTED))
                    .title_top(title_line.clone())
                    .title_top(Line::from(badges.clone()).right_aligned());
                let inner = block.inner(area);
                f.render_widget(block, area);
                let chunks = Layout::vertical(constraints[1..].to_vec()).split(inner);
                let mut i = 0;
                let lr = Layout::horizontal([Constraint::Fill(1), Constraint::Length(progress_right.iter().map(|sp| sp.content.width()).sum::<usize>() as u16 + 1)]).split(chunks[i]);
                f.render_widget(Paragraph::new(Line::from(progress.clone())), lr[0]);
                f.render_widget(Paragraph::new(Line::from(progress_right.clone())).right_aligned(), lr[1]);
                i += 1;
                if rows >= 6 {
                    let g = Layout::horizontal([
                        Constraint::Length(9),
                        Constraint::Length(20),
                        Constraint::Length(5),
                        Constraint::Length(6),
                        Constraint::Length(20),
                        Constraint::Length(7),
                        Constraint::Fill(1),
                    ])
                    .split(chunks[i]);
                    f.render_widget(Paragraph::new(Span::styled("parallel ", st(MUTED))), g[0]);
                    f.render_widget(Sparkline::default().data(&running_hist).max(8).style(st(gradient(HEAT, s.running_count() as f32 / running_max))), g[1]);
                    f.render_widget(Paragraph::new(format!(" {}", s.running_count())), g[2]);
                    f.render_widget(Paragraph::new(Span::styled("act/s ", st(MUTED))), g[3]);
                    f.render_widget(Sparkline::default().data(&tp_hist).max(8).style(st(COOL[1].1)), g[4]);
                    f.render_widget(Paragraph::new(format!(" {:.1}", tp_now)), g[5]);
                    f.render_widget(Paragraph::new(Line::from(server.clone())), g[6]);
                    i += 1;
                }
                if rows >= 7 && !items.is_empty() {
                    f.render_widget(Paragraph::new(Line::from(mnemonics.clone())), chunks[i]);
                    i += 1;
                }
                for (left, right) in &running_rows {
                    if i >= chunks.len() {
                        break;
                    }
                    let rw = right.iter().map(|sp| sp.content.width()).sum::<usize>() as u16;
                    let lr = Layout::horizontal([Constraint::Fill(1), Constraint::Length(rw + 1)]).split(chunks[i]);
                    f.render_widget(Paragraph::new(Line::from(left.clone())), lr[0]);
                    f.render_widget(Paragraph::new(Line::from(right.clone())).right_aligned(), lr[1]);
                    i += 1;
                }
                for line in &tree {
                    if i >= chunks.len() {
                        break;
                    }
                    f.render_widget(Paragraph::new(line.clone()), chunks[i]);
                    i += 1;
                }
            })
            .ok();
        buffer_to_ansi(self.term.backend().buffer(), used, self.color)
    }
}

/// Folds a whole history into `cols` buckets (max of each) scaled to sparkline levels 0..8,
/// so the graph spans the entire build and small non-zero values still show a bar.
pub(crate) fn sparkline_levels(hist: &[f32], cols: usize, vmax: f32) -> Vec<u64> {
    if hist.is_empty() {
        return Vec::new();
    }
    let per = (hist.len() as f32 / cols as f32).max(1.0);
    let mut out = Vec::with_capacity(cols);
    let mut i = 0.0f32;
    while (i as usize) < hist.len() && out.len() < cols {
        let end = ((i + per) as usize).min(hist.len()).max(i as usize + 1);
        let m = hist[i as usize..end].iter().cloned().fold(0.0f32, f32::max);
        let level = if m <= 0.0 { 0 } else { ((m / vmax.max(1e-6)) * 8.0).ceil().clamp(1.0, 8.0) as u64 };
        out.push(level);
        i += per;
    }
    out
}

fn sgr(color: Color, bg: bool) -> Option<String> {
    let base = if bg { 40 } else { 30 };
    let ext = if bg { 48 } else { 38 };
    Some(match color {
        Color::Reset => return None,
        Color::Black => format!("{}", base),
        Color::Red => format!("{}", base + 1),
        Color::Green => format!("{}", base + 2),
        Color::Yellow => format!("{}", base + 3),
        Color::Blue => format!("{}", base + 4),
        Color::Magenta => format!("{}", base + 5),
        Color::Cyan => format!("{}", base + 6),
        Color::Gray => format!("{}", base + 7),
        Color::DarkGray => format!("{}", base + 60),
        Color::LightRed => format!("{}", base + 61),
        Color::LightGreen => format!("{}", base + 62),
        Color::LightYellow => format!("{}", base + 63),
        Color::LightBlue => format!("{}", base + 64),
        Color::LightMagenta => format!("{}", base + 65),
        Color::LightCyan => format!("{}", base + 66),
        Color::White => format!("{}", base + 67),
        Color::Indexed(i) => format!("{};5;{}", ext, i),
        Color::Rgb(r, g, b) => color_code(ext, r, g, b),
    })
}

/// Converts the first `rows` rows of a ratatui buffer into ANSI-styled lines joined by '\n'.
pub(crate) fn buffer_to_ansi(buf: &Buffer, rows: usize, color: bool) -> String {
    let mut out = String::new();
    let area = buf.area;
    for y in 0..(rows as u16).min(area.height) {
        if y > 0 {
            out.push('\n');
        }
        let mut last: Option<(Color, Color, Modifier)> = None;
        let mut skip = 0;
        for x in 0..area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let Some(cell) = buf.cell((x, y)) else { continue };
            let sym = cell.symbol();
            let w = sym.width().max(1);
            skip = w - 1;
            if color {
                let cur = (cell.fg, cell.bg, cell.modifier);
                if last != Some(cur) {
                    let mut codes: Vec<String> = vec!["0".into()];
                    if let Some(c) = sgr(cell.fg, false) {
                        codes.push(c);
                    }
                    if let Some(c) = sgr(cell.bg, true) {
                        codes.push(c);
                    }
                    if cell.modifier.contains(Modifier::BOLD) {
                        codes.push("1".into());
                    }
                    if cell.modifier.contains(Modifier::DIM) {
                        codes.push("2".into());
                    }
                    if cell.modifier.contains(Modifier::ITALIC) {
                        codes.push("3".into());
                    }
                    if cell.modifier.contains(Modifier::UNDERLINED) {
                        codes.push("4".into());
                    }
                    out.push_str(&format!("\x1b[{}m", codes.join(";")));
                    last = Some(cur);
                }
            }
            out.push_str(sym);
        }
        if color {
            out.push_str(RESET);
        }
    }
    out
}

#[cfg(test)]
mod fetch_render_tests {
    use super::{fetch_bar, fetch_detail};
    use crate::state::Fetch;

    fn text(spans: &[ratatui::text::Span]) -> String {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn detail_shares_the_unit_and_shows_percent_and_rate() {
        let mut f = Fetch::default();
        f.update(0, "", 10 << 20, 120 << 20);
        f.update(2000, "", 37 << 20, 120 << 20);
        let t = text(&fetch_detail(&f));
        assert_eq!(t, " 30%  37/120 MiB  14 MiB/s", "{t}");
        assert_eq!(text(&fetch_bar(&f, 10, 0)), "███▏      ");
    }

    #[test]
    fn aligned_detail_has_the_same_width_whatever_is_known() {
        let mut known = Fetch::default();
        known.update(0, "", 10 << 20, 120 << 20);
        known.update(2000, "", 37 << 20, 120 << 20);
        let mut unknown = Fetch::default();
        unknown.update(0, "12 MiB", 12 << 20, 0);
        let (a, b) = (text(&super::fetch_detail_aligned(&known)), text(&super::fetch_detail_aligned(&unknown)));
        assert_eq!(a.chars().count(), b.chars().count(), "{a:?} vs {b:?}");
        assert_eq!(a, " 30%     37/120 MiB    14 MiB/s");
    }

    #[test]
    fn unknown_size_gets_bytes_and_a_moving_pulse() {
        let mut f = Fetch::default();
        f.update(0, "12 MiB", 12 << 20, 0);
        assert_eq!(text(&fetch_detail(&f)), "  12 MiB");
        assert_ne!(text(&fetch_bar(&f, 10, 0)), text(&fetch_bar(&f, 10, 300)));
        assert_eq!(fetch_bar(&f, 10, 0).len(), 10);
    }

    #[test]
    fn url_labels_keep_host_and_file_name() {
        let url = "https://github.com/bazelbuild/bazel/releases/download/8.4.2/bazel-8.4.2-linux-x86_64";
        assert_eq!(super::fetch_label(url, 50), "github.com …/bazel-8.4.2-linux-x86_64");
        assert_eq!(super::fetch_label(url, 100), "github.com/bazelbuild/bazel/releases/download/8.4.2/bazel-8.4.2-linux-x86_64");
        assert_eq!(super::fetch_label(url, 20), "bazel-8.4.2-linux-x…");
        assert_eq!(super::fetch_label("repository @@foo", 40), "repository @@foo");
    }

    #[test]
    fn repository_fetch_keeps_bazels_text() {
        let mut f = Fetch::default();
        f.update(0, "Fetching repository @@rules_go", 0, 0);
        assert_eq!(text(&fetch_detail(&f)), "Fetching repository @@rules_go");
    }
}
