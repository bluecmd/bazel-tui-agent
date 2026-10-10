//! The interactive `help` finder: `bazel help` on a terminal opens a full-screen, fzf-style
//! search over every command and option Bazel documents (the index the Java side sends as
//! `HelpCommand`/`HelpOption` events). Typing filters with lazy (fuzzy subsequence) matching,
//! the right pane previews the selected entry, Enter prints it to stdout, Esc falls back to
//! Bazel's usual usage text.

use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::pty::Key;
use crate::render::{bold, buffer_to_ansi, dim, st};
use crate::summary::Sty;
use crate::text::*;

#[derive(Clone, Debug)]
pub struct OptionDoc {
    pub name: String,
    pub abbrev: String,
    pub value_type: String,
    pub default: String,
    pub category: String,
    pub help: String,
    pub commands: Vec<String>,
    pub effect_tags: Vec<String>,
    pub metadata_tags: Vec<String>,
    pub expansion: Vec<String>,
    pub deprecation: String,
    pub old_name: String,
    pub multiple: bool,
    pub boolean_syntax: bool,
}

#[derive(Clone, Debug)]
pub enum Doc {
    Command { summary: String, help: String },
    Option(OptionDoc),
}

/// One searchable entry.
#[derive(Clone, Debug)]
pub struct Item {
    /// What the list shows: `build`, `--jobs`, `target-syntax`.
    pub display: String,
    pub doc: Doc,
    /// `display`, two spaces, the one-line summary — original case, for the list row.
    row: Vec<char>,
    /// `row` lower-cased one char per char, what the query is matched against.
    hay: Vec<char>,
    /// Word starts in `hay` (position 0 or after a separator).
    bound: Vec<bool>,
    name_len: usize,
}

impl Item {
    pub fn command(name: &str, summary: &str, help: &str) -> Item {
        Item::new(name.to_string(), summary, Doc::Command { summary: summary.to_string(), help: help.to_string() })
    }

    pub fn option(d: OptionDoc) -> Item {
        let summary = first_sentence(&d.help);
        Item::new(format!("--{}", d.name), &summary, Doc::Option(d))
    }

    fn new(display: String, summary: &str, doc: Doc) -> Item {
        let row: Vec<char> = format!("{}  {}", display, summary.replace('\n', " ")).chars().collect();
        let hay: Vec<char> = row.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        let bound: Vec<bool> = (0..hay.len()).map(|i| i == 0 || matches!(hay[i - 1], ' ' | '-' | '_' | ':' | '/' | '.' | ',' | '(' | '"')).collect();
        let name_len = display.chars().count();
        Item { display, doc, row, hay, bound, name_len }
    }
}

fn first_sentence(help: &str) -> String {
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = flat.len();
    for (i, _) in flat.match_indices(". ") {
        if i > 20 {
            end = i + 1;
            break;
        }
    }
    flat[..end].to_string()
}

// ---- fuzzy matching ---------------------------------------------------------------------------

const MATCH: i32 = 16;
const BOUNDARY: i32 = 12;
const CONSEC: i32 = 10;
const IN_NAME: i32 = 6;
const PREFIX: i32 = 20;
const GAP_OPEN: i32 = 6;
const GAP_EXT: i32 = 1;
const NEG: i32 = i32::MIN / 2;

