//! The full-screen dashboard (alternate screen), used when the client's terminal size is known.
//! Panels as in the original design: build header, server, parallelism and throughput graphs,
//! running actions, build tree, mnemonics, cache sources and the log tail.

use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, BorderType, Borders, Paragraph};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::render::{bar_spans, buffer_to_ansi, download_spans, fetch_bar, fetch_detail, fetch_label, st, bold, dim, sparkline_levels};
use crate::state::{ActState, Node, Source, State};
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

/// Border colour for every panel: the usual blue-grey, or orange while a cancel is in progress.
static ABORTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn border() -> (u8, u8, u8) {
    if ABORTING.load(std::sync::atomic::Ordering::Relaxed) { ABORT } else { BORDER }
}

fn panel(title: &str, right: &str) -> Block<'static> {
    let mut b = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(st(border()))
        .title_top(Line::from(Span::styled(format!(" {} ", title), bold(TITLE))));
    if !right.is_empty() {
        b = b.title_top(Line::from(Span::styled(format!(" {} ", right), st(MUTED))).right_aligned());
    }
    b
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
        ABORTING.store(s.cancelling(), std::sync::atomic::Ordering::Relaxed);
        let state_span = match s.build_success {
            Some(true) => Span::styled("✔ success", bold(OK)),
            Some(false) => Span::styled("✘ failed", bold(BAD)),
            None if s.cancelling() => Span::styled(format!("{} ABORTING", spin), bold(ABORT)),
            None if s.analysis_live() => Span::styled(format!("{} analysis", spin), bold(ACCENT)),
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
        if s.cancelling() {
            let secs = (now - s.cancelled_ms.unwrap_or(now)) / 1000;
            line2.push(Span::styled("Ctrl-C received ", bold(ABORT)));
            line2.push(Span::styled(
                format!("· stopping {} running action{} and unwinding, {}s so far · a third Ctrl-C kills the server (slow next start)", s.running_count(), if s.running_count() == 1 { "" } else { "s" }, secs),
                st(ABORT),
            ));
        } else if s.analysis_live() {
            // Skymeld: analysis and execution overlap; the analysis counter is the one moving.
            line2.push(Span::styled("analysis  ", st(MUTED)));
            line2.push(Span::raw(s.analysis.clone().unwrap_or_default()));
            if let Some((done, total)) = counts {
                if total > 1 {
                    line2.push(Span::styled(format!("  · executing {}/{}", with_commas(done), with_commas(total)), st(MUTED)));
                }
            }
            let room = (self.cols as usize).saturating_sub(70);
            if let Some(dl) = download_spans(s, (room / 3).clamp(12, 40), (room / 5).clamp(8, 24), now) {
                // A download in flight is the activity; Bazel's "Downloading …: N bytes" text is
                // the same information without the bar.
                line2.push(Span::raw("  "));
                line2.extend(dl);
            } else if !s.activity.is_empty() && room > 12 {
                line2.push(Span::styled(format!("  {}", s.activity.chars().take(room).collect::<String>()), dim()));
            }
        } else if let Some((done, total)) = counts {
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
            let room = (self.cols as usize).saturating_sub(40);
            if let Some(dl) = download_spans(s, (room / 3).clamp(12, 40), (room / 5).clamp(8, 24), now) {
                line2.push(Span::raw("  "));
                line2.extend(dl);
            } else if let Some(d) = &s.loading_detail {
                line2.push(Span::styled(format!("  {}", d), dim()));
            }
        } else if let Some(a) = &s.analysis {
            line2.push(Span::styled("analysis  ", st(MUTED)));
            line2.push(Span::raw(a.clone()));
            let room = (self.cols as usize).saturating_sub(40);
            if let Some(dl) = download_spans(s, (room / 3).clamp(12, 40), (room / 5).clamp(8, 24), now) {
                line2.push(Span::raw("  "));
                line2.extend(dl);
            }
        } else if let Some((res, f)) = crate::render::downloads(s)
            .first()
            .copied()
            .or_else(|| s.fetches.iter().find(|(_, f)| !f.progress.is_empty() && f.progress != "starting"))
        {
            line2.push(Span::styled("fetching  ", st(MUTED)));
            if f.is_download() {
                let barw = (self.cols as usize / 5).clamp(10, 40);
                line2.push(Span::raw(fetch_label(res, (self.cols as usize).saturating_sub(barw + 50).clamp(20, 60))));
                line2.push(Span::raw("  "));
                line2.extend(fetch_bar(f, barw, now));
                line2.push(Span::raw(" "));
            } else {
                line2.push(Span::raw(short_label(res, 60)));
                line2.push(Span::raw("  "));
            }
            line2.extend(fetch_detail(f));
        } else if !s.activity.is_empty() {
            line2.push(Span::raw(s.activity.clone()));
        } else {
            line2.push(Span::styled("starting…", st(MUTED)));
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
        if let Some(hit) = s.hit_rate() {
            let mut l = vec![Span::styled("cache", st(MUTED)), Span::raw(" ")];
            l.extend(bar_spans(hit, sbar, &[(0.0, (120, 120, 160)), (1.0, OK)]));
            l.push(Span::raw(format!(" {}% of {}", (hit * 100.0) as u32, s.cache_total())));
            server.push(Line::from(l));
        } else {
            server.push(Line::from(Span::styled("cache no actions yet", st(MUTED))));
        }

        // ---- running panel content (downloads take its place until actions start)
        let acts = s.running_sorted();
        let running_now = s.running_count();
        let mut running_lines: Vec<(Vec<Span>, Vec<Span>)> = Vec::new();
        // Downloads and repository fetches are listed first; they are the activity while actions wait.
        let fetching = !s.fetches.is_empty();
        if fetching {
            let barw = (self.cols as usize / 8).clamp(8, 24);
            for (res, f) in s.fetches.iter() {
                let left = vec![Span::raw(" "), Span::styled("↓ ", st(ACCENT)), Span::raw(fetch_label(res, 70))];
                let mut right = Vec::new();
                if f.is_download() {
                    right.extend(fetch_bar(f, barw, now));
                    right.push(Span::raw(" "));
                }
                right.extend(fetch_detail(f));
                running_lines.push((left, right));
            }
        }
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
        let mut runners: Vec<(&String, &u64)> = s.runners.iter().filter(|(n, _)| Source::of_spawn(n, false, false) == Source::LocalExec).collect();
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
                let hb = panel(if s.cancelling() { "aborting" } else { "build" }, "");
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
                    let rb = if fetching {
                        panel(&format!("fetching {} · running {}", s.fetches.len(), running_now), &format!("{} in flight · peak {}", acts.len(), s.peak_running))
                    } else {
                        panel(&format!("running {}", running_now), &format!("{} in flight · peak {}", acts.len(), s.peak_running))
                    };
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
                    let rn_w = ((area.width as f32 * 0.25) as u16).max(26);
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
                    let cb = panel("cache", &format!("{} actions", with_commas(s.cache_total())));
                    let ci = cb.inner(bs[1]);
                    f.render_widget(cb, bs[1]);
                    f.render_widget(Paragraph::new(cache_lines(s, &runners, ci.width as usize, ci.height as usize)), ci);
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

/// Colour of each action source in the cache panel (same order as `Source::ALL`).
pub fn source_rgb(src: Source) -> (u8, u8, u8) {
    match src {
        Source::ActionCache => OK,
        Source::RemoteCache => ACCENT,
        Source::DiskCache => (120, 200, 255),
        Source::RemoteExec => WARN,
        Source::LocalExec => (220, 140, 255),
    }
}

const HIT_GRADIENT: &[(f32, (u8, u8, u8))] = &[(0.0, BAD), (0.5, WARN), (1.0, OK)];

/// Splits `width` cells between the sources in proportion to their counts; every non-zero
/// source gets at least one cell when there is room.
fn stacked_widths(counts: &[u64], width: usize) -> Vec<usize> {
    let total: u64 = counts.iter().sum();
    if total == 0 || width == 0 {
        return vec![0; counts.len()];
    }
    let mut widths = Vec::with_capacity(counts.len());
    let (mut cum, mut prev) = (0u64, 0usize);
    for c in counts {
        cum += c;
        let b = ((cum as f64 / total as f64) * width as f64).round() as usize;
        widths.push(b.saturating_sub(prev));
        prev = b;
    }
    for i in 0..counts.len() {
        if counts[i] > 0 && widths[i] == 0 {
            if let Some(j) = (0..counts.len()).filter(|j| widths[*j] > 1).max_by_key(|j| widths[*j]) {
                widths[j] -= 1;
                widths[i] = 1;
            }
        }
    }
    widths
}

/// One-row sparkline of the hit rate: each column folds a slice of the history (mean of the
/// windows that had actions); a column without any action stays blank.
fn hit_sparkline(hist: &[f32], cols: usize) -> Vec<Span<'static>> {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if hist.is_empty() || cols == 0 {
        return Vec::new();
    }
    let per = (hist.len() as f32 / cols as f32).max(1.0);
    let mut out = Vec::with_capacity(cols);
    let mut i = 0.0f32;
    while (i as usize) < hist.len() && out.len() < cols {
        let end = ((i + per) as usize).min(hist.len()).max(i as usize + 1);
        let active: Vec<f32> = hist[i as usize..end].iter().copied().filter(|v| *v >= 0.0).collect();
        if active.is_empty() {
            out.push(Span::styled("·", st((45, 50, 65))));
        } else {
            let rate = active.iter().sum::<f32>() / active.len() as f32;
            out.push(Span::styled(BLOCKS[((rate * 7.0).round() as usize).min(7)].to_string(), st(gradient(HIT_GRADIENT, rate))));
        }
        i += per;
    }
    out
}

