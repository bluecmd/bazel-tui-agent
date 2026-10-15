//! The end-of-command report, printed once the terminal is back to normal: Bazel's completion
//! line, a one-line verdict with timings, then what the command actually produced — the output
//! files of every top-level target for a build, the test overview for a test run.
//!
//! Replaces Bazel's own "Target //x up-to-date:" block and test summary (both silenced by the
//! Java side), honouring the `--show_result` and `--test_summary` values that were in effect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use unicode_width::UnicodeWidthStr;

use crate::state::{Source, State, TargetDone, TestResult};
use crate::text::*;

/// ANSI styling that switches off with `--color=no`.
pub struct Sty {
    pub color: bool,
}

impl Sty {
    fn wrap(&self, codes: &str, t: &str) -> String {
        if self.color { format!("\x1b[{}m{}{}", codes, t, RESET) } else { t.to_string() }
    }
    /// A name in bold, always with an explicit foreground (never bold on the default colour).
    pub fn bold(&self, t: &str) -> String {
        self.wrap(&format!("1;{}", crate::text::color_code(38, REPORT_TEXT.0, REPORT_TEXT.1, REPORT_TEXT.2)), t)
    }
    /// Secondary text, in the report's muted colour (never SGR 2 faint: Konsole and other
    /// terminals draw that at almost normal intensity).
    pub fn dim(&self, t: &str) -> String {
        self.rgb(t, REPORT_MUTED)
    }
    pub fn rgb(&self, t: &str, c: (u8, u8, u8)) -> String {
        self.wrap(&crate::text::color_code(38, c.0, c.1, c.2), t)
    }
    /// Colour and bold as two SGR runs, one reset after both.
    pub fn rgb_bold(&self, t: &str, c: (u8, u8, u8)) -> String {
        if self.color { format!("\x1b[{}m\x1b[1m{}{}", crate::text::color_code(38, c.0, c.1, c.2), t, RESET) } else { t.to_string() }
    }
    /// A clickable web address (OSC 8, which terminals without hyperlinks ignore), coloured.
    pub fn link(&self, url: &str, c: (u8, u8, u8)) -> String {
        if self.color { format!("\x1b[{}m\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\{}", crate::text::color_code(38, c.0, c.1, c.2), url, url, RESET) } else { url.to_string() }
    }
}

pub struct Report {
    pub stderr: String,
    pub stdout: String,
}

/// Everything to print after the last Bazel message; empty when the command built nothing.
pub fn report(s: &State, now: i64, color: bool, width: usize) -> Report {
    let sty = Sty { color };
    let mut rep = Report { stderr: String::new(), stdout: String::new() };
    let Some(success) = s.build_success else {
        // No build verdict (a query, a failure before the build started): the BES page still
        // exists and Bazel's own line about it was held back.
        if let Some(line) = results_line(s, &sty) {
            rep.stderr.push_str(&line);
            rep.stderr.push('\n');
        }
        return rep;
    };
    let width = width.clamp(60, 200);

    rep.stderr.push_str(&completion_line(s, &sty, success));
    rep.stderr.push('\n');
    rep.stderr.push_str(&verdict_line(s, now, &sty, success, width));
    rep.stderr.push('\n');
    if let Some(line) = sources_line(s, &sty, width) {
        rep.stderr.push_str(&line);
        rep.stderr.push('\n');
    }
    if let Some(line) = results_line(s, &sty) {
        rep.stderr.push_str(&line);
        rep.stderr.push('\n');
    }

    match s.command.as_deref().unwrap_or("") {
        "test" | "coverage" => {
            let t = tests(s, &sty, width);
            if !t.is_empty() {
                rep.stdout.push('\n');
                rep.stdout.push_str(&t);
            }
        }
        "build" | "run" | "mobile-install" => {
            let a = artifacts(s, &sty, width);
            if !a.is_empty() {
                rep.stderr.push('\n');
                rep.stderr.push_str(&a);
            }
        }
        _ => {}
    }
    rep
}

