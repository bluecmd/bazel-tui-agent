//! Synthetic Bazel commands: deterministic event streams shaped like what the Java probe sends
//! for a real build (same events, same order, plausible labels and messages), so the engine can
//! be driven without Bazel. The integration tests and the replay tool (`src/bin/replay.rs`) use
//! them; a trace file from a real build (`BAZEL_HOOK_TRACE`) replays the same way.

use crate::events::{ActionRef, Event, Message, Phase, SpawnBrief, Stamped};

/// What kind of command to synthesise.
#[derive(Clone, Debug)]
pub struct Params {
    /// "build" or "test".
    pub command: &'static str,
    pub actions: usize,
    /// Actions running at once at the peak.
    pub parallel: usize,
    /// Length of the execution phase, roughly.
    pub duration_ms: i64,
    /// One action fails (with captured stderr) and the build with it.
    pub fail: bool,
    /// Ctrl-C this long into the execution phase.
    pub cancel_at_ms: Option<i64>,
    /// Repository downloads with byte progress during loading.
    pub downloads: bool,
    /// A Build Event Service page is announced.
    pub bes: bool,
    /// Labels with East Asian wide characters, emoji and absurd lengths.
    pub wide: bool,
    /// A mix of remote execution and cache hits instead of local sandboxed actions.
    pub remote: bool,
    pub seed: u64,
    /// Where the build's outputs live (`<output_base>/execroot/_main`); `materialize` creates
    /// the files the report lists there.
    pub exec_root: String,
    /// The workspace directory; `materialize` puts the `bazel-bin` style symlinks there, so the
    /// report prints the short paths Bazel's result printer would.
    pub workspace: String,
}

/// The named scenarios and what they are for.
pub const NAMES: &[(&str, &str)] = &[
    ("build", "a 300-action build, 8 wide, 20 s: downloads, a warning, remote and cached work"),
    ("test", "the same build as `bazel test`: passing, flaky and cached test summaries"),
    ("failing", "a compile error half-way; the build stops with the error and its stderr"),
    ("cancelled", "Ctrl-C at 6 s into execution, running actions wind down, exit 8"),
    ("huge", "5,000 actions 64 wide in 30 s, deep target tree, long log"),
    ("tiny", "three genrules in two seconds, no downloads"),
    ("unicode", "CJK and emoji labels, a 200-character label, wide characters in messages"),
    ("local", "everything sandboxed locally, no cache at all"),
];

impl Params {
    pub fn named(name: &str) -> Option<Params> {
        let base = Params { command: "build", actions: 300, parallel: 8, duration_ms: 20_000, fail: false, cancel_at_ms: None, downloads: true, bes: true, wide: false, remote: true, seed: 7, exec_root: "/home/dev/.cache/bazel/_bazel_dev/3f1a9e/execroot/_main".into(), workspace: "/home/dev/acme".into() };
        Some(match name {
            "build" => base,
            "test" => Params { command: "test", ..base },
            "failing" => Params { fail: true, ..base },
            "cancelled" => Params { cancel_at_ms: Some(6_000), ..base },
            "huge" => Params { actions: 5_000, parallel: 64, duration_ms: 30_000, ..base },
            "tiny" => Params { actions: 3, parallel: 1, duration_ms: 2_000, downloads: false, bes: false, remote: false, ..base },
            "unicode" => Params { wide: true, actions: 120, duration_ms: 12_000, ..base },
            "local" => Params { remote: false, downloads: false, bes: false, ..base },
            _ => return None,
        })
    }
}

/// xorshift64*: the same stream for the same seed on every platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    /// Uniform in `lo..hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo { lo } else { lo + self.next() % (hi - lo) }
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

const PACKAGES: &[&str] = &["src/core", "src/net", "src/net/http", "src/storage", "src/storage/index", "src/api", "src/api/v2", "src/cli", "src/util", "src/util/strings", "proto", "third_party/zlib", "third_party/protobuf", "tools/build", "docs"];
const NAMES_LIB: &[&str] = &["core", "net", "http", "storage", "index", "api", "handlers", "cli", "util", "strings", "config", "metrics", "auth", "codec", "scheduler"];
const FILES: &[&str] = &["main", "server", "client", "buffer", "parser", "lexer", "codec", "config", "metrics", "handler", "router", "store", "index", "cache", "worker", "pool", "stream", "frame", "auth", "token"];

