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
    pub fn bold(&self, t: &str) -> String {
        self.wrap("1", t)
    }
    pub fn dim(&self, t: &str) -> String {
        self.wrap("2", t)
    }
    pub fn rgb(&self, t: &str, c: (u8, u8, u8)) -> String {
        self.wrap(&format!("38;5;{}", cube(c.0, c.1, c.2)), t)
    }
    pub fn rgb_bold(&self, t: &str, c: (u8, u8, u8)) -> String {
        self.wrap(&format!("38;5;{};1", cube(c.0, c.1, c.2)), t)
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
    let Some(success) = s.build_success else { return rep };
    let width = width.clamp(60, 200);

    rep.stderr.push_str(&completion_line(s, &sty, success));
    rep.stderr.push('\n');
    rep.stderr.push_str(&verdict_line(s, now, &sty, success));
    rep.stderr.push('\n');
    if let Some(line) = sources_line(s, &sty) {
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

fn verdict_line(s: &State, now: i64, sty: &Sty, success: bool) -> String {
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
    format!("{} {} {}  {}", mark, sty.bold(s.command.as_deref().unwrap_or("build")), sty.bold(&fmt_duration(total)), sty.dim(&parts.join(" · ")))
}

/// Where the actions came from: "actions 455 · 449 action cache · 3 remote cache · 2 local exec".
fn sources_line(s: &State, sty: &Sty) -> Option<String> {
    let total = s.cache_total();
    if total == 0 {
        return None;
    }
    let mut parts = Vec::new();
    for src in Source::ALL {
        let n = s.source_count(src);
        if n > 0 {
            let text = format!("{} {}", with_commas(n), src.name());
            parts.push(if src.is_hit() { sty.rgb(&text, OK) } else { sty.rgb(&text, WARN) });
        }
    }
    Some(format!("  {} {}  {}", sty.dim("actions"), sty.bold(&with_commas(total)), parts.join(&sty.dim(" · "))))
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
    File { exec: bool },
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
        use std::os::unix::fs::PermissionsExt;
        let exec = md.permissions().mode() & 0o111 != 0;
        FileRow { path, abs, size: md.len(), kind: Kind::File { exec }, fresh: mtime_ms(&md) >= t0_ms - 2000 }
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
        let name_s = if *is_dir { sty.rgb(&shown_name, ACCENT) } else { shown_name.clone() };
        let detail = if *is_dir {
            format!("{:>9}   {}", fmt_size(*size), sty.dim(&format!("{} file{}", files, if *files == 1 { "" } else { "s" })))
        } else {
            format!("{:>9}   {}", fmt_size(*size), if *fresh { sty.rgb("●", OK) } else { sty.dim("○") })
        };
        out.push_str(&format!("       {} {}{}  {}\n", sty.dim(branch), name_s, pad, detail));
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
    let shown = &targets[..targets.len().min(limit)];

    const PER_TARGET: usize = 20;
    let mut rows: Vec<(String, Vec<FileRow>, usize, u64)> = Vec::new(); // label, files, hidden count, hidden bytes
    let mut n_files = 0u64;
    let mut n_bytes = 0u64;
    let mut n_fresh = 0u64;
    let mut n_failed = 0u64;
    for t in shown {
        if t.failed {
            n_failed += 1;
            rows.push((t.label.clone(), Vec::new(), 0, 0));
            continue;
        }
        let mut files = Vec::new();
        let mut hidden = 0;
        let mut hidden_bytes = 0;
        for (i, p) in t.outputs.iter().enumerate() {
            let r = stat_row(&paths, p, s.t0_ms);
            n_files += match r.kind {
                Kind::Dir { files } => files,
                Kind::Missing => 0,
                _ => 1,
            };
            n_bytes += r.size;
            if r.fresh {
                n_fresh += 1;
            }
            if i < PER_TARGET {
                files.push(r);
            } else {
                hidden += 1;
                hidden_bytes += r.size;
            }
        }
        rows.push((t.label.clone(), files, hidden, hidden_bytes));
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
                out.push_str(&format!(" {} {}  {}\n", sty.rgb("▸", ACCENT), sty.bold(label), sty.dim("no files")));
            }
            continue;
        }
        out.push_str(&format!(" {} {}\n", sty.rgb("▸", ACCENT), sty.bold(label)));
        for f in files {
            let pad = " ".repeat(pathw.saturating_sub(f.path.width()));
            let path = match f.kind {
                Kind::File { exec: true } => sty.rgb_bold(&f.path, ACCENT),
                Kind::Dir { .. } => sty.rgb(&format!("{}/", f.path), ACCENT),
                Kind::Missing => sty.dim(&f.path),
                _ => f.path.clone(),
            };
            let pad = if matches!(f.kind, Kind::Dir { .. }) { pad.get(1..).unwrap_or("").to_string() } else { pad };
            let size = match f.kind {
                Kind::Missing => String::new(),
                _ => fmt_size(f.size),
            };
            let status = match (&f.kind, f.fresh) {
                (Kind::Missing, _) => sty.dim("? missing"),
                (Kind::Dir { files }, true) => sty.rgb(&format!("● {} files, built", files), OK),
                (Kind::Dir { files }, false) => sty.dim(&format!("○ {} files", files)),
                (_, true) => sty.rgb("● built", OK),
                (_, false) => sty.dim("○ unchanged"),
            };
            out.push_str(&format!("     {}{}  {:>9}   {}\n", path, pad, size, status));
            if solo_dir {
                out.push_str(&dir_tree(sty, &f.abs, s.t0_ms, width));
            }
        }
        if *hidden > 0 {
            out.push_str(&format!("     {}\n", sty.dim(&format!("… {} more files ({})", hidden, fmt_size(*hidden_bytes)))));
        }
    }
    if targets.len() > shown.len() {
        out.push_str(&format!(" {}\n", sty.dim(&format!("… {} more targets", targets.len() - shown.len()))));
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