/// The agent jar on disk is newer than the one this server loaded: the user rebuilt it and is
/// still talking to the old code (the classic "why did nothing change" trap).
pub fn stale_jar_notice(s: &State, color: bool) -> Option<String> {
    if s.agent_jar.is_empty() || s.agent_jar_mtime_ms <= 0 {
        return None;
    }
    let mtime = std::fs::metadata(&s.agent_jar).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64;
    if (mtime - s.agent_jar_mtime_ms).abs() < 1500 {
        return None;
    }
    let sty = Sty { color };
    Some(format!("{} {}\n", sty.rgb_bold("bazel-hook:", WARN), sty.rgb("the agent jar changed on disk after this server started; run `bazel shutdown` to load the new one", WARN)))
}

/// Bazel's own last line, which its (silenced) UI handler would have printed.
fn completion_line(s: &State, sty: &Sty, success: bool) -> String {
    if success {
        let failed_tests = s.tests.values().filter(|t| t.status != "PASSED" && t.status != "NO_STATUS").count();
        let mut m = String::from("Build completed");
        if failed_tests == 0 {
            m.push_str(" successfully");
        } else {
            m.push_str(&format!(", {} test{} FAILED", failed_tests, if failed_tests == 1 { "" } else { "s" }));
        }
        m.push_str(&format!(", {} total action{}", s.completed, if s.completed == 1 { "" } else { "s" }));
        format!("{} {}", sty.rgb("INFO:", OK), m)
    } else {
        format!("{} Build did NOT complete successfully", sty.rgb_bold("ERROR:", BAD))
    }
}

/// `head` (already styled, `head_w` columns wide) followed by `parts` joined with " · " in the
/// muted colour, wrapped at `width` with continuation lines indented by `indent`. The report
/// is printed once, so a wrapped line would not break anything, but it reads badly on an
/// 80-column terminal where these lines do not fit.
fn head_and_parts(sty: &Sty, head: &str, head_w: usize, parts: &[String], indent: usize, width: usize) -> String {
    let mut out = String::from(head);
    let mut col = head_w;
    let mut line = String::new();
    for p in parts {
        let w = p.width();
        if !line.is_empty() && col + 3 + w > width {
            out.push_str(&sty.dim(&line));
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            line.clear();
            col = indent;
        }
        if !line.is_empty() {
            line.push_str(" · ");
            col += 3;
        }
        line.push_str(p);
        col += w;
    }
    out.push_str(&sty.dim(&line));
    out
}

fn verdict_line(s: &State, now: i64, sty: &Sty, success: bool, width: usize) -> String {
    let total = (now - s.t0_ms) as f64 / 1000.0;
    let mut parts = Vec::new();
    let order = ["loading", "analysis", "execution", "finished"];
    let times: Vec<(&str, i64)> = order.iter().filter_map(|p| s.phase_t.get(*p).map(|t| (*p, *t))).collect();
    for w in times.windows(2) {
        if w[0].0 != "finished" {
            parts.push(format!("{} {}", w[0].0, fmt_duration((w[1].1 - w[0].1) as f64 / 1000.0)));
        }
    }
    if s.completed > 0 {
        parts.push(format!("{} actions", with_commas(s.completed)));
    }
    if s.cache_total() > 0 {
        parts.push(format!("{}% cached", (s.hit_rate().unwrap_or(0.0) * 100.0).round() as u32));
    }
    if s.peak_running > 0 {
        parts.push(format!("peak {} parallel", s.peak_running));
    }
    let tests_failed = s.tests.values().any(|t| rank(t) == 0);
    let mark = if success && !tests_failed { sty.rgb_bold("✔", OK) } else { sty.rgb_bold("✘", BAD) };
    let name = s.command.as_deref().unwrap_or("build");
    let command = sty.bold(name);
    match s.cancelled_ms {
        Some(t) => {
            let what = format!("cancelled by Ctrl-C after {}, stopped in {}", fmt_duration((t - s.t0_ms) as f64 / 1000.0), fmt_duration((now - t).max(0) as f64 / 1000.0));
            let head = format!("{} {} {}  ", mark, command, sty.rgb(&what, ABORT));
            head_and_parts(sty, &head, 1 + 1 + name.width() + 1 + what.width() + 2, &parts, 2, width)
        }
        None => {
            let time = fmt_duration(total);
            let head = format!("{} {} {}  ", mark, command, time);
            head_and_parts(sty, &head, 1 + 1 + name.width() + 1 + time.width() + 2, &parts, 2, width)
        }
    }
}