const WIDE_LABELS: &[&str] = &[
    "//国際化/テスト:日本語_ターゲット",
    "//emoji:🚀_rocket_library",
    "//한국어/패키지:라이브러리",
    "//中文/库:核心",
    "//mixed/ｆｕｌｌｗｉｄｔｈ:ｔａｒｇｅｔ",
    "//very/long/package/path/that/keeps/going/and/going/far/beyond/what/any/reasonable/terminal/width/can/show/without/truncation/of/some/kind:target_with_an_extremely_long_name_that_also_never_seems_to_end_at_all_really",
    "//combining:e\u{301}le\u{300}ve_cafe\u{301}",
    "//zero-width:a\u{200b}b\u{200b}c",
];

#[derive(Clone)]
struct Mnemonic {
    name: &'static str,
    verb: &'static str,
    ext: &'static str,
    out: &'static str,
    weight: u64,
}

const MNEMONICS: &[Mnemonic] = &[
    Mnemonic { name: "CppCompile", verb: "Compiling", ext: ".cc", out: ".pic.o", weight: 40 },
    Mnemonic { name: "Rustc", verb: "Compiling Rust rlib", ext: ".rs", out: ".rlib", weight: 15 },
    Mnemonic { name: "Javac", verb: "Building", ext: ".java", out: ".jar", weight: 10 },
    Mnemonic { name: "GoCompilePkg", verb: "GoCompilePkg", ext: ".go", out: ".a", weight: 6 },
    Mnemonic { name: "Genrule", verb: "Executing genrule", ext: "", out: ".txt", weight: 10 },
    Mnemonic { name: "CppLink", verb: "Linking", ext: "", out: ".so", weight: 6 },
    Mnemonic { name: "ProtoCompile", verb: "Generating Descriptor Set proto_library", ext: ".proto", out: "-descriptor-set.proto.bin", weight: 5 },
    Mnemonic { name: "TypeScriptCompile", verb: "Compiling TypeScript", ext: ".ts", out: ".js", weight: 4 },
    Mnemonic { name: "PythonZipper", verb: "Building Python zip", ext: ".py", out: ".zip", weight: 2 },
    Mnemonic { name: "Middleman", verb: "Creating runfiles tree", ext: "", out: ".runfiles", weight: 2 },
];

fn pick_mnemonic(rng: &mut Rng) -> &'static Mnemonic {
    let total: u64 = MNEMONICS.iter().map(|m| m.weight).sum();
    let mut r = rng.range(0, total);
    for m in MNEMONICS {
        if r < m.weight {
            return m;
        }
        r -= m.weight;
    }
    &MNEMONICS[0]
}

fn with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

struct Plan {
    action: ActionRef,
    /// Start offset into the execution phase.
    start: i64,
    end: i64,
    /// Local action-cache hit: Bazel skips the action without running it.
    cached: bool,
    runner: &'static str,
    cache_hit: bool,
    remote: bool,
    fails: bool,
    stderr: Option<String>,
}

fn msg(kind: &str, text: &str) -> Event {
    Event::Message(Message { kind: kind.into(), text: text.into(), location: String::new(), tag: String::new(), stdout: String::new(), stderr: String::new() })
}

fn progress(loading: &str, detail: &str, analysis: &str, execution: &str) -> Event {
    Event::Progress { loading: loading.into(), loading_detail: detail.into(), analysis: analysis.into(), execution: execution.into() }
}