/// Best-scoring subsequence match of `term` in the item (Smith-Waterman style: bonuses for word
/// starts, runs of consecutive matches and hits inside the name; affine gap penalty). Returns
/// the score and the matched positions, or None when `term` is not a subsequence.
fn fuzzy_term(item: &Item, term: &[char]) -> Option<(i32, Vec<usize>)> {
    let n = item.hay.len();
    let m = term.len();
    if m == 0 || m > n {
        return None;
    }
    let mut prev = vec![NEG; n];
    let mut parents: Vec<Vec<u32>> = Vec::with_capacity(m);
    for (i, &q) in term.iter().enumerate() {
        let mut cur = vec![NEG; n];
        let mut par = vec![u32::MAX; n];
        let mut any = false;
        let mut best_k: Option<(i32, usize)> = None; // max over k <= j-2 of prev[k] + GAP_EXT*k
        for j in 0..n {
            if j >= 2 && prev[j - 2] > NEG {
                let v = prev[j - 2] + GAP_EXT * (j as i32 - 2);
                if best_k.map_or(true, |(b, _)| v > b) {
                    best_k = Some((v, j - 2));
                }
            }
            if item.hay[j] != q {
                continue;
            }
            let bonus = MATCH + if item.bound[j] { BOUNDARY } else { 0 } + if j < item.name_len { IN_NAME } else { 0 };
            if i == 0 {
                cur[j] = bonus + if j == 0 { PREFIX } else { 0 } - (j.min(30) as i32) / 3;
                any = true;
                continue;
            }
            let mut best = NEG;
            let mut bp = u32::MAX;
            if j >= 1 && prev[j - 1] > NEG {
                best = prev[j - 1] + bonus + CONSEC;
                bp = (j - 1) as u32;
            }
            if let Some((v, k)) = best_k {
                let sc = v - GAP_OPEN - GAP_EXT * (j as i32 - 2) + bonus;
                if sc > best {
                    best = sc;
                    bp = k as u32;
                }
            }
            if best > NEG {
                cur[j] = best;
                par[j] = bp;
                any = true;
            }
        }
        if !any {
            return None;
        }
        parents.push(par);
        prev = cur;
    }
    let (mut j, score) = prev.iter().enumerate().max_by_key(|(_, v)| **v).map(|(j, v)| (j, *v))?;
    if score <= NEG {
        return None;
    }
    let mut pos = vec![0usize; m];
    for i in (0..m).rev() {
        pos[i] = j;
        if i > 0 {
            j = parents[i][j] as usize;
        }
    }
    Some((score, pos))
}

/// All whitespace-separated terms must match; scores add up.
fn fuzzy(item: &Item, terms: &[Vec<char>]) -> Option<(i32, Vec<usize>)> {
    let mut score = 0;
    let mut pos = Vec::new();
    for t in terms {
        let (s, p) = fuzzy_term(item, t)?;
        score += s;
        pos.extend(p);
    }
    pos.sort_unstable();
    pos.dedup();
    Some((score, pos))
}

// ---- the finder -------------------------------------------------------------------------------

struct Match {
    idx: usize,
    score: i32,
    pos: Vec<usize>,
}

pub struct Finder {
    items: Vec<Item>,
    query: String,
    matches: Vec<Match>,
    selected: usize,
    scroll: usize,
    preview_scroll: u16,
    /// `Some(None)`: quit; `Some(Some(i))`: item `i` chosen.
    done: Option<Option<usize>>,
    color: bool,
    cols: u16,
    rows: u16,
    tick: usize,
    term: Terminal<TestBackend>,
}

impl Finder {
    pub fn new(items: &[Item], cols: u16, rows: u16, color: bool) -> Finder {
        let mut f = Finder {
            items: items.to_vec(),
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: 0,
            preview_scroll: 0,
            done: None,
            color,
            cols,
            rows,
            tick: 0,
            term: Terminal::new(TestBackend::new(cols, rows)).expect("backend"),
        };
        f.refilter();
        f
    }

    pub fn finished(&self) -> bool {
        self.done.is_some()
    }