/// Where the actions came from: "actions 455 · 449 action cache · 3 remote cache · 2 local exec".
fn sources_line(s: &State, sty: &Sty, width: usize) -> Option<String> {
    let total = s.cache_total();
    if total == 0 {
        return None;
    }
    let mut parts = Vec::new();
    for src in Source::ALL {
        let n = s.source_count(src);
        if n > 0 {
            parts.push(format!("{} {}", with_commas(n), src.name()));
        }
    }
    let n = with_commas(total);
    let head = format!("  {} {}  ", sty.dim("actions"), n);
    let head_w = 2 + 7 + 1 + n.width() + 2;
    Some(head_and_parts(sty, &head, head_w, &parts, head_w, width))
}

/// The build's page on the Build Event Service, as a link: "  results  https://…".
fn results_line(s: &State, sty: &Sty) -> Option<String> {
    if s.bes_url.is_empty() {
        return None;
    }
    Some(format!("  {}  {}", sty.dim("results"), sty.link(&s.bes_url, LINK)))
}

// ---- paths ------------------------------------------------------------------------------------

/// Maps output paths to the convenience symlinks in the workspace (`bazel-bin/...`), the way
/// Bazel's result printer does, by reading where those symlinks point.
struct Paths {
    exec_root: PathBuf,
    /// (symlink target, symlink name), longest target first.
    links: Vec<(PathBuf, String)>,
}

impl Paths {
    fn new(workspace: &str, exec_root: &str) -> Paths {
        let mut links = Vec::new();
        if let Ok(rd) = std::fs::read_dir(workspace) {
            for entry in rd.flatten() {
                let Ok(target) = std::fs::read_link(entry.path()) else { continue };
                if target.is_absolute() {
                    links.push((target, entry.file_name().to_string_lossy().into_owned()));
                }
            }
        }
        links.sort_by(|a, b| b.0.as_os_str().len().cmp(&a.0.as_os_str().len()).then(a.1.cmp(&b.1)));
        Paths { exec_root: PathBuf::from(exec_root), links }
    }

    /// `bazel-bin/x` for a path under a workspace symlink; the exec path for anything else under
    /// the exec root; the path itself otherwise.
    fn pretty(&self, path: &str) -> String {
        let p = self.absolute(path);
        for (target, name) in &self.links {
            if let Ok(rest) = p.strip_prefix(target) {
                let rest = rest.to_string_lossy();
                return if rest.is_empty() { name.clone() } else { format!("{}/{}", name, rest) };
            }
        }
        if !self.exec_root.as_os_str().is_empty() {
            if let Ok(rel) = p.strip_prefix(&self.exec_root) {
                return rel.to_string_lossy().into_owned();
            }
        }
        path.to_string()
    }

    fn absolute(&self, path: &str) -> PathBuf {
        if path.starts_with('/') { PathBuf::from(path) } else { self.exec_root.join(path) }
    }
}

// ---- build: artifacts -------------------------------------------------------------------------

enum Kind {
    File,
    Dir { files: u64 },
    Missing,
}

struct FileRow {
    path: String,
    abs: PathBuf,
    size: u64,
    kind: Kind,
    /// Written during this command (as opposed to left from an earlier one).
    fresh: bool,
}

fn stat_row(paths: &Paths, exec_path: &str, t0_ms: i64) -> FileRow {
    let abs = paths.absolute(exec_path);
    let path = paths.pretty(exec_path);
    let Ok(md) = std::fs::metadata(&abs) else { return FileRow { path, abs, size: 0, kind: Kind::Missing, fresh: false } };
    let mtime_ms = |m: &std::fs::Metadata| m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
    if md.is_dir() {
        let (files, size, newest) = walk(&abs, 0, 0, 0, &mut 0);
        FileRow { path, abs, size, kind: Kind::Dir { files }, fresh: newest >= t0_ms - 2000 }
    } else {
        FileRow { path, abs, size: md.len(), kind: Kind::File, fresh: mtime_ms(&md) >= t0_ms - 2000 }
    }
}

