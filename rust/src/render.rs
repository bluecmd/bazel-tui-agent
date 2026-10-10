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

use crate::state::{ActState, State};
use crate::text::*;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const BLOCKS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub(crate) fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
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
        let frac = frac.clamp(0.0, 1.0);
        let cells = frac * width as f64;
        let full = cells.floor() as usize;
        (0..width)
            .map(|i| {
                let t = i as f32 / (width.max(2) - 1) as f32;
                if i < full {
                    Span::styled("█", st(gradient(HEAT, t)))
                } else if i == full && cells - full as f64 > 0.0 {
                    Span::styled(BLOCKS[(((cells - full as f64) * 8.0) as usize).clamp(1, 8)].to_string(), st(gradient(HEAT, t)))
                } else {
                    Span::styled("░", st((45, 50, 65)))
                }
            })
            .collect()
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
        let mut title = vec![
            Span::styled(format!(" {} ", spin), st(ACCENT)),
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
        let mut progress_right: Vec<Span> = Vec::new();
        if let Some((done, total)) = counts {
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
            if s.spawns > 0 {
                right.push_str(&format!(" · {}% cache", s.cache_hits * 100 / s.spawns));
            }
            if s.remote > 0 {
                right.push_str(&format!(" · {} remote", s.remote));
            }
            progress_right.push(Span::styled(right, st(MUTED)));
        } else if let (Some(l), false) = (&s.loading, s.analysis_done) {
            progress.push(Span::styled("loading ", st(MUTED)));
            progress.push(Span::raw(l.clone()));
            if let Some(d) = &s.loading_detail {
                progress.push(Span::styled(format!("  {}", d), dim()));
            }
        } else if let Some(a) = &s.analysis {
            progress.push(Span::styled("analysis ", st(MUTED)));
            progress.push(Span::raw(a.clone()));
        } else if !s.fetches.is_empty() {
            let (res, prog) = s.fetches.iter().next().unwrap();
            progress.push(Span::styled(format!("fetching {} ", s.fetches.len()), st(MUTED)));
            progress.push(Span::raw(short_label(res, 50)));
            progress.push(Span::styled(format!("  {}", prog), dim()));
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
        if let Some((res, prog)) = s.fetches.iter().next() {
            server.push(Span::styled("  ↓ ", st(ACCENT)));
            server.push(Span::raw(short_label(res, 30)));
            server.push(Span::styled(format!(" {}", prog), dim()));
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
        Color::Rgb(r, g, b) => format!("{};5;{}", ext, cube(r, g, b)),
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