    /// The chosen item once Enter was pressed; None after a quit (or while running).
    pub fn result(&self) -> Option<&Item> {
        self.done.flatten().and_then(|i| self.items.get(i))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.term.backend_mut().resize(cols, rows);
            self.term.clear().ok();
        }
    }

    fn refilter(&mut self) {
        let terms: Vec<Vec<char>> = self.query.split_whitespace().map(|t| t.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()).collect();
        self.matches = if terms.is_empty() {
            self.items.iter().enumerate().map(|(idx, _)| Match { idx, score: 0, pos: Vec::new() }).collect()
        } else {
            let mut v: Vec<Match> = self.items.iter().enumerate().filter_map(|(idx, it)| fuzzy(it, &terms).map(|(score, pos)| Match { idx, score, pos })).collect();
            v.sort_by(|a, b| b.score.cmp(&a.score).then(self.items[a.idx].name_len.cmp(&self.items[b.idx].name_len)).then(self.items[a.idx].display.cmp(&self.items[b.idx].display)));
            v
        };
        self.selected = 0;
        self.scroll = 0;
        self.preview_scroll = 0;
    }

    fn move_sel(&mut self, delta: i64) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as i64 - 1;
        self.selected = (self.selected as i64 + delta).clamp(0, last) as usize;
        self.preview_scroll = 0;
    }

    pub fn key(&mut self, k: Key) {
        let page = (self.rows as i64 - 6).max(1);
        match k {
            Key::Char(c) => {
                self.query.push(c);
                self.refilter();
            }
            Key::Backspace => {
                if self.query.pop().is_some() {
                    self.refilter();
                }
            }
            Key::Ctrl('u') => {
                self.query.clear();
                self.refilter();
            }
            Key::Ctrl('w') => {
                let trimmed = self.query.trim_end().to_string();
                let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                self.query.truncate(cut);
                self.refilter();
            }
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => self.move_sel(-1),
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') | Key::Tab => self.move_sel(1),
            Key::PageUp => self.move_sel(-page),
            Key::PageDown => self.move_sel(page),
            Key::Home => self.move_sel(-(self.matches.len() as i64)),
            Key::End => self.move_sel(self.matches.len() as i64),
            Key::Right | Key::Ctrl('f') => self.preview_scroll = self.preview_scroll.saturating_add(3),
            Key::Left | Key::Ctrl('b') => self.preview_scroll = self.preview_scroll.saturating_sub(3),
            Key::Enter => {
                if let Some(m) = self.matches.get(self.selected) {
                    self.done = Some(Some(m.idx));
                }
            }
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('g') => self.done = Some(None),
            Key::Ctrl('d') if self.query.is_empty() => self.done = Some(None),
            _ => {}
        }
    }

    /// The whole screen as `rows` ANSI lines.
    pub fn render(&mut self) -> Vec<String> {
        self.tick += 1;
        let color = self.color;
        let rows_c = Layout::vertical([Constraint::Length(3), Constraint::Fill(1), Constraint::Length(1)]).split(ratatui::layout::Rect::new(0, 0, self.cols, self.rows));
        let body = Layout::horizontal([Constraint::Length(((self.cols as usize * 2 / 5).clamp(30, 64)) as u16), Constraint::Fill(1)]).split(rows_c[1]);
        let list_h = body[0].height.saturating_sub(2) as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if list_h > 0 && self.selected >= self.scroll + list_h {
            self.scroll = self.selected + 1 - list_h;
        }

        let n_cmd = self.items.iter().filter(|i| matches!(i.doc, Doc::Command { .. })).count();
        let title_right = format!("{} / {}  ·  {} commands, {} options", self.matches.len(), self.items.len(), n_cmd, self.items.len() - n_cmd);
        let input = Line::from(vec![
            Span::styled("❯ ", bold(ACCENT)),
            Span::styled(self.query.clone(), bold(TITLE)),
            Span::styled(if self.tick / 5 % 2 == 0 { "▏" } else { " " }, st(ACCENT)),
        ]);

        let mut list_lines: Vec<Line> = Vec::with_capacity(list_h);
        let list_w = body[0].width.saturating_sub(2) as usize;
        for (row, m) in self.matches.iter().enumerate().skip(self.scroll).take(list_h) {
            let it = &self.items[m.idx];
            let sel = row == self.selected;
            let mut spans: Vec<Span> = vec![Span::styled(if sel { "▶ " } else { "  " }, bold(ACCENT))];
            let avail = list_w.saturating_sub(2);
            let mut used = 0;
            let mut buf = String::new();
            let mut buf_style: Option<Style> = None;
            let flush = |spans: &mut Vec<Span>, buf: &mut String, style: Option<Style>| {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(buf), style.unwrap_or_default()));
                }
            };
            for (i, c) in it.row.iter().enumerate() {
                let w = c.to_string().width();
                if used + w > avail {
                    break;
                }
                used += w;
                let hit = m.pos.binary_search(&i).is_ok();
                let in_name = i < it.name_len;
                let style = if hit {
                    bold(WARN)
                } else if in_name {
                    if sel { bold(TITLE) } else { Style::new().fg(crate::render::rgb(TITLE)) }
                } else {
                    st(MUTED)
                };
                if buf_style != Some(style) {
                    flush(&mut spans, &mut buf, buf_style);
                    buf_style = Some(style);
                }
                buf.push(*c);
            }
            flush(&mut spans, &mut buf, buf_style);
            let mut line = Line::from(spans);
            if sel {
                line = line.style(Style::new().bg(crate::render::rgb((40, 46, 66))));
            }
            list_lines.push(line);
        }
        if self.matches.is_empty() {
            list_lines.push(Line::from(Span::styled("  no matches", dim())));
        }

        let preview: Vec<Line> = match self.matches.get(self.selected) {
            Some(m) => preview_lines(&self.items[m.idx], body[1].width.saturating_sub(2) as usize),
            None => Vec::new(),
        };
        let preview_title = self.matches.get(self.selected).map(|m| self.items[m.idx].display.clone()).unwrap_or_default();
        let preview_scroll = self.preview_scroll.min(preview.len().saturating_sub(1) as u16);
        self.preview_scroll = preview_scroll;

        let footer = Line::from(vec![
            Span::styled(" ↑↓", bold(TITLE)),
            Span::styled(" select  ", st(MUTED)),
            Span::styled("⏎", bold(TITLE)),
            Span::styled(" print  ", st(MUTED)),
            Span::styled("←→", bold(TITLE)),
            Span::styled(" scroll  ", st(MUTED)),
            Span::styled("esc", bold(TITLE)),
            Span::styled(" usage text  ", st(MUTED)),
            Span::styled("type to search: words must all match, letters may be apart", dim()),
        ]);

        self.term
            .draw(|f| {
                let ib = Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(st(BORDER))
                    .title_top(Line::from(Span::styled(" bazel help ", bold(ACCENT))))
                    .title_top(Line::from(Span::styled(format!(" {} ", title_right), st(MUTED))).right_aligned());
                let ii = ib.inner(rows_c[0]);
                f.render_widget(ib, rows_c[0]);
                f.render_widget(Paragraph::new(input.clone()), ii);

                let lb = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(st(BORDER));
                let li = lb.inner(body[0]);
                f.render_widget(lb, body[0]);
                f.render_widget(Paragraph::new(list_lines.clone()), li);

                let pb = Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(st(BORDER))
                    .title_top(Line::from(Span::styled(format!(" {} ", preview_title), bold(TITLE))));
                let pi = pb.inner(body[1]);
                f.render_widget(pb, body[1]);
                f.render_widget(Paragraph::new(preview.clone()).wrap(Wrap { trim: false }).scroll((preview_scroll, 0)), pi);

                f.render_widget(Paragraph::new(footer.clone()), rows_c[2]);
            })
            .ok();
        let text = buffer_to_ansi(self.term.backend().buffer(), self.rows as usize, color);
        text.split('\n').map(|s| s.to_string()).collect()
    }
}