/// The events of one command, stamped from `t0_ms` on, in time order.
pub fn events(p: &Params, t0_ms: i64) -> Vec<Stamped> {
    let mut rng = Rng::new(p.seed);
    let mut ev: Vec<Stamped> = Vec::new();
    let push = |ev: &mut Vec<Stamped>, t: i64, e: Event| ev.push(Stamped { t_ms: t0_ms + t, event: e });

    // ---- targets and actions
    let n_targets = (p.actions / 25).clamp(3, 400);
    let mut labels: Vec<String> = (0..n_targets).map(|i| format!("//{}:{}", PACKAGES[i % PACKAGES.len()], if i < NAMES_LIB.len() { NAMES_LIB[i].to_string() } else { format!("{}_{}", NAMES_LIB[i % NAMES_LIB.len()], i / NAMES_LIB.len()) })).collect();
    if p.wide {
        let n = labels.len();
        for (i, w) in WIDE_LABELS.iter().enumerate() {
            labels[i % n] = (*w).to_string();
        }
    }
    let config = "k8-fastbuild";
    let mut plans: Vec<Plan> = Vec::with_capacity(p.actions);
    for i in 0..p.actions {
        let label = labels[i % labels.len()].clone();
        let m = pick_mnemonic(&mut rng);
        let pkg = label.trim_start_matches('/').split(':').next().unwrap_or("pkg").to_string();
        let name = label.rsplit(':').next().unwrap_or("x").to_string();
        let file = format!("{}_{}", FILES[i % FILES.len()], i);
        let id = format!("bazel-out/{}/bin/{}/_objs/{}/{}{}", config, pkg, name, file, m.out);
        let description = if m.ext.is_empty() { format!("{} {}", m.verb, label) } else { format!("{} {}/{}{}", m.verb, pkg, file, m.ext) };
        let fails = p.fail && i == p.actions * 6 / 10;
        let cached = p.remote && !fails && rng.chance(25);
        let (runner, cache_hit, remote): (&str, bool, bool) = if fails {
            ("linux-sandbox", false, false)
        } else if p.remote {
            match rng.range(0, 100) {
                0..=34 => ("remote", false, true),
                35..=54 => ("remote cache hit", true, true),
                55..=64 => ("disk cache hit", true, false),
                65..=79 => ("linux-sandbox", false, false),
                _ => ("worker", false, false),
            }
        } else if rng.chance(20) {
            ("worker", false, false)
        } else {
            ("linux-sandbox", false, false)
        };
        let stderr = if fails {
            Some(format!("{}/{}{}:3:5: error: 'frobnicate' was not declared in this scope\n    3 |     frobnicate(x);\n      |     ^~~~~~~~~~\n", pkg, file, m.ext))
        } else if m.name == "Genrule" && rng.chance(30) {
            Some(format!("step {} of {}\n", i, name))
        } else if m.name == "CppCompile" && rng.chance(4) {
            Some(format!("{}/{}{}:12:7: warning: unused variable 'tmp' [-Wunused-variable]\n", pkg, file, m.ext))
        } else {
            None
        };
        plans.push(Plan {
            action: ActionRef { id, mnemonic: m.name.into(), label, target_kind: format!("{} rule", if m.name == "Genrule" { "genrule" } else { "cc_library" }), config: config.into(), description },
            start: 0,
            end: 0,
            cached,
            runner,
            cache_hit,
            remote,
            fails,
            stderr,
        });
    }
    // Schedule: `parallel` slots, durations drawn then scaled so the phase takes about `duration_ms`.
    let durations: Vec<i64> = plans.iter().map(|pl| if pl.cached { rng.range(1, 30) as i64 } else if pl.cache_hit { rng.range(20, 200) as i64 } else { rng.range(150, 3_000) as i64 }).collect();
    let work: i64 = durations.iter().sum();
    let scale = (p.duration_ms as f64 * p.parallel as f64 / work.max(1) as f64).min(1.0);
    let mut slots: Vec<i64> = vec![0; p.parallel.max(1)];
    for (i, pl) in plans.iter_mut().enumerate() {
        let (si, free_at) = slots.iter().enumerate().min_by_key(|(_, t)| **t).map(|(i, t)| (i, *t)).unwrap_or((0, 0));
        pl.start = free_at + rng.range(0, 40) as i64;
        pl.end = pl.start + ((durations[i] as f64 * scale) as i64).max(1);
        slots[si] = pl.end;
    }

    // ---- the command starts
    let command_id = format!("{:08x}-4c2e-4e0b-9d1f-{:012x}", rng.next() as u32, rng.next() & 0xffff_ffff_ffff);
    let mut targets_arg: Vec<String> = vec!["//...".into()];
    if p.command == "test" {
        targets_arg = vec!["//src/...".into(), "//proto/...".into()];
    }
    push(&mut ev, 0, Event::CommandStarted {
        command: p.command.into(),
        command_id: command_id.clone(),
        workspace: p.workspace.clone(),
        cwd: p.workspace.clone(),
        args: targets_arg.clone(),
        start_ms: t0_ms - 180,
        bazel_version: "9.3.0".into(),
        agent_jar: "/home/dev/.cache/bazel-tui-agent/bazel-tui-agent.jar".into(),
        agent_jar_mtime_ms: t0_ms - 86_400_000,
        exec_root: p.exec_root.clone(),
        show_result: 1,
        test_summary: "short".into(),
        agent_version: "replay".into(),
        bes_url: if p.bes { format!("https://app.buildbuddy.io/invocation/{}", command_id) } else { String::new() },
    });
    let mut sys_t = 10;
    let mut heap = 320u64;
    let system = |rng: &mut Rng, heap: &mut u64, running: u64| {
        *heap = (*heap as i64 + rng.range(0, 120) as i64 - 50).clamp(200, 3_900) as u64;
        Event::System { heap_used_mb: *heap, heap_max_mb: 4_096, load: 1.5 + running as f64 * 0.9 + rng.range(0, 10) as f64 / 10.0, cpus: 16 }
    };
    push(&mut ev, sys_t, system(&mut rng, &mut heap, 0));
    if p.bes {
        push(&mut ev, 15, msg("INFO", &format!("Streaming build results to: https://app.buildbuddy.io/invocation/{}", command_id)));
    }
    push(&mut ev, 20, msg("PROGRESS", "Computing main repo mapping"));
    push(&mut ev, 60, Event::BuildStarted { targets: targets_arg.clone(), build_id: command_id.clone() });
    push(&mut ev, 70, Event::Phase { phase: Phase::Loading });

    // ---- loading, with downloads
    let mut t = 80;
    let load_end = if p.downloads { 2_600 } else { 500 };
    let downloads: Vec<(&str, u64)> = if p.downloads {
        vec![("https://static.rust-lang.org/dist/2026-09-15/rust-std-1.93.0-x86_64-unknown-linux-gnu.tar.xz", 42_000_000), ("https://github.com/protocolbuffers/protobuf/releases/download/v33.1/protoc-33.1-linux-x86_64.zip", 3_400_000), ("https://mirror.bazel.build/github.com/madler/zlib/archive/v1.3.1.tar.gz", 1_500_000)]
    } else {
        Vec::new()
    };
    let mut pk = 0u64;
    while t < load_end {
        pk += rng.range(3, 20);
        let pkg = PACKAGES[(pk as usize) % PACKAGES.len()];
        push(&mut ev, t, progress(&format!("Loading: {} packages loaded", pk), &format!("currently loading: {} ... ({} packages)", pkg, rng.range(1, 6)), "", ""));
        if p.downloads {
            for (i, (url, total)) in downloads.iter().enumerate() {
                let t_start = 150 + i as i64 * 300;
                let t_end = t_start + 1_500 + i as i64 * 200;
                if t >= t_start && t < t_end {
                    let frac = (t - t_start) as f64 / (t_end - t_start) as f64;
                    let bytes = (*total as f64 * frac) as u64;
                    push(&mut ev, t, msg("PROGRESS", &format!("Downloading {}: {} bytes", url, with_commas(bytes))));
                    push(&mut ev, t, Event::Fetch { resource: (*url).into(), progress: format!("{} bytes", with_commas(bytes)), finished: false, bytes, total: *total });
                } else if t >= t_end && t < t_end + 100 {
                    push(&mut ev, t, Event::Fetch { resource: (*url).into(), progress: String::new(), finished: true, bytes: *total, total: *total });
                }
            }
        }
        if t >= sys_t + 1_000 {
            sys_t = t;
            push(&mut ev, t, system(&mut rng, &mut heap, 0));
        }
        t += 100;
    }
    push(&mut ev, t, Event::LoadingComplete { targets: n_targets as u64 });
    t += 10;
    push(&mut ev, t, Event::Phase { phase: Phase::Analysis });
    let configured = n_targets as u64 * 7 + 13;
    let a_end = t + 1_400;
    let mut conf = 0u64;
    while t < a_end {
        conf = (conf + rng.range(20, 90)).min(configured);
        push(&mut ev, t, progress("", "", &format!("Analyzing: {} targets ({} packages loaded, {} targets configured)", n_targets, pk, conf), ""));
        if t >= sys_t + 1_000 {
            sys_t = t;
            push(&mut ev, t, system(&mut rng, &mut heap, 0));
        }
        t += 150;
    }
    push(&mut ev, t, Event::AnalysisComplete { targets: n_targets as u64, targets_configured: configured, actions: p.actions as u64, time_ms: (t - load_end) as u64 });
    push(&mut ev, t, msg("INFO", &format!("Analyzed {} targets ({} packages loaded, {} targets configured).", n_targets, pk, configured)));
    if p.command == "test" {
        push(&mut ev, t + 1, Event::TestFilteringComplete { targets: n_targets as u64, tests: (n_targets as u64 / 2).max(1), skipped: 0 });
    }
    t += 20;
    push(&mut ev, t, Event::Phase { phase: Phase::Execution });
    let te0 = t;

    // ---- execution
    let stop_at = p.cancel_at_ms.map(|c| te0 + c);
    let mut failed_at: Option<i64> = None;
    let mut timeline: Vec<(i64, Event)> = Vec::new();
    // (running from, until) per executed action and the completion times, for the counters.
    let mut runs: Vec<(i64, i64)> = Vec::new();
    let mut dones: Vec<i64> = Vec::new();
    for pl in &plans {
        let ts = te0 + pl.start;
        let te = te0 + pl.end;
        if let Some(f) = failed_at {
            if ts > f + 300 {
                continue; // Bazel stops scheduling after the failure
            }
        }
        if let Some(s) = stop_at {
            if ts > s {
                continue;
            }
        }
        if pl.cached {
            timeline.push((ts, Event::ActionCached { action: pl.action.clone() }));
            dones.push(ts);
            continue;
        }
        timeline.push((ts, Event::ActionStarted { action: pl.action.clone(), start_ns: (t0_ms + ts) * 1_000_000 }));
        timeline.push((ts + 1, Event::ActionScheduling { action: pl.action.clone(), strategy: pl.runner.into() }));
        if pl.cache_hit {
            timeline.push((ts + 2, Event::ActionCacheCheck { action: pl.action.clone(), strategy: pl.runner.into() }));
        } else {
            timeline.push((ts + 5, Event::ActionRunning { action: pl.action.clone(), strategy: pl.runner.into() }));
        }
        let te = match stop_at {
            Some(s) if te > s => s + rng.range(50, 900) as i64, // SIGKILLed while unwinding
            _ => te,
        };
        if !pl.cache_hit {
            runs.push((ts + 5, te));
        }
        dones.push(te);
        if pl.action.mnemonic == "Rustc" && te - ts > 1_000 {
            timeline.push((ts + (te - ts) / 2, Event::ActionProgress { action: pl.action.clone(), progress_id: pl.action.id.clone(), progress: "codegen".into(), finished: false }));
        }
        if pl.fails && stop_at.map_or(true, |s| te <= s) {
            failed_at = Some(te);
            let m = Message {
                kind: "ERROR".into(),
                text: format!("{} failed: (Exit 1): gcc failed: error executing CppCompile command (from target {}) /usr/bin/gcc -U_FORTIFY_SOURCE -fstack-protector -Wall ... (remaining 32 arguments skipped)", pl.action.description, pl.action.label),
                location: format!("{}/BUILD:12:11", pl.action.label.trim_start_matches('/').split(':').next().unwrap_or("")),
                tag: String::new(),
                stdout: String::new(),
                stderr: pl.stderr.clone().unwrap_or_default(),
            };
            timeline.push((te, Event::Message(m)));
            timeline.push((te, Event::ActionExecuted { action: pl.action.clone(), success: false, error: "Exit 1".into() }));
            continue;
        }
        if let Some(err) = &pl.stderr {
            timeline.push((te - 1, Event::Message(Message { kind: "INFO".into(), text: format!("From {}:", pl.action.description), location: String::new(), tag: String::new(), stdout: String::new(), stderr: err.clone() })));
            if err.contains("warning:") {
                timeline.push((te - 1, Event::Message(Message { kind: "WARNING".into(), text: err.trim_end().into(), location: String::new(), tag: String::new(), stdout: String::new(), stderr: String::new() })));
            }
        }
        timeline.push((te, Event::Spawn { id: pl.action.id.clone(), mnemonic: pl.action.mnemonic.clone(), runner: pl.runner.into(), runner_subtype: String::new(), host: if pl.remote { "rbe-worker-07".into() } else { String::new() }, status: "SUCCESS".into(), exit_code: 0, cache_hit: pl.cache_hit, remote: pl.remote, wall_ms: te - ts }));
        timeline.push((te, Event::ActionResult { action: pl.action.clone(), spawns: vec![SpawnBrief { runner: pl.runner.into(), cache_hit: pl.cache_hit, remote: pl.remote, wall_ms: te - ts, status: "SUCCESS".into() }] }));
        timeline.push((te, Event::ActionCompleted { action: pl.action.clone(), start_ns: (t0_ms + ts) * 1_000_000, finish_ns: (t0_ms + te) * 1_000_000 }));
    }
    // Progress lines and system samples through the phase; the running count is derived.
    let exec_end = timeline.iter().map(|(t, _)| *t).max().unwrap_or(te0).max(te0 + 1);
    let mut tt = te0 + 100;
    let total = p.actions as u64;
    while tt <= exec_end {
        let finished = dones.iter().filter(|d| **d <= tt).count() as u64;
        let running = runs.iter().filter(|(a, b)| *a <= tt && tt < *b).count() as u64;
        timeline.push((tt, progress("", "", "", &format!("[{} / {}] {} action{} running", with_commas(finished), with_commas(total), running, if running == 1 { "" } else { "s" }))));
        if tt >= sys_t + 1_000 {
            sys_t = tt;
            timeline.push((tt, system(&mut rng, &mut heap, running)));
        }
        tt += 200;
    }
    if let Some(s) = stop_at {
        timeline.push((s, Event::Cancelled));
        timeline.push((s + 5, msg("ERROR", "build interrupted")));
    }
    timeline.sort_by_key(|(t, _)| *t);
    for (t, e) in timeline {
        push(&mut ev, t, e);
    }
    t = exec_end + 20;

    // ---- targets, tests, the end
    if failed_at.is_none() && stop_at.is_none() {
        for (i, l) in labels.iter().enumerate() {
            let pkg = l.trim_start_matches('/').split(':').next().unwrap_or("pkg");
            let name = l.rsplit(':').next().unwrap_or("x");
            let outputs = match i % 3 {
                0 => vec![format!("bazel-out/{}/bin/{}/lib{}.a", config, pkg, name), format!("bazel-out/{}/bin/{}/lib{}.so", config, pkg, name)],
                1 => vec![format!("bazel-out/{}/bin/{}/{}", config, pkg, name)],
                _ => vec![format!("bazel-out/{}/bin/{}/{}.jar", config, pkg, name), format!("bazel-out/{}/bin/{}/{}-src.jar", config, pkg, name), format!("bazel-out/{}/bin/{}/{}.runfiles", config, pkg, name)],
            };
            push(&mut ev, t, Event::TargetComplete { label: l.clone(), failed: false, outputs });
            t += 3;
        }
        if p.command == "test" {
            for (i, l) in labels.iter().enumerate().filter(|(i, _)| i % 2 == 0) {
                let (status, failed_cases, attempts) = match i % 7 {
                    3 => ("FLAKY", 0, 2),
                    5 if p.fail => ("FAILED", 2, 1),
                    _ => ("PASSED", 0, 1),
                };
                let cached = i % 4 == 0;
                push(&mut ev, t, Event::TestSummary {
                    label: format!("{}_test", l),
                    status: status.into(),
                    runs: 1,
                    cached,
                    skipped: false,
                    failed_cases,
                    total_cases: 12,
                    time_ms: rng.range(100, 9_000),
                    failed_logs: if status == "FAILED" { vec![format!("/home/dev/.cache/bazel/_bazel_dev/3f1a9e/execroot/_main/bazel-out/{}/testlogs/{}/{}_test/test.log", config, l.trim_start_matches('/').split(':').next().unwrap_or(""), l.rsplit(':').next().unwrap_or(""))] } else { Vec::new() },
                    passed_logs: Vec::new(),
                    failed_case_names: if status == "FAILED" { vec!["ParserTest.rejects_unterminated_string".into(), "ParserTest.handles_escapes".into()] } else { Vec::new() },
                    attempts,
                    wrong_size: false,
                });
                t += 4;
            }
        }
    } else if let Some(f) = failed_at {
        let pl = plans.iter().find(|pl| pl.fails).expect("failing plan");
        push(&mut ev, f + 1, Event::TargetComplete { label: pl.action.label.clone(), failed: true, outputs: Vec::new() });
    }
    let success = failed_at.is_none() && stop_at.is_none();
    push(&mut ev, t, Event::BuildComplete { success, catastrophe: false, exit_code: if stop_at.is_some() { 8 } else if success { 0 } else { 1 }, stop_ms: t0_ms + t });
    if p.bes {
        push(&mut ev, t + 5, msg("INFO", &format!("Streaming build results to: https://app.buildbuddy.io/invocation/{}", command_id)));
    }
    push(&mut ev, t + 40, Event::CommandFinished);
    ev.sort_by_key(|s| s.t_ms);
    ev
}

