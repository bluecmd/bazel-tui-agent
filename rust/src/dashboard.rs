//! The full-screen dashboard (alternate screen), used when the client's terminal size is known.
//! Panels as in the original design: build header, server, parallelism and throughput graphs,
//! running actions, build tree, mnemonics, runners and the log tail.

use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, BorderType, Borders, Paragraph};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::render::{buffer_to_ansi, st, bold, dim, sparkline_levels};
use crate::state::{ActState, Node, State};
use crate::text::*;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct Dashboard {
    pub cols: u16,
    pub rows: u16,
    color: bool,
    tick: usize,
    pub show_log_big: bool,
    pub tree_by_size: bool,
    term: Terminal<TestBackend>,
}

fn panel(title: &str, right: &str) -> Block<'static> {
    let mut b = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(st(BORDER))
        .title_top(Line::from(Span::styled(format!(" {} ", title), bold(TITLE))));
    if !right.is_empty() {
        b = b.title_top(Line::from(Span::styled(format!(" {} ", right), st(MUTED))).right_aligned());
    }
    b
}

fn bar_spans(frac: f64, width: usize, stops: &[(f32, (u8, u8, u8))]) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    let cells = frac * width as f64;
    let full = cells.floor() as usize;
    (0..width)
        .map(|i| {
            let t = i as f32 / (width.max(2) - 1) as f32;
            if i < full {
                Span::styled("█", st(gradient(stops, t)))
            } else if i == full && cells - full as f64 > 0.0 {
                let blocks = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
                Span::styled(blocks[(((cells - full as f64) * 8.0) as usize).clamp(1, 8)].to_string(), st(gradient(stops, t)))
            } else {
                Span::styled("░", st((45, 50, 65)))
            }
        })
        .collect()
}

impl Dashboard {
    pub fn new(cols: u16, rows: u16, color: bool) -> Dashboard {
        Dashboard { cols, rows, color, tick: 0, show_log_big: false, tree_by_size: false, term: Terminal::new(TestBackend::new(cols, rows)).expect("backend") }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.term.backend_mut().resize(cols, rows);
            self.term.clear().ok();
        }
    }

}

fn graph_bars(hist: &[f32], width: usize, vmax: f32, stops: &[(f32, (u8, u8, u8))]) -> Vec<Bar<'static>> {
        let levels = sparkline_levels(hist, width.max(1), vmax); // 0..8 per column, folded over the whole build
        let mut bars: Vec<Bar> = Vec::with_capacity(width);
        for _ in 0..width.saturating_sub(levels.len()) {
            bars.push(Bar::default().value(0).text_value(String::new()));
        }
        for l in levels {
            bars.push(Bar::default().value(l).text_value(String::new()).style(st(gradient(stops, l as f32 / 8.0))));
        }
        bars
}

