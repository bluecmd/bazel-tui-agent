//! bazelhook-replay: drives the engine from a synthetic scenario or a recorded trace and writes
//! its bytes to the terminal it runs in, exactly as the Java side writes them to the Bazel
//! client's streams. The terminal matrix (test/terminals) runs this inside real terminal
//! emulators, resizes them and takes screenshots; it also records asciinema casts for terminals
//! the Rust library cannot run in (Windows Terminal).
//!
//!   bazelhook-replay [--scenario NAME | --trace FILE] [--speed F] [--size CxR] [--cast FILE]
//!                    [--log FILE] [--no-color] [--hold SECS] [--probe] [--list]
//!
//! Size and keys come from the terminal on stdin/stderr (the engine opens its own pty the way
//! it opens the Bazel client's), so resizing the window is seen by the engine on its next poll.
//! `--size` makes it headless (no terminal needed), for recording casts. `--log` appends what
//! happened and when: every size the engine saw, the switches between the inline block and the
//! dashboard, the report, the exit; the matrix asserts on it. `--probe` draws colour bands with
//! known SGR sequences instead of a build, so a screenshot shows how the terminal renders them.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bazelhook_ui::engine::Engine;
use bazelhook_ui::events::{Event, Stamped};
use bazelhook_ui::pty::Pty;
use bazelhook_ui::scenario::{self, Params};
use bazelhook_ui::terminal::{Output, Terminal};
use bazelhook_ui::text;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

struct Opts {
    scenario: String,
    trace: Option<String>,
    speed: f64,
    size: Option<(u16, u16)>,
    cast: Option<String>,
    log: Option<String>,
    color: bool,
    hold: f64,
    probe: bool,
}

fn usage() -> ! {
    eprintln!("usage: bazelhook-replay [--scenario NAME | --trace FILE] [--speed F] [--size CxR] [--cast FILE] [--log FILE] [--no-color] [--hold SECS] [--probe] [--list]");
    std::process::exit(2)
}

fn parse_args() -> Opts {
    let mut o = Opts { scenario: "build".into(), trace: None, speed: 1.0, size: None, cast: None, log: None, color: true, hold: 0.0, probe: false };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--scenario" => o.scenario = val(),
            "--trace" => o.trace = Some(val()),
            "--speed" => o.speed = val().parse().unwrap_or_else(|_| usage()),
            "--size" => {
                let v = val();
                let (c, r) = v.split_once('x').unwrap_or_else(|| usage());
                o.size = Some((c.parse().unwrap_or_else(|_| usage()), r.parse().unwrap_or_else(|_| usage())));
            }
            "--cast" => o.cast = Some(val()),
            "--log" => o.log = Some(val()),
            "--no-color" => o.color = false,
            "--hold" => o.hold = val().parse().unwrap_or_else(|_| usage()),
            "--probe" => o.probe = true,
            "--list" => {
                for (n, what) in scenario::NAMES {
                    println!("{:<10} {}", n, what);
                }
                std::process::exit(0)
            }
            _ => usage(),
        }
    }
    o
}

/// The two client streams of the terminal we run in, plus the optional cast and log files.
struct Sink {
    cast: Option<std::fs::File>,
    log: Option<std::fs::File>,
    started: Instant,
    /// Headless (`--size`): nothing goes to our own streams, only to the cast.
    quiet: bool,
}

impl Sink {
    fn write(&mut self, out: &Output) {
        if out.stderr.is_empty() && out.stdout.is_empty() {
            return;
        }
        if !self.quiet {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(&out.stderr);
            let _ = e.flush();
            let mut s = std::io::stdout().lock();
            let _ = s.write_all(&out.stdout);
            let _ = s.flush();
        }
        if let Some(c) = self.cast.as_mut() {
            let t = self.started.elapsed().as_secs_f64();
            for bytes in [&out.stderr, &out.stdout] {
                if !bytes.is_empty() {
                    let data = serde_json::to_string(&String::from_utf8_lossy(bytes)).unwrap_or_default();
                    let _ = writeln!(c, "[{:.6}, \"o\", {}]", t, data);
                }
            }
        }
    }

    fn note(&mut self, what: &str) {
        if let Some(l) = self.log.as_mut() {
            let _ = writeln!(l, "t={:.3} {}", self.started.elapsed().as_secs_f64(), what);
            let _ = l.flush();
        }
    }
}

fn load_trace(path: &str) -> Vec<Stamped> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("bazelhook-replay: {}: {}", path, e);
        std::process::exit(1)
    });
    let mut ev: Vec<Stamped> = Vec::new();
    let mut started = false;
    for line in text.lines() {
        let Ok(s) = serde_json::from_str::<Stamped>(line) else { continue };
        // A trace may hold several commands; the first one that builds is replayed.
        if matches!(s.event, Event::CommandStarted { .. }) {
            if started {
                break;
            }
            started = true;
        }
        if started {
            ev.push(s);
        }
    }
    ev
}