/// Creates the output files the stream's `TargetComplete` events name, under `exec_root`, so
/// the report at the end finds them (sizes and freshness as after a real build). Sparse files,
/// so even large sizes cost nothing; `.runfiles` outputs become directories with a few files.
pub fn materialize(events: &[Stamped], exec_root: &std::path::Path) -> std::io::Result<()> {
    let mut rng = Rng::new(99);
    if let Some(Event::CommandStarted { workspace, .. }) = events.first().map(|s| &s.event) {
        // The convenience symlinks of a workspace that has been built in.
        if std::fs::create_dir_all(workspace).is_ok() {
            for (name, target) in [("bazel-bin", "bazel-out/k8-fastbuild/bin"), ("bazel-testlogs", "bazel-out/k8-fastbuild/testlogs"), ("bazel-out", "bazel-out")] {
                let link = std::path::Path::new(workspace).join(name);
                let _ = std::fs::remove_file(&link);
                let _ = std::os::unix::fs::symlink(exec_root.join(target), &link);
            }
        }
    }
    for s in events {
        let Event::TargetComplete { outputs, .. } = &s.event else { continue };
        for out in outputs {
            let path = exec_root.join(out);
            if out.ends_with(".runfiles") {
                std::fs::create_dir_all(path.join("_main/src"))?;
                for name in ["MANIFEST", "_main/src/data.txt", "_main/src/tool"] {
                    let f = std::fs::File::create(path.join(name))?;
                    f.set_len(rng.range(200, 50_000))?;
                }
                continue;
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let f = std::fs::File::create(&path)?;
            let size = match out.rsplit('.').next().unwrap_or("") {
                "so" => rng.range(100_000, 9_000_000),
                "a" => rng.range(20_000, 4_000_000),
                "jar" => rng.range(5_000, 900_000),
                _ => rng.range(500, 120_000),
            };
            f.set_len(size)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_scenario_starts_and_finishes_in_order() {
        for (name, _) in NAMES {
            let p = Params::named(name).expect(name);
            let ev = events(&p, 1_000_000);
            assert!(matches!(ev.first().map(|s| &s.event), Some(Event::CommandStarted { .. })), "{name}");
            assert!(matches!(ev.last().map(|s| &s.event), Some(Event::CommandFinished)), "{name}");
            assert!(ev.windows(2).all(|w| w[0].t_ms <= w[1].t_ms), "{name}: out of order");
            let completed = ev.iter().filter(|s| matches!(s.event, Event::ActionCompleted { .. } | Event::ActionCached { .. })).count();
            if !p.fail && p.cancel_at_ms.is_none() {
                assert_eq!(completed, p.actions, "{name}");
            }
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let p = Params::named("build").unwrap();
        let a = serde_json::to_string(&events(&p, 5)).unwrap();
        let b = serde_json::to_string(&events(&p, 5)).unwrap();
        assert_eq!(a, b);
    }
}