impl Dashboard {
    /// Full frame as `rows` lines of `cols` columns.
    pub fn render(&mut self, s: &State, log: &[String], now: i64) -> Vec<String> {
        self.tick += 1;
        let spin = SPINNER[self.tick % SPINNER.len()];
        let elapsed = fmt_elapsed((now - s.t0_ms) as f64 / 1000.0);

        // ---- header content
        let targets: Vec<&str> = s.args.iter().map(|x| x.as_str()).take(4).collect();
        let state_span = match s.build_success {
            Some(true) => Span::styled("✔ success", bold(OK)),
            Some(false) => Span::styled("✘ failed", bold(BAD)),
            None => Span::styled(format!("{} {}", spin, s.phase), bold(ACCENT)),
        };
        let line1 = Line::from(vec![
            Span::styled(s.command.clone().unwrap_or_else(|| "bazel".into()), bold(ACCENT)),
            Span::raw(" "),
            Span::styled(format!("{}{}", targets.join(" "), if s.args.len() > 4 { " …" } else { "" }), st(TITLE)),
            Span::raw("   "),
            state_span,
        ]);
        let counts = s.exec_counts();
        let mut line2: Vec<Span> = Vec::new();
        if let Some((done, total)) = counts {
            let frac = if total > 0 { done as f64 / total as f64 } else { 0.0 };
            let barw = (self.cols as usize).saturating_sub(self.cols as usize / 4 + 44).clamp(10, 80);
            line2.extend(bar_spans(frac, barw, HEAT));
            line2.push(Span::raw(format!("  {}/{} ", with_commas(done), with_commas(total))));
            line2.push(Span::styled(format!("{:3}%", (frac * 100.0) as u32), bold(TITLE)));
            if let Some(eta) = s.eta_secs(now) {
                if eta >= 5.0 {
                    line2.push(Span::styled(format!("  ~{} left", fmt_duration(eta)), st(MUTED)));
                }
            }
        } else if let (Some(l), false) = (&s.loading, s.analysis_done) {
            line2.push(Span::styled("loading  ", st(MUTED)));
            line2.push(Span::raw(l.clone()));
            if let Some(d) = &s.loading_detail {
                line2.push(Span::styled(format!("  {}", d), dim()));
            }
        } else if let Some(a) = &s.analysis {
            line2.push(Span::styled("analysis  ", st(MUTED)));
            line2.push(Span::raw(a.clone()));
        } else {
            line2.push(Span::styled("waiting for the server…", st(MUTED)));
        }
        let mut line3: Vec<Span> = Vec::new();
        for (i, p) in ["loading", "analysis", "execution"].iter().enumerate() {
            if i > 0 {
                line3.push(Span::raw("  "));
            }
            match s.phase_t.get(*p) {
                None => line3.push(Span::styled(format!("{} ·", p), st(MUTED))),
                Some(&t0) => {
                    let later = ["loading", "analysis", "execution", "finished"].iter().skip_while(|x| *x != p).skip(1).filter_map(|q| s.phase_t.get(*q)).max().copied();
                    let done_at = if *p == "analysis" && s.analysis_done { later.or(Some(now)) } else { later };
                    match done_at {
                        Some(te) if te >= t0 && *p != "execution" => line3.push(Span::styled(format!("{} ✓{}", p, fmt_duration((te - t0) as f64 / 1000.0)), st(OK))),
                        Some(te) if te >= t0 && s.build_success.is_some() => line3.push(Span::styled(format!("{} ✓{}", p, fmt_duration((te - t0) as f64 / 1000.0)), st(OK))),
                        _ => line3.push(Span::styled(format!("{} ▸{}", p, fmt_duration((now - t0) as f64 / 1000.0)), bold(WARN))),
                    }
                }
            }
        }
        if s.analysis.is_some() && counts.is_some() {
            line3.push(Span::styled("  skymeld", st(MUTED)));
        }
        let mut badges: Vec<Span> = Vec::new();
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
        }

        // ---- server panel content
        let mut server: Vec<Line> = Vec::new();
        let sw = (self.cols as usize / 4).clamp(30, 40);
        let sbar = sw.saturating_sub(24).max(6);
        if s.heap_max_mb > 0 {
            let mut l = vec![Span::styled("heap ", st(MUTED))];
            l.extend(bar_spans(s.heap_used_mb as f64 / s.heap_max_mb as f64, sbar, COOL));
            l.push(Span::raw(format!(" {:.1}/{}G", s.heap_used_mb as f64 / 1024.0, s.heap_max_mb / 1024)));
            server.push(Line::from(l));
        } else {
            server.push(Line::from(Span::styled("heap …", st(MUTED))));
        }
        if s.cpus > 0 {
            let mut l = vec![Span::styled("load ", st(MUTED))];
            l.extend(bar_spans(s.load / s.cpus as f64, sbar, HEAT));
            l.push(Span::raw(format!(" {:.1}/{}", s.load, s.cpus)));
            server.push(Line::from(l));
        } else {
            server.push(Line::from(Span::styled("load …", st(MUTED))));
        }
        if s.spawns > 0 {
            let hit = s.cache_hits as f64 / s.spawns as f64;
            let mut l = vec![Span::styled("cache", st(MUTED)), Span::raw(" ")];
            l.extend(bar_spans(hit, sbar, &[(0.0, (120, 120, 160)), (1.0, OK)]));
            l.push(Span::raw(format!(" {}% of {}", (hit * 100.0) as u32, s.spawns)));
            server.push(Line::from(l));
        } else {
            server.push(Line::from(Span::styled("cache no spawns yet", st(MUTED))));
        }