/// The colour bands of `--probe`: name, SGR parameters, and the colour a faithful terminal shows
/// for the band (None where it depends on the terminal's own scheme).
fn probe_bands() -> Vec<(&'static str, String, Option<(u8, u8, u8)>)> {
    let tc = |c: (u8, u8, u8)| format!("38;2;{};{};{}", c.0, c.1, c.2);
    let t = text::REPORT_TEXT;
    let m = text::REPORT_MUTED;
    vec![
        ("marker red", "38;2;255;0;0".into(), Some((255, 0, 0))),
        ("marker green", "38;2;0;255;0".into(), Some((0, 255, 0))),
        ("marker blue", "38;2;0;0;255".into(), Some((0, 0, 255))),
        ("text bold (1;38;2)", format!("1;{}", tc(t)), Some(t)),
        ("muted (38;2)", tc(m), Some(m)),
        ("link (38;2)", tc(text::LINK), Some(text::LINK)),
        ("ok (38;2)", tc(text::REPORT_OK), Some(text::REPORT_OK)),
        ("bad bold (1;38;2)", format!("1;{}", tc(text::BAD)), Some(text::BAD)),
        ("muted faint (2;38;2)", format!("2;{}", tc(m)), None),
        ("muted 256 (38;5)", format!("38;5;{}", text::cube(m.0, m.1, m.2)), None),
        ("default fg", String::new(), None),
        ("default bold (1)", "1".into(), None),
        ("default faint (2)", "2".into(), None),
        ("background (48;2)", format!("48;2;{};{};{}", 67, 160, 71), Some((67, 160, 71))),
    ]
}

fn probe(cols: u16, rows: u16, sink: &mut Sink) {
    let bands = probe_bands();
    let per = ((rows as usize).saturating_sub(1) / bands.len()).max(1);
    let mut out = Output::default();
    out.stderr.extend_from_slice(b"\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J");
    let mut json = String::from("probe [");
    for (i, (name, sgr, expect)) in bands.iter().enumerate() {
        let fill: String = if sgr.starts_with("48;") { " ".repeat(cols as usize) } else { "█".repeat(cols as usize) };
        for r in 0..per {
            let y = i * per + r + 1;
            if y >= rows as usize {
                break;
            }
            out.stderr.extend_from_slice(format!("\x1b[{};1H\x1b[0m\x1b[{}m{}\x1b[0m", y + 1, sgr, fill).as_bytes());
        }
        if i > 0 {
            json.push(',');
        }
        let exp = match expect {
            Some((r, g, b)) => format!("[{},{},{}]", r, g, b),
            None => "null".into(),
        };
        json.push_str(&format!("{{\"name\":{},\"sgr\":{},\"rows\":[{},{}],\"expect\":{}}}", serde_json::to_string(name).unwrap(), serde_json::to_string(sgr).unwrap(), i * per + 1, i * per + per, exp));
    }
    json.push(']');
    sink.write(&out);
    sink.note(&format!("size {}x{}", cols, rows));
    sink.note(&json);
    sink.note("probe");
}