/// (files, bytes, newest mtime ms) under a directory, giving up after 20k entries.
fn walk(dir: &Path, mut files: u64, mut size: u64, mut newest: i64, budget: &mut u32) -> (u64, u64, i64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return (files, size, newest) };
    for e in rd.flatten() {
        *budget += 1;
        if *budget > 20_000 {
            break;
        }
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            let (f, s, n) = walk(&e.path(), files, size, newest, budget);
            files = f;
            size = s;
            newest = n;
        } else {
            files += 1;
            size += md.len();
            let m = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
            newest = newest.max(m);
        }
    }
    (files, size, newest)
}

/// The first level of a directory output as a tree: what a single-target build of a directory
/// (a tree artifact, a packaged bundle) is usually run to see. Directories first, then files.
fn dir_tree(sty: &Sty, dir: &Path, t0_ms: i64, width: usize) -> String {
    const MAX: usize = 40;
    let Ok(rd) = std::fs::read_dir(dir) else { return String::new() };
    let mut entries: Vec<(bool, String, u64, u64, bool)> = Vec::new(); // is_dir, name, size, files, fresh
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            let (files, size, newest) = walk(&e.path(), 0, 0, 0, &mut 0);
            entries.push((true, name, size, files, newest >= t0_ms - 2000));
        } else {
            let m = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
            entries.push((false, name, md.len(), 0, m >= t0_ms - 2000));
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let shown = entries.len().min(MAX);
    let namew = entries.iter().take(shown).map(|e| e.1.width() + if e.0 { 1 } else { 0 }).max().unwrap_or(0).min(width.saturating_sub(30));
    let mut out = String::new();
    for (i, (is_dir, name, size, files, fresh)) in entries.iter().take(shown).enumerate() {
        let last = i + 1 == shown && entries.len() == shown;
        let branch = if last { "└─" } else { "├─" };
        let shown_name = if *is_dir { format!("{}/", name) } else { name.clone() };
        let pad = " ".repeat(namew.saturating_sub(shown_name.width()));
        let name_s = shown_name.clone();
        let detail = if *is_dir {
            format!("{}   {}", sty.dim(&format!("{:>8}", fmt_size(*size))), sty.dim(&format!("{} file{}", files, if *files == 1 { "" } else { "s" })))
        } else {
            format!("{}   {}", sty.dim(&format!("{:>8}", fmt_size(*size))), if *fresh { sty.rgb("✓", REPORT_OK) } else { sty.dim("○") })
        };
        out.push_str(&format!("       {} {}{}   {}\n", sty.dim(branch), name_s, pad, detail));
    }
    if entries.len() > shown {
        out.push_str(&format!("       {} {}\n", sty.dim("└─"), sty.dim(&format!("… {} more entries", entries.len() - shown))));
    }
    out
}

pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} B", bytes)
    } else if v < 10.0 {
        format!("{:.1} {}", v, UNITS[u])
    } else {
        format!("{:.0} {}", v, UNITS[u])
    }
}

/// A path as two runs: everything up to and including the last slash dim, the last component
/// plain. A trailing slash (directory outputs) belongs to the last component.
fn path_runs(sty: &Sty, path: &str) -> String {
    let body = path.strip_suffix('/').unwrap_or(path);
    match body.rfind('/') {
        Some(i) => format!("{}{}", sty.dim(&path[..i + 1]), &path[i + 1..]),
        None => path.to_string(),
    }
}