fn preview_lines(it: &Item, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{:<10}", k), st(MUTED)), Span::styled(v, st(TITLE))]);
    match &it.doc {
        Doc::Command { summary, help } => {
            lines.push(Line::from(vec![Span::styled("bazel ", st(MUTED)), Span::styled(it.display.clone(), bold(ACCENT))]));
            lines.push(Line::from(Span::styled(summary.clone(), st(TITLE))));
            lines.push(Line::default());
            for l in help.lines() {
                lines.push(Line::from(Span::raw(l.to_string())));
            }
        }
        Doc::Option(d) => {
            let mut head = vec![Span::styled(it.display.clone(), bold(ACCENT))];
            if d.boolean_syntax {
                head.push(Span::styled("  (--no".to_string() + &d.name + ")", st(MUTED)));
            } else if !d.value_type.is_empty() {
                head.push(Span::styled(format!("=<{}>", d.value_type), st(TITLE)));
            }
            if !d.abbrev.is_empty() {
                head.push(Span::styled(format!("   -{}", d.abbrev), bold(TITLE)));
            }
            lines.push(Line::from(head));
            if !d.default.is_empty() {
                lines.push(kv("default", d.default.clone()));
            }
            if d.multiple {
                lines.push(kv("repeat", "may be given several times".into()));
            }
            if !d.commands.is_empty() {
                lines.push(kv("commands", d.commands.join(", ")));
            }
            if !d.category.is_empty() {
                lines.push(kv("category", d.category.to_lowercase().replace('_', " ")));
            }
            if !d.effect_tags.is_empty() {
                lines.push(kv("effects", d.effect_tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>().join(", ")));
            }
            if !d.metadata_tags.is_empty() {
                lines.push(kv("tags", d.metadata_tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>().join(", ")));
            }
            if !d.expansion.is_empty() {
                lines.push(kv("expands to", d.expansion.join(" ")));
            }
            if !d.old_name.is_empty() {
                lines.push(kv("old name", format!("--{}", d.old_name)));
            }
            if !d.deprecation.is_empty() {
                lines.push(Line::from(vec![Span::styled("deprecated ", bold(WARN)), Span::styled(d.deprecation.clone(), st(WARN))]));
            }
            lines.push(Line::default());
            for para in d.help.split('\n') {
                lines.push(Line::from(Span::raw(para.to_string())));
            }
        }
    }
    let _ = width;
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("⏎ prints this to the terminal", dim())));
    lines
}