fn main() {
    let o = parse_args();
    let truecolor = match std::env::var("BAZEL_HOOK_TRUECOLOR").ok().as_deref() {
        Some("1") => true,
        Some(_) => false,
        None => matches!(std::env::var("COLORTERM").ok().as_deref(), Some("truecolor") | Some("24bit")),
    };
    text::TRUECOLOR.store(truecolor, Ordering::Relaxed);

    // A panic would otherwise only show on the terminal being tested; the log gets it too.
    if let Some(path) = o.log.clone() {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "panic {}", info);
            }
            default(info);
        }));
    }
    let mut sink = Sink {
        cast: o.cast.as_ref().map(|p| std::fs::File::create(p).unwrap_or_else(|e| {
            eprintln!("bazelhook-replay: {}: {}", p, e);
            std::process::exit(1)
        })),
        log: o.log.as_ref().map(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap_or_else(|e| {
            eprintln!("bazelhook-replay: {}: {}", p, e);
            std::process::exit(1)
        })),
        started: Instant::now(),
        quiet: o.size.is_some(),
    };

    // The terminal: our own, found through /proc like the Bazel client's; or a fixed size.
    let pty = if o.size.is_some() { None } else { Pty::open(std::process::id() as i64) };
    if pty.is_none() && o.size.is_none() {
        eprintln!("bazelhook-replay: no terminal on stdin/stderr; use --size CxR to run headless");
        std::process::exit(1);
    }
    let (cols, rows) = o.size.or_else(|| pty.as_ref().and_then(|p| p.size())).unwrap_or((80, 24));
    if let Some(c) = sink.cast.as_mut() {
        let _ = writeln!(c, "{{\"version\": 2, \"width\": {}, \"height\": {}, \"timestamp\": 0, \"env\": {{\"TERM\": \"xterm-256color\"}}, \"title\": \"bazel-hook replay {}\"}}", cols, rows, if o.probe { "probe".to_string() } else { o.scenario.clone() });
    }
    sink.note(&format!("start {} truecolor={} color={}", if o.probe { "probe".into() } else { o.trace.clone().unwrap_or_else(|| o.scenario.clone()) }, truecolor, o.color));

    if o.probe {
        probe(cols, rows, &mut sink);
        std::thread::sleep(Duration::from_secs_f64(o.hold.max(0.5)));
        let mut out = Output::default();
        out.stderr.extend_from_slice(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        sink.write(&out);
        drop(pty);
        sink.note("exit 0");
        return;
    }

    // Synthetic builds get a scratch exec root with their output files, so the report at the
    // end lists sizes and freshness as it would after a real build.
    let scratch = std::env::temp_dir().join(format!("bazelhook-replay-{}", std::process::id()));
    let events = match &o.trace {
        Some(path) => load_trace(path),
        None => {
            let mut p = Params::named(&o.scenario).unwrap_or_else(|| {
                eprintln!("bazelhook-replay: unknown scenario {} (--list)", o.scenario);
                std::process::exit(2)
            });
            p.exec_root = scratch.join("execroot/_main").to_string_lossy().into_owned();
            p.workspace = scratch.join("workspace").to_string_lossy().into_owned();
            let ev = scenario::events(&p, 1_700_000_000_000);
            if let Err(e) = scenario::materialize(&ev, std::path::Path::new(&p.exec_root)) {
                eprintln!("bazelhook-replay: cannot create outputs under {}: {}", p.exec_root, e);
            }
            ev
        }
    };
    if events.is_empty() {
        eprintln!("bazelhook-replay: nothing to replay");
        std::process::exit(1);
    }
    let t0 = events[0].t_ms;
    let mut terminal = Terminal::new(cols as usize, o.color, 12, pty, std::process::id() as i64);
    if let Some((c, r)) = o.size {
        terminal.reported_size(c, r);
    }
    let mut engine = Engine::new(Some(terminal), None);
    // SAFETY: a plain signal handler that sets a flag.
    unsafe { libc::signal(libc::SIGINT, on_sigint as extern "C" fn(libc::c_int) as usize) };

    let mut next = 0;
    let mut last_size: Option<(u16, u16)> = None;
    let mut last_alt = false;
    let mut cancelled = false;
    let speed = if o.speed > 0.0 { o.speed } else { 1.0 };
    let clock = |started: Instant| t0 + (started.elapsed().as_secs_f64() * 1000.0 * speed) as i64;
    loop {
        let now = clock(sink.started);
        while next < events.len() && events[next].t_ms <= now {
            engine.event(&events[next]);
            next += 1;
        }
        if INTERRUPTED.swap(false, Ordering::Relaxed) && !cancelled {
            cancelled = true;
            engine.event(&Stamped { t_ms: now, event: Event::Cancelled });
            engine.event(&Stamped { t_ms: now, event: Event::Message(bazelhook_ui::events::Message { kind: "ERROR".into(), text: "build interrupted".into(), location: String::new(), tag: String::new(), stdout: String::new(), stderr: String::new() }) });
            // Whatever is still queued is dropped; the running actions end on the next tick.
            next = events.len();
            sink.note("ctrl-c");
        }
        let out = engine.poll(now);
        sink.write(&out);
        if let Some(t) = engine.terminal.as_ref() {
            let size = t.current_size();
            if size != last_size {
                last_size = size;
                match size {
                    Some((c, r)) => sink.note(&format!("size {}x{}", c, r)),
                    None => sink.note("size unknown"),
                }
            }
            if t.on_alt_screen() != last_alt {
                last_alt = t.on_alt_screen();
                sink.note(if last_alt { "dashboard" } else { "inline" });
            }
        }
        if next >= events.len() {
            break;
        }
        // A poll per 100 ms of the command's own time, so faster replays keep the same frames.
        std::thread::sleep(Duration::from_secs_f64((0.1 / speed).max(0.005)));
    }
    let now = clock(sink.started);
    if cancelled {
        engine.event(&Stamped { t_ms: now, event: Event::BuildComplete { success: false, catastrophe: false, exit_code: 8, stop_ms: now } });
        engine.event(&Stamped { t_ms: now, event: Event::CommandFinished });
    }
    let out = engine.finish(now);
    sink.write(&out);
    sink.note("report");
    if o.hold > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(o.hold));
    }
    drop(engine);
    let _ = std::fs::remove_dir_all(&scratch);
    sink.note(&format!("exit {}", if engine_failed(&events) { 1 } else { 0 }));
}

fn engine_failed(events: &[Stamped]) -> bool {
    events.iter().any(|s| matches!(s.event, Event::BuildComplete { success: false, .. }))
}