fn artifacts(s: &State, sty: &Sty, width: usize) -> String {
    let limit = match s.show_result {
        0 => return String::new(),
        x if x <= 1 => 30,
        x => x as usize,
    };
    // Last report per label wins (a target can complete once per configuration).
    let mut by_label: BTreeMap<&str, &TargetDone> = BTreeMap::new();
    for t in &s.targets {
        by_label.insert(t.label.as_str(), t);
    }
    if by_label.is_empty() {
        return String::new();
    }
    let paths = Paths::new(&s.workspace, &s.exec_root);
    let targets: Vec<&TargetDone> = by_label.values().copied().collect();
    // A handful of targets: list them all. A wildcard build: only the targets that failed or
    // whose outputs were written by this command; the unchanged ones are a count in the footer.
    let all_rows = targets.len() <= 10;
    let mut unchanged = 0usize;
    let mut picked: Vec<&TargetDone> = Vec::new();
    let mut stats: Vec<(&TargetDone, Vec<FileRow>)> = Vec::new();
    for t in &targets {
        let files: Vec<FileRow> = if t.failed { Vec::new() } else { t.outputs.iter().map(|p| stat_row(&paths, p, s.t0_ms)).collect() };
        let interesting = t.failed || files.iter().any(|f| f.fresh || matches!(f.kind, Kind::Missing));
        if all_rows || interesting {
            if picked.len() < limit {
                picked.push(t);
            }
        } else {
            unchanged += 1;
        }
        stats.push((t, files));
    }
    let shown = &picked[..];

    const PER_TARGET: usize = 20;
    let mut rows: Vec<(String, Vec<FileRow>, usize, u64)> = Vec::new(); // label, files, hidden count, hidden bytes
    let mut n_files = 0u64;
    let mut n_bytes = 0u64;
    let mut n_fresh = 0u64;
    let mut n_failed = 0u64;
    for (t, files) in stats {
        if t.failed {
            n_failed += 1;
        }
        for r in &files {
            n_files += match r.kind {
                Kind::Dir { files } => files,
                Kind::Missing => 0,
                _ => 1,
            };
            n_bytes += r.size;
            if r.fresh {
                n_fresh += 1;
            }
        }
        if !shown.iter().any(|p| std::ptr::eq(*p, t)) {
            continue;
        }
        let total = files.len();
        let hidden_bytes: u64 = files.iter().skip(PER_TARGET).map(|f| f.size).sum();
        let kept: Vec<FileRow> = files.into_iter().take(PER_TARGET).collect();
        rows.push((t.label.clone(), kept, total.saturating_sub(PER_TARGET), hidden_bytes));
    }

    let pathw = rows.iter().flat_map(|r| r.1.iter()).map(|f| f.path.width()).max().unwrap_or(0).min(width.saturating_sub(5 + 9 + 2 + 14));
    // One target with one directory output: the user almost certainly wants to see what is in it.
    let solo_dir = rows.len() == 1 && rows[0].1.len() == 1 && rows[0].2 == 0 && matches!(rows[0].1[0].kind, Kind::Dir { .. });
    let mut out = String::new();
    for (label, files, hidden, hidden_bytes) in &rows {
        if files.is_empty() && *hidden == 0 {
            if targets.iter().any(|t| t.label == *label && t.failed) {
                out.push_str(&format!(" {} {}  {}\n", sty.rgb_bold("✘", BAD), sty.bold(label), sty.rgb("failed to build", BAD)));
            } else {
                out.push_str(&format!(" {} {}  {}\n", sty.dim("▸"), sty.bold(label), sty.dim("no files")));
            }
            continue;
        }
        out.push_str(&format!(" {} {}\n", sty.dim("▸"), sty.bold(label)));
        for f in files {
            let pad = " ".repeat(pathw.saturating_sub(f.path.width()));
            // The directory part dim, the file or directory name in the plain foreground.
            let path = match f.kind {
                Kind::Missing => sty.dim(&f.path),
                Kind::Dir { .. } => path_runs(sty, &format!("{}/", f.path)),
                _ => path_runs(sty, &f.path),
            };
            let pad = if matches!(f.kind, Kind::Dir { .. }) { pad.get(1..).unwrap_or("").to_string() } else { pad };
            let size = match f.kind {
                Kind::Missing => String::new(),
                _ => fmt_size(f.size),
            };
            let status = match (&f.kind, f.fresh) {
                (Kind::Missing, _) => sty.rgb("? missing", WARN),
                (Kind::Dir { files }, true) => format!("{} {}", sty.rgb("✓", REPORT_OK), sty.dim(&format!("{} files", files))),
                (Kind::Dir { files }, false) => sty.dim(&format!("○ {} files", files)),
                (_, true) => format!("{} {}", sty.rgb("✓", REPORT_OK), sty.dim("built")),
                (_, false) => sty.dim("○ unchanged"),
            };
            out.push_str(&format!("     {}{}   {}   {}\n", path, pad, sty.dim(&format!("{:>8}", size)), status));
            if solo_dir {
                out.push_str(&dir_tree(sty, &f.abs, s.t0_ms, width));
            }
        }
        if *hidden > 0 {
            out.push_str(&format!("     {}\n", sty.dim(&format!("… {} more files ({})", hidden, fmt_size(*hidden_bytes)))));
        }
    }
    let more = targets.len() - shown.len() - unchanged;
    if more > 0 {
        out.push_str(&format!(" {}\n", sty.dim(&format!("… {} more targets", more))));
    }
    let mut foot = vec![format!("{} target{}", targets.len(), if targets.len() == 1 { "" } else { "s" })];
    if n_files > 0 {
        foot.push(format!("{} file{}", with_commas(n_files), if n_files == 1 { "" } else { "s" }));
        foot.push(fmt_size(n_bytes));
    }
    if n_fresh > 0 {
        foot.push(format!("{} built now", n_fresh));
    }
    if n_failed > 0 {
        foot.push(format!("{} failed", n_failed));
    }
    if unchanged > 0 {
        foot.push(format!("{} unchanged target{} not listed", unchanged, if unchanged == 1 { "" } else { "s" }));
    }
    out.push_str(&format!(" {}\n", sty.dim(&foot.join(" · "))));
    out
}