        // ---- running panel content
        let acts = s.running_sorted();
        let running_now = s.running_count();
        let mut running_lines: Vec<(Vec<Span>, Vec<Span>)> = Vec::new();
        for a in &acts {
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
            let left = vec![Span::raw(" "), mark, Span::styled(format!("{:<11} ", a.mnemonic.chars().take(11).collect::<String>()), bold(mnemonic_rgb(&a.mnemonic))), Span::raw(body)];
            let extra = a.progress.clone().unwrap_or_else(|| match a.state { ActState::Run => a.strategy.clone(), ActState::Cache => "cache".into(), ActState::Sched => "scheduling".into(), ActState::Queued => "queued".into() });
            let dur = if a.state == ActState::Run { fmt_duration(age) } else { String::new() };
            let right = vec![
                Span::styled(format!("{:>16} ", extra.chars().take(16).collect::<String>()), st(MUTED)),
                Span::styled(format!("{:>6}", dur), st(gradient(HEAT, (age / 60.0).min(1.0) as f32))),
            ];
            running_lines.push((left, right));
        }

        // ---- mnemonics and runners content
        let mut items: Vec<(&String, &(u64, u64))> = s.mnemonic_stats.iter().collect();
        items.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        let top = items.first().map(|i| i.1 .0).unwrap_or(1).max(1);
        let mut runners: Vec<(&String, &u64)> = s.runners.iter().collect();
        runners.sort_by(|a, b| b.1.cmp(a.1));