/// The cache panel: a stacked bar of where actions came from, the hit rate, one legend line
/// per source, the hit rate over time and (with room) the local runners.
fn cache_lines(s: &State, local_runners: &[(&String, &u64)], width: usize, height: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if height == 0 {
        return lines;
    }
    let total = s.cache_total();
    let counts: Vec<u64> = Source::ALL.iter().map(|src| s.source_count(*src)).collect();

    // Row 0: stacked bar + bold hit rate.
    let pct = match s.hit_rate() {
        Some(r) => format!("{:>3}%", (r * 100.0).round() as u32),
        None => "  —".to_string(),
    };
    let barw = width.saturating_sub(pct.width() + 1).max(4);
    let mut bar: Vec<Span> = Vec::new();
    if total == 0 {
        bar.push(Span::styled("░".repeat(barw), st((45, 50, 65))));
    } else {
        for (src, w) in Source::ALL.iter().zip(stacked_widths(&counts, barw)) {
            if w > 0 {
                bar.push(Span::styled("█".repeat(w), st(source_rgb(*src))));
            }
        }
    }
    bar.push(Span::raw(" "));
    bar.push(Span::styled(pct, bold(if s.hit_rate().unwrap_or(0.0) >= 0.5 { OK } else { TITLE })));
    lines.push(Line::from(bar));

    // Legend: only sources seen so far, in a fixed order so colours stay put.
    let seen: Vec<(Source, u64)> = Source::ALL.iter().copied().zip(counts.iter().copied()).filter(|(_, c)| *c > 0).collect();
    if seen.is_empty() {
        if height > 1 {
            lines.push(Line::from(Span::styled("no actions yet", st(MUTED))));
        }
        return lines;
    }
    // Legend first; the sparkline only when every source still fits above it.
    let show_sparkline = height >= 2 + seen.len() && !s.history_hit.is_empty();
    let legend_rows = height - 1 - usize::from(show_sparkline);
    let show_count = width >= 27;
    for (i, (src, c)) in seen.iter().enumerate() {
        if i >= legend_rows {
            break;
        }
        let rest = seen.len() - i;
        let mut l = vec![Span::styled("■ ", st(source_rgb(*src)))];
        if i + 1 == legend_rows && rest > 1 {
            // Fold the remaining sources into one line rather than hiding them.
            let other: u64 = seen[i..].iter().map(|(_, c)| c).sum();
            l.push(Span::styled(format!("{:<12}", format!("{} more", rest)), st(MUTED)));
            l.push(Span::styled(format!("{:>4}%", other * 100 / total.max(1)), st(MUTED)));
            lines.push(Line::from(l));
            break;
        }
        l.push(Span::styled(format!("{:<12}", src.name()), st(if src.is_hit() { TITLE } else { MUTED })));
        if show_count {
            l.push(Span::raw(format!("{:>6}", with_commas(*c))));
        }
        l.push(Span::styled(format!("{:>4}%", c * 100 / total.max(1)), st(source_rgb(*src))));
        lines.push(Line::from(l));
    }
    if show_sparkline && lines.len() < height {
        // The window in progress counts too, so the strip moves as actions come in.
        let mut hist = s.history_hit.clone();
        let (dh, dt) = (s.cache_hit_count() - s.hit_last.1, total - s.hit_last.2);
        if dt > 0 {
            hist.push(dh as f32 / dt as f32);
        }
        let mut l = vec![Span::styled("rate ", st(MUTED))];
        l.extend(hit_sparkline(&hist, width.saturating_sub(5)));
        lines.push(Line::from(l));
    }
    // Spare rows: how local execution split between runners (sandbox, worker, local ...).
    if lines.len() < height && !local_runners.is_empty() {
        let local: u64 = local_runners.iter().map(|(_, n)| **n).sum::<u64>().max(1);
        let parts: Vec<String> = local_runners.iter().take(3).map(|(n, c)| format!("{} {}%", n.chars().take(14).collect::<String>(), **c * 100 / local)).collect();
        lines.push(Line::from(Span::styled(format!("local: {}", parts.join(" · ")).chars().take(width).collect::<String>(), dim())));
    }
    if lines.len() < height && !s.hosts.is_empty() {
        lines.push(Line::from(Span::styled(format!("{} remote hosts", s.hosts.len()), dim())));
    }
    lines
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ActionRef, Event, Stamped};

    fn spawn(runner: &str, cache_hit: bool, remote: bool) -> Event {
        Event::Spawn {
            id: "x".into(), mnemonic: "Genrule".into(), runner: runner.into(), runner_subtype: String::new(), host: String::new(),
            status: "SUCCESS".into(), exit_code: 0, cache_hit, remote, wall_ms: 10,
        }
    }

    fn synthetic() -> State {
        let mut s = State::default();
        let mut t = 1_000_000i64;
        let mut push = |s: &mut State, ev: Event| {
            t += 300;
            s.apply(&Stamped { t_ms: t, event: ev });
            s.sample(t);
        };
        push(&mut s, Event::CommandStarted { command: "build".into(), command_id: String::new(), workspace: String::new(), cwd: String::new(), args: vec!["//...".into()], start_ms: 1_000_000, bazel_version: String::new(), agent_jar: String::new(), agent_jar_mtime_ms: 0, exec_root: String::new(), show_result: 1, test_summary: "short".into() });
        push(&mut s, Event::BuildStarted { targets: vec![], build_id: String::new() });
        for i in 0..40 {
            let ev = match i % 8 {
                0 | 1 | 2 => Event::ActionCached { action: ActionRef { id: format!("a{}", i), ..Default::default() } },
                3 | 4 => spawn("remote cache hit", true, true),
                5 => spawn("disk cache hit", true, false),
                6 => spawn("remote", false, true),
                _ => spawn(if i % 16 == 7 { "worker" } else { "linux-sandbox" }, false, false),
            };
            push(&mut s, ev);
        }
        s
    }

    #[test]
    fn aborting_state_shows_in_both_renderers() {
        let mut s = synthetic();
        let t = 1_100_000i64;
        let slow = ActionRef { id: "slow".into(), mnemonic: "Rustc".into(), label: "//crate:lib".into(), description: "Compiling lib".into(), ..Default::default() };
        s.apply(&Stamped { t_ms: t, event: Event::Phase { phase: crate::events::Phase::Execution } });
        s.apply(&Stamped { t_ms: t, event: Event::ActionRunning { action: slow, strategy: "linux-sandbox".into() } });
        s.apply(&Stamped { t_ms: t, event: Event::Progress { loading: String::new(), loading_detail: String::new(), analysis: String::new(), execution: "[40 / 70]".into() } });
        let now = t + 8_000;
        let mut d = Dashboard::new(120, 30, false);
        let text = d.render(&s, &[], now).join("\n");
        assert!(text.contains("Rustc"), "{}", text);
        let mut r = crate::render::Renderer::new(120, false, 12);
        let inline = r.render(&s, now);
        assert!(inline.contains("40/70"), "{}", inline);

        s.apply(&Stamped { t_ms: now, event: Event::Cancelled });
        assert!(s.cancelling());
        let text = d.render(&s, &[], now + 1_000).join("\n");
        assert!(text.contains("ABORTING") && text.contains("Ctrl-C received") && text.contains("aborting"), "{}", text);
        assert!(r.render(&s, now + 1_000).contains("ABORTING"));
    }

    #[test]
    fn stacked_bar_sums_to_width_and_keeps_small_sources_visible() {
        assert_eq!(stacked_widths(&[1, 0, 999], 20).iter().sum::<usize>(), 20);
        assert_eq!(stacked_widths(&[1, 0, 999], 20)[0], 1);
        assert_eq!(stacked_widths(&[0, 0, 0], 20), vec![0, 0, 0]);
    }

    #[test]
    fn cache_panel_fits_small_and_large_terminals() {
        let s = synthetic();
        assert_eq!(s.cache_total(), 40);
        assert_eq!(s.source_count(Source::ActionCache), 15);
        assert_eq!(s.source_count(Source::LocalExec), 5);
        for (cols, rows) in [(80u16, 20u16), (120, 40), (200, 60)] {
            let mut d = Dashboard::new(cols, rows, false);
            let frame = d.render(&s, &[], 1_100_000);
            let text = frame.join("\n");
            if std::env::var("SHOW").is_ok() {
                println!("---- {}x{}\n{}", cols, rows, text);
            }
            assert_eq!(frame.len(), rows as usize);
            assert!(text.contains("cache"), "no cache panel at {}x{}", cols, rows);
            assert!(text.contains("action cache") || text.contains("more"), "no legend at {}x{}", cols, rows);
            assert!(text.contains("75%"), "hit rate missing at {}x{}", cols, rows);
        }
    }
}