// ---- test: overview ---------------------------------------------------------------------------

fn rank(t: &TestResult) -> u8 {
    match t.status.as_str() {
        "FAILED" | "REMOTE_FAILURE" | "FAILED_TO_BUILD" | "TIMEOUT" => 0,
        "FLAKY" => 1,
        "PASSED" => 3,
        _ => 2,
    }
}

fn tests(s: &State, sty: &Sty, width: usize) -> String {
    if s.tests.is_empty() {
        return String::new();
    }
    let level = s.test_summary.as_str();
    let mut v: Vec<&TestResult> = s.tests.values().collect();
    v.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.label.cmp(&b.label)));
    let paths = Paths::new(&s.workspace, &s.exec_root);

    let passed = v.iter().filter(|t| t.status == "PASSED").count();
    let flaky = v.iter().filter(|t| t.status == "FLAKY").count();
    let failed = v.iter().filter(|t| rank(t) == 0).count();
    let other = v.len() - passed - flaky - failed;
    let cached = v.iter().filter(|t| t.cached).count();
    let executed: Vec<&&TestResult> = v.iter().filter(|t| !t.cached && !t.skipped && t.runs > 0 && !matches!(t.status.as_str(), "NO_STATUS" | "FAILED_TO_BUILD" | "BLAZE_HALTED_BEFORE_TESTING")).collect();
    let test_time: u64 = executed.iter().map(|t| t.time_ms).sum();

    let mut head = vec![format!("{} {}", sty.rgb_bold("tests", ACCENT), sty.dim(&format!("{}", v.len())))];
    if passed > 0 {
        head.push(sty.rgb(&format!("{} passed", passed), OK));
    }
    if failed > 0 {
        head.push(sty.rgb_bold(&format!("{} failed", failed), BAD));
    }
    if flaky > 0 {
        head.push(sty.rgb(&format!("{} flaky", flaky), WARN));
    }
    if other > 0 {
        head.push(sty.dim(&format!("{} not run", other)));
    }
    if cached > 0 {
        head.push(sty.dim(&format!("{} cached", cached)));
    }
    let right = if executed.is_empty() {
        sty.dim("nothing executed")
    } else {
        sty.dim(&format!("{} of {} executed, {} test time", executed.len(), v.len(), fmt_test_time(test_time)))
    };
    let mut out = format!(" {}   {}\n", head.join(sty.dim(" · ").as_str()), right);
    if level == "none" {
        return out;
    }
    let detailed = matches!(level, "detailed" | "testcase");
    let show_passed = level != "terse";
    let labelw = v.iter().filter(|t| show_passed || t.status != "PASSED").map(|t| t.label.width()).max().unwrap_or(0).min(width.saturating_sub(32));
    let mut passed_shown = 0;
    let passed_cap = if detailed { usize::MAX } else { 40 };
    let mut hidden_passed = 0;
    for t in &v {
        let is_pass = t.status == "PASSED";
        if is_pass && !show_passed {
            continue;
        }
        if is_pass {
            passed_shown += 1;
            if passed_shown > passed_cap {
                hidden_passed += 1;
                continue;
            }
        }
        let (mark, word, col) = match t.status.as_str() {
            "PASSED" => ("✔", "PASSED", OK),
            "FLAKY" => ("⚠", "FLAKY", WARN),
            "TIMEOUT" => ("⏱", "TIMEOUT", BAD),
            "FAILED" => ("✘", "FAILED", BAD),
            "REMOTE_FAILURE" => ("✘", "REMOTE FAILURE", BAD),
            "FAILED_TO_BUILD" => ("✘", "FAILED TO BUILD", BAD),
            "INCOMPLETE" => ("◌", "INCOMPLETE", MUTED),
            "BLAZE_HALTED_BEFORE_TESTING" => ("◌", "NOT RUN", MUTED),
            _ => ("◌", "NO STATUS", MUTED),
        };
        let word = if t.skipped { "SKIPPED" } else { word };
        let pad = " ".repeat(labelw.saturating_sub(t.label.width()));
        let time = if t.cached || t.runs == 0 { String::new() } else { fmt_test_time(t.time_ms) };
        let mut extra: Vec<String> = Vec::new();
        if t.cached {
            extra.push("cached".into());
        }
        if t.status == "FLAKY" && t.attempts > 1 {
            extra.push(format!("passed after {} attempts", t.attempts));
        } else if rank(t) == 0 && t.attempts > 1 {
            extra.push(format!("{} attempts", t.attempts));
        }
        if t.failed_cases > 0 {
            extra.push(format!("{} of {} cases failed", t.failed_cases, t.total_cases.max(t.failed_cases)));
        } else if detailed && t.total_cases > 0 {
            extra.push(format!("{} cases", t.total_cases));
        }
        if t.runs > 1 && t.runs > t.attempts {
            extra.push(format!("{} runs", t.runs));
        }
        out.push_str(&format!(
            " {} {}{}  {:<15} {:>6}   {}\n",
            sty.rgb_bold(mark, col),
            if is_pass { t.label.clone() } else { sty.bold(&t.label) },
            pad,
            sty.rgb(word, col),
            sty.dim(&time),
            sty.dim(&extra.join(", "))
        ));
        if !is_pass {
            let cases_cap = if detailed { 50 } else { 8 };
            if !t.failed_case_names.is_empty() {
                let names: Vec<String> = t.failed_case_names.iter().take(cases_cap).map(|c| sty.rgb(c, BAD)).collect();
                let more = if t.failed_case_names.len() > cases_cap { sty.dim(&format!(" … {} more", t.failed_case_names.len() - cases_cap)) } else { String::new() };
                out.push_str(&format!("       {}{}\n", names.join(sty.dim(" · ").as_str()), more));
            }
            for log in t.failed_logs.iter().take(if detailed { 20 } else { 3 }) {
                out.push_str(&format!("       {}\n", sty.dim(&paths.pretty(log))));
            }
        } else if detailed {
            for log in t.passed_logs.iter().take(1) {
                out.push_str(&format!("       {}\n", sty.dim(&paths.pretty(log))));
            }
        }
    }
    if hidden_passed > 0 {
        out.push_str(&format!(" {}\n", sty.dim(&format!("… {} more passed", hidden_passed))));
    }
    if v.iter().any(|t| t.wrong_size) {
        out.push_str(&format!(" {}\n", sty.dim("some tests have a bigger size than they need; --test_verbose_timeout_warnings lists them")));
    }
    out
}