/// The chosen entry as plain terminal text (what `bazel help` leaves behind after Enter).
pub fn print(it: &Item, color: bool, width: usize) -> String {
    let sty = Sty { color };
    let w = width.clamp(40, 100);
    let mut out = String::new();
    match &it.doc {
        Doc::Command { summary, help } => {
            out.push_str(&format!("{} {}\n", sty.dim("bazel"), sty.rgb_bold(&it.display, ACCENT)));
            out.push_str(&format!("  {}\n\n", summary));
            for l in help.lines() {
                out.push_str(&wrap_indent(l, w, "  "));
            }
            if !help.is_empty() {
                out.push('\n');
            }
            out.push_str(&sty.dim(&format!("  options: bazel help {} --long\n", it.display)));
        }
        Doc::Option(d) => {
            let mut head = sty.rgb_bold(&it.display, ACCENT);
            if d.boolean_syntax {
                head.push_str(&sty.dim(&format!("  (--no{})", d.name)));
            } else if !d.value_type.is_empty() {
                head.push_str(&format!("={}", sty.bold(&format!("<{}>", d.value_type))));
            }
            if !d.abbrev.is_empty() {
                head.push_str(&format!("   {}", sty.bold(&format!("-{}", d.abbrev))));
            }
            out.push_str(&head);
            out.push('\n');
            let kv = |out: &mut String, k: &str, v: &str| out.push_str(&format!("  {} {}\n", sty.dim(&format!("{:<10}", k)), v));
            if !d.default.is_empty() {
                kv(&mut out, "default", &d.default);
            }
            if !d.commands.is_empty() {
                kv(&mut out, "commands", &d.commands.join(", "));
            }
            if !d.category.is_empty() {
                kv(&mut out, "category", &d.category.to_lowercase().replace('_', " "));
            }
            if !d.effect_tags.is_empty() {
                kv(&mut out, "effects", &d.effect_tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>().join(", "));
            }
            if !d.metadata_tags.is_empty() {
                kv(&mut out, "tags", &d.metadata_tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>().join(", "));
            }
            if !d.expansion.is_empty() {
                kv(&mut out, "expands to", &d.expansion.join(" "));
            }
            if !d.old_name.is_empty() {
                kv(&mut out, "old name", &format!("--{}", d.old_name));
            }
            if !d.deprecation.is_empty() {
                kv(&mut out, "deprecated", &sty.rgb(&d.deprecation, WARN));
            }
            out.push('\n');
            for para in d.help.split('\n') {
                out.push_str(&wrap_indent(para, w, "  "));
            }
        }
    }
    out
}

fn wrap_indent(text: &str, width: usize, indent: &str) -> String {
    let avail = width.saturating_sub(indent.len()).max(20);
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() && line.width() + 1 + word.width() > avail {
            out.push_str(indent);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push_str(indent);
    out.push_str(&line);
    out.push('\n');
    out
}