        // ---- layout
        let tree_mode = self.tree_by_size;
        let color = self.color;
        let total_rows = self.rows as usize;
        let log_big = self.show_log_big;
        self.term
            .draw(|f| {
                let area = f.area();
                let hint_h: u16 = 1;
                let head_h: u16 = 5;
                let graph_h: u16 = if area.height >= 30 { (area.height as usize / 5).clamp(6, 9) as u16 } else { 0 };
                let bottom_h: u16 = if log_big { 0 } else { ((area.height as usize - head_h as usize - graph_h as usize - hint_h as usize) / 3).clamp(6, 10) as u16 };
                let rows_c = Layout::vertical([
                    Constraint::Length(head_h),
                    Constraint::Length(graph_h),
                    Constraint::Fill(1),
                    Constraint::Length(bottom_h),
                    Constraint::Length(hint_h),
                ])
                .split(area);

                // header + server
                let server_w = (area.width / 4).clamp(30, 40);
                let hs = Layout::horizontal([Constraint::Fill(1), Constraint::Length(server_w)]).split(rows_c[0]);
                let hb = panel("build", "");
                let hinner = hb.inner(hs[0]);
                f.render_widget(hb, hs[0]);
                let hl = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)]).split(hinner);
                let el = Layout::horizontal([Constraint::Fill(1), Constraint::Length(elapsed.width() as u16 + 1)]).split(hl[0]);
                f.render_widget(Paragraph::new(line1.clone()), el[0]);
                f.render_widget(Paragraph::new(Span::styled(elapsed.clone(), bold(TITLE))).right_aligned(), el[1]);
                f.render_widget(Paragraph::new(Line::from(line2.clone())), hl[1]);
                let bw: usize = badges.iter().map(|b| b.content.width()).sum();
                let bl = Layout::horizontal([Constraint::Fill(1), Constraint::Length(bw as u16 + 1)]).split(hl[2]);
                f.render_widget(Paragraph::new(Line::from(line3.clone())), bl[0]);
                f.render_widget(Paragraph::new(Line::from(badges.clone())).right_aligned(), bl[1]);
                let sb = panel("server", "");
                let sinner = sb.inner(hs[1]);
                f.render_widget(sb, hs[1]);
                f.render_widget(Paragraph::new(server.clone()), sinner);

                // graphs
                if graph_h > 0 {
                    let gs = Layout::horizontal([Constraint::Percentage(50), Constraint::Fill(1)]).split(rows_c[1]);
                    let vmax = s.peak_running.max(s.cpus).max(1) as f32;
                    let gb = panel("parallelism", &format!("{}  scale {}", running_now, vmax as u32));
                    let gi = gb.inner(gs[0]);
                    f.render_widget(gb, gs[0]);
                    let hist: Vec<f32> = s.history_running.iter().map(|x| *x as f32).collect();
                    let bars = graph_bars(&hist, gi.width as usize, vmax, HEAT);
                    f.render_widget(BarChart::default().data(BarGroup::default().bars(&bars)).bar_width(1).bar_gap(0).max(8), gi);
                    let tp_scale = {
                        let mut v: Vec<f32> = s.history_tp.iter().rev().take(gi.width as usize).copied().collect();
                        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        if v.is_empty() { 1.0 } else { (v[((v.len() as f32 * 0.95) as usize).min(v.len() - 1)] * 1.15).max(1.0) }
                    };
                    let tp = s.history_tp.last().copied().unwrap_or(0.0);
                    let tb = panel("throughput", &format!("{:.1} act/s  scale {:.0}", tp, tp_scale));
                    let ti = tb.inner(gs[1]);
                    f.render_widget(tb, gs[1]);
                    let bars = graph_bars(&s.history_tp, ti.width as usize, tp_scale, COOL);
                    f.render_widget(BarChart::default().data(BarGroup::default().bars(&bars)).bar_width(1).bar_gap(0).max(8), ti);
                }

                // middle: running + tree, or the big log
                if log_big {
                    let lb = panel("log", &format!("{} lines  [l]", log.len()));
                    let li = lb.inner(rows_c[2]);
                    f.render_widget(lb, rows_c[2]);
                    let tail: Vec<Line> = log.iter().rev().take(li.height as usize).rev().map(|l| Line::from(ansi_spans(l, color))).collect();
                    f.render_widget(Paragraph::new(tail), li);
                } else {
                    let tree_w = ((area.width as f32 * 0.4) as u16).max(34);
                    let ms = Layout::horizontal([Constraint::Fill(1), Constraint::Length(tree_w)]).split(rows_c[2]);
                    let rb = panel(&format!("running {}", running_now), &format!("{} in flight · peak {}", acts.len(), s.peak_running));
                    let ri = rb.inner(ms[0]);
                    f.render_widget(rb, ms[0]);
                    let rrows = Layout::vertical(vec![Constraint::Length(1); ri.height as usize]).split(ri);
                    let shown = running_lines.len().min(ri.height as usize);
                    for (i, (left, right)) in running_lines.iter().take(shown).enumerate() {
                        if i + 1 == shown && running_lines.len() > shown {
                            f.render_widget(Paragraph::new(Span::styled(format!("   … and {} more", running_lines.len() - shown + 1), st(MUTED))), rrows[i]);
                            break;
                        }
                        let rw = right.iter().map(|sp| sp.content.width()).sum::<usize>() as u16;
                        let lr = Layout::horizontal([Constraint::Fill(1), Constraint::Length(rw + 1)]).split(rrows[i]);
                        f.render_widget(Paragraph::new(Line::from(left.clone())), lr[0]);
                        f.render_widget(Paragraph::new(Line::from(right.clone())).right_aligned(), lr[1]);
                    }
                    let tb = panel("build tree", &format!("by {}  [t]", if tree_mode { "size" } else { "active" }));
                    let ti = tb.inner(ms[1]);
                    f.render_widget(tb, ms[1]);
                    let lines = tree_lines(s, now, ti.height as usize, ti.width as usize, tree_mode);
                    f.render_widget(Paragraph::new(lines), ti);
                }

                // bottom: mnemonics, runners, log
                if bottom_h > 0 {
                    let mn_w = ((area.width as f32 * 0.3) as u16).max(34);
                    let rn_w = ((area.width as f32 * 0.22) as u16).max(26);
                    let bs = Layout::horizontal([Constraint::Length(mn_w), Constraint::Length(rn_w), Constraint::Fill(1)]).split(rows_c[3]);
                    let mb = panel("mnemonics", &format!("{} done", s.completed));
                    let mi = mb.inner(bs[0]);
                    f.render_widget(mb, bs[0]);
                    let namew = if mi.width >= 44 { 16 } else { 12 };
                    let show_avg = mi.width >= 40;
                    let barw = (mi.width as usize).saturating_sub(namew + 1 + 6 + if show_avg { 7 } else { 0 }).max(2);
                    let mlines: Vec<Line> = items.iter().take(mi.height as usize).map(|(m, (c, ms))| {
                        let mut l = vec![Span::styled(format!("{:<w$} ", m.chars().take(namew).collect::<String>(), w = namew), bold(mnemonic_rgb(m)))];
                        let col = mnemonic_rgb(m);
                        l.extend(bar_spans(*c as f64 / top as f64, barw, &[(0.0, col), (1.0, col)]));
                        l.push(Span::raw(format!("{:>6}", c)));
                        if show_avg {
                            let avg = if *c > 0 { *ms as f64 / *c as f64 / 1000.0 } else { 0.0 };
                            l.push(Span::styled(format!("{:>7}", format!("{:.1}s", avg)), st(MUTED)));
                        }
                        Line::from(l)
                    }).collect();
                    f.render_widget(Paragraph::new(mlines), mi);
                    let rbk = panel("runners", &format!("{} remote", s.remote));
                    let rni = rbk.inner(bs[1]);
                    f.render_widget(rbk, bs[1]);
                    let total = s.spawns.max(1) as f64;
                    let rbarw = (rni.width as usize).saturating_sub(16 + 5).max(4);
                    let mut rlines: Vec<Line> = runners.iter().take(rni.height as usize).map(|(name, n)| {
                        let mut l = vec![Span::styled(format!("{:<15} ", name.chars().take(15).collect::<String>()), st(TITLE))];
                        l.extend(bar_spans(**n as f64 / total, rbarw, COOL));
                        l.push(Span::raw(format!("{:>4}%", (**n as f64 / total * 100.0) as u32)));
                        Line::from(l)
                    }).collect();
                    if !s.hosts.is_empty() && rlines.len() < rni.height as usize {
                        rlines.push(Line::from(Span::styled(format!("{} remote hosts", s.hosts.len()), st(MUTED))));
                    }
                    f.render_widget(Paragraph::new(rlines), rni);
                    let lb = panel("log", &format!("{} lines  [l]", log.len()));
                    let li = lb.inner(bs[2]);
                    f.render_widget(lb, bs[2]);
                    let tail: Vec<Line> = log.iter().rev().take(li.height as usize).rev().map(|l| Line::from(ansi_spans(l, color))).collect();
                    f.render_widget(Paragraph::new(tail), li);
                }

                // hint
                let hint = Line::from(vec![
                    Span::styled(" t ", bold(ACCENT)), Span::styled("tree order  ", st(MUTED)),
                    Span::styled(" l ", bold(ACCENT)), Span::styled("log  ", st(MUTED)),
                    Span::styled(" ^C ", bold(ACCENT)), Span::styled("cancel", st(MUTED)),
                ]);
                f.render_widget(Paragraph::new(hint), rows_c[4]);
            })
            .ok();
        let _ = total_rows;
        let text = buffer_to_ansi(self.term.backend().buffer(), self.rows as usize, self.color);
        text.split('\n').map(|l| l.to_string()).collect()
    }
}