fn fmt_test_time(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        fmt_duration(ms as f64 / 1000.0)
    }
}

#[cfg(test)]
mod report_style_tests {
    use super::*;
    use crate::state::State;

    /// One target with a fresh file output renders exactly as the design spec's reference line:
    /// a dim marker, the label bold, the directory part dim, the name plain, the size dim, the
    /// tick in the report's green and the status word dim, with a reset between every run.
    #[test]
    fn artifact_rows_use_bold_for_names_and_colour_only_for_the_status() {
        crate::text::TRUECOLOR.store(true, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("bazel-hook-report-{}", std::process::id()));
        let pkg = dir.join("bazel-out/bin/pkg");
        std::fs::create_dir_all(pkg.join("docs.rustdoc")).unwrap();
        std::fs::write(pkg.join("unit_test"), b"0123456789").unwrap();
        std::fs::write(pkg.join("docs.rustdoc/index.html"), b"<html>").unwrap();
        let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        let mut s = State::default();
        s.command = Some("build".into());
        s.exec_root = dir.to_string_lossy().into_owned();
        s.workspace = dir.join("nowhere").to_string_lossy().into_owned();
        s.show_result = 1;
        s.t0_ms = now - 1000;
        s.build_success = Some(true);
        s.targets.push(TargetDone { label: "//pkg:unit_test".into(), failed: false, outputs: vec![pkg.join("unit_test").to_string_lossy().into_owned()] });
        s.targets.push(TargetDone { label: "//pkg:docs".into(), failed: false, outputs: vec![pkg.join("docs.rustdoc").to_string_lossy().into_owned()] });
        let out = report(&s, now, true, 120).stderr;
        std::fs::remove_dir_all(&dir).ok();
        let expect_file = " \x1b[38;2;120;130;150m▸\x1b[0m \x1b[1;38;2;214;219;230m//pkg:unit_test\x1b[0m\n     \x1b[38;2;120;130;150mbazel-out/bin/pkg/\x1b[0munit_test      \x1b[38;2;120;130;150m    10 B\x1b[0m   \x1b[38;2;90;220;120m✓\x1b[0m \x1b[38;2;120;130;150mbuilt\x1b[0m\n";
        let expect_dir = " \x1b[38;2;120;130;150m▸\x1b[0m \x1b[1;38;2;214;219;230m//pkg:docs\x1b[0m\n     \x1b[38;2;120;130;150mbazel-out/bin/pkg/\x1b[0mdocs.rustdoc/   \x1b[38;2;120;130;150m     6 B\x1b[0m   \x1b[38;2;90;220;120m✓\x1b[0m \x1b[38;2;120;130;150m1 files\x1b[0m\n";
        assert!(out.contains(expect_file), "{:?}", out);
        assert!(out.contains(expect_dir), "{:?}", out);
        // The header: the tick and verb as before, the time plain, the statistics dim.
        assert!(out.contains("\x1b[38;2;118;230;117m\x1b[1m✔\x1b[0m \x1b[1;38;2;214;219;230mbuild\x1b[0m 1s  \x1b[38;2;120;130;150m"), "{:?}", out);
        assert!(!out.contains("\x1b[1m1s"), "{:?}", out);
        assert!(!out.contains("\x1b[2"), "no faint anywhere: {:?}", out);
        // Every `ESC[1m` must follow a colour run; a bare one would be bold on the default colour.
        assert_eq!(out.matches("\x1b[1m").count(), out.matches("m\x1b[1m").count(), "bold on the default foreground: {:?}", out);
        // The cancelled header and the link, byte for byte as in the design reference.
        s.cancelled_ms = Some(s.t0_ms + 217_000);
        s.bes_url = "https://app.buildbuddy.io/invocation/d5a6".into();
        s.build_success = Some(false);
        let out = report(&s, s.t0_ms + 219_000, true, 120).stderr;
        assert!(out.starts_with("\x1b[38;2;255;90;90m\x1b[1mERROR:\x1b[0m Build did NOT complete successfully\n\x1b[38;2;255;90;90m\x1b[1m✘\x1b[0m \x1b[1;38;2;214;219;230mbuild\x1b[0m \x1b[38;2;255;140;40mcancelled by Ctrl-C after 3m37s, stopped in 2s\x1b[0m  \x1b[38;2;120;130;150m"), "{:?}", out);
        assert!(out.contains("  \x1b[38;2;120;130;150mresults\x1b[0m  \x1b[38;2;140;170;255m\x1b]8;;https://app.buildbuddy.io/invocation/d5a6\x1b\\https://app.buildbuddy.io/invocation/d5a6\x1b]8;;\x1b\\\x1b[0m\n"), "{:?}", out);
    }

    #[test]
    fn path_splits_at_the_last_slash_with_a_trailing_slash_kept_on_the_name() {
        let sty = Sty { color: true };
        crate::text::TRUECOLOR.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(path_runs(&sty, "bazel-bin/a/b"), "\x1b[38;2;120;130;150mbazel-bin/a/\x1b[0mb");
        assert_eq!(path_runs(&sty, "bazel-bin/a/docs/"), "\x1b[38;2;120;130;150mbazel-bin/a/\x1b[0mdocs/");
        assert_eq!(path_runs(&sty, "plain"), "plain");
        assert_eq!(path_runs(&Sty { color: false }, "bazel-bin/a/b"), "bazel-bin/a/b");
    }
}