/// Strips ANSI escapes from a captured log line (the panel applies its own styling).
fn ansi_spans(line: &str, _color: bool) -> Vec<Span<'static>> {
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
    let style = if out.starts_with("ERROR:") {
        bold(BAD)
    } else if out.starts_with("WARNING:") {
        st(WARN)
    } else if out.starts_with("INFO:") {
        st(OK)
    } else {
        Style::new()
    };
    vec![Span::styled(out, style)]
}

/// The tree panel: repositories/packages with live and recently finished actions, most active first.
fn tree_lines(s: &State, now: i64, height: usize, width: usize, by_size: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    fn key(n: &Node, by_size: bool) -> (i64, i64, i64) {
        if by_size { (-(n.done as i64 + n.running), -n.last_ms, 0) } else { (-n.running, -n.last_ms, -(n.done as i64)) }
    }
    fn walk(s: &State, lines: &mut Vec<Line<'static>>, n: &Node, mut name: String, prefix: String, depth: usize, is_last: bool, now: i64, height: usize, width: usize, by_size: bool) {
        if lines.len() >= height {
            return;
        }
        let mut node = n;
        let mut kids: Vec<(&String, &Node)> = node.children.iter().collect();
        kids.sort_by_key(|(_, k)| key(k, by_size));
        while kids.len() == 1 && node.action_ids.is_empty() && !kids[0].1.children.is_empty() {
            let (cn, child) = kids[0];
            name = if name.is_empty() { cn.clone() } else { format!("{}/{}", name, cn) };
            node = child;
            kids = node.children.iter().collect();
            kids.sort_by_key(|(_, k)| key(k, by_size));
        }
        let active = node.running > 0 || now - node.last_ms < 4000;
        let branch = if depth == 0 { String::new() } else { format!("{}{}", prefix, if is_last { "└─ " } else { "├─ " }) };
        let name_style = if node.running > 0 { bold(TITLE) } else if node.failed > 0 { st(BAD) } else if active { st(OK) } else { st(MUTED) };
        let mut right: Vec<Span> = Vec::new();
        if let Some(a) = node.action_ids.iter().filter_map(|id| s.actions.get(id)).filter(|a| a.state == ActState::Run).min_by_key(|a| a.t_ms) {
            right.push(Span::styled(a.mnemonic.clone(), st(mnemonic_rgb(&a.mnemonic))));
            right.push(Span::styled(format!(" {}  ", fmt_duration((now - a.t_ms).max(0) as f64 / 1000.0)), st(WARN)));
        }
        if node.running > 0 {
            right.push(Span::styled(format!("▸{} ", node.running), bold(OK)));
        }
        if node.done > 0 {
            right.push(Span::styled(format!("✓{} ", node.done), st(MUTED)));
        }
        if node.failed > 0 {
            right.push(Span::styled(format!("✗{}", node.failed), bold(BAD)));
        }
        let rw: usize = right.iter().map(|x| x.content.width()).sum();
        let lw = branch.width() + name.width();
        let pad = width.saturating_sub(lw + rw).max(1);
        let mut spans = vec![Span::styled(branch.clone(), st(BORDER)), Span::styled(name.clone(), name_style), Span::raw(" ".repeat(pad))];
        spans.extend(right);
        lines.push(Line::from(spans));
        if !active || depth >= 6 {
            return;
        }
        let shown: Vec<(&String, &Node)> = kids.into_iter().filter(|(_, k)| k.running > 0 || k.done > 0 || k.failed > 0).collect();
        let n_shown = shown.len();
        for (i, (cn, k)) in shown.into_iter().enumerate() {
            if lines.len() >= height {
                break;
            }
            let p = if depth == 0 { "   ".to_string() } else if is_last { format!("{}   ", prefix) } else { format!("{}│  ", prefix) };
            walk(s, lines, k, cn.clone(), p, depth + 1, i + 1 == n_shown, now, height, width, by_size);
        }
    }
    let mut tops: Vec<(&String, &Node)> = s.tree.children.iter().collect();
    tops.sort_by_key(|(_, k)| key(k, by_size));
    let n = tops.len();
    for (i, (name, node)) in tops.into_iter().enumerate() {
        if lines.len() >= height {
            break;
        }
        walk(s, &mut lines, node, name.clone(), String::new(), 0, i + 1 == n, now, height, width, by_size);
    }
    if lines.len() >= height && height > 0 {
        lines[height - 1] = Line::from(Span::styled("   …", st(MUTED)));
    }
    lines
}
