//! Width-aware text helpers and ANSI styling (256-colour; works wherever Bazel's --color does).

pub const RESET: &str = "\x1b[0m";

/// Whether the client's terminal takes 24-bit colour (COLORTERM=truecolor); otherwise every
/// colour is mapped to the 256-colour cube, six levels per channel, which turns the greens
/// grey-ish and makes fades step.
pub static TRUECOLOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The SGR parameters for a colour: `ext` is 38 (foreground) or 48 (background).
pub fn color_code(ext: u8, r: u8, g: u8, b: u8) -> String {
    if TRUECOLOR.load(std::sync::atomic::Ordering::Relaxed) {
        format!("{};2;{};{};{}", ext, r, g, b)
    } else {
        format!("{};5;{}", ext, cube(r, g, b))
    }
}

/// Text without its escape sequences: CSI (`ESC [ … final`), OSC (`ESC ] … BEL` or `ESC \\`)
/// and two-byte escapes are removed, everything else is kept.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for d in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&d) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(d) = it.next() {
                    if d == '\x07' {
                        break;
                    }
                    if d == '\x1b' && it.peek() == Some(&'\\') {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn cube(r: u8, g: u8, b: u8) -> u8 {
    let q = |v: u8| ((v as f32 / 255.0 * 5.0).round()) as u8;
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

pub fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

pub fn gradient(stops: &[(f32, (u8, u8, u8))], t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    for w in stops.windows(2) {
        let (p0, c0) = w[0];
        let (p1, c1) = w[1];
        if t <= p1 {
            let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
            return lerp(c0, c1, f);
        }
    }
    stops[stops.len() - 1].1
}

// The base palette follows the Bazel icon's greens (#76D275 light, #43A047 regular, #00701A
// dark) instead of the blue-grey it had: borders and muted text in a grey-green, the accent
// the icon's light green, titles a pale green-white.
/// Download bars: dark green to the icon's light green, distinct from the action heat scale.
pub const DOWNLOAD: &[(f32, (u8, u8, u8))] = &[(0.0, (0, 112, 26)), (1.0, (118, 210, 117))];
/// Load and parallelism: dark green through the icon's light green and amber to a coral top.
pub const HEAT: &[(f32, (u8, u8, u8))] = &[(0.0, (40, 110, 60)), (0.5, (118, 210, 117)), (0.8, (240, 200, 90)), (1.0, (230, 125, 105))];
pub const COOL: &[(f32, (u8, u8, u8))] = &[(0.0, (30, 90, 45)), (1.0, (118, 210, 117))];
pub const BORDER: (u8, u8, u8) = (70, 105, 75);
pub const OK: (u8, u8, u8) = (118, 230, 117);
pub const BAD: (u8, u8, u8) = (255, 90, 90);
pub const WARN: (u8, u8, u8) = (250, 200, 60);
pub const ABORT: (u8, u8, u8) = (255, 140, 40);
pub const ACCENT: (u8, u8, u8) = (118, 210, 117);
pub const TITLE: (u8, u8, u8) = (200, 225, 200);
pub const MUTED: (u8, u8, u8) = (125, 150, 130);
/// The end-of-command report's calmer tokens: its success tick, its secondary text (a grey-blue
/// rather than SGR 2 faint, which Konsole and others draw at almost full intensity) and the one
/// link it prints.
pub const REPORT_OK: (u8, u8, u8) = (90, 220, 120);
pub const REPORT_MUTED: (u8, u8, u8) = (120, 130, 150);
/// Emphasised names in the report: bold always with this explicit foreground, because bold on
/// the default foreground comes out in the scheme's "intense" colour (blue in Konsole's Breeze).
pub const REPORT_TEXT: (u8, u8, u8) = (214, 219, 230);
pub const LINK: (u8, u8, u8) = (140, 170, 255);

pub fn mnemonic_rgb(m: &str) -> (u8, u8, u8) {
    match m {
        "Rustc" => (220, 140, 255),
        "Clippy" | "Rustdoc" => (200, 120, 230),
        "CppCompile" | "CppLink" | "CppArchive" => (120, 170, 255),
        "Javac" | "JavaIjar" => (250, 200, 80),
        "TestRunner" => (90, 220, 120),
        "GoCompilePkg" | "GoLink" => (90, 200, 220),
        "PyCompile" => (250, 220, 90),
        "ProtoCompile" => (90, 210, 210),
        "Genrule" => (200, 200, 200),
        _ => {
            let h: u32 = m.bytes().enumerate().map(|(i, c)| c as u32 * (i as u32 + 7)).sum::<u32>() % 360;
            gradient(&[(0.0, (255, 140, 140)), (0.33, (140, 255, 170)), (0.66, (150, 170, 255)), (1.0, (255, 140, 140))], h as f32 / 360.0)
        }
    }
}

pub fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Wall-clock `HH:MM:SS` in the local time zone, for the log panel's stamps.
pub fn fmt_clock(ms: i64) -> String {
    let t: libc::time_t = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub fn fmt_elapsed(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

pub fn with_commas(n: u64) -> String {
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

/// '@@rules_rs++crate+crates__aws-sdk-s3-1.144.0' -> '@aws-sdk-s3-1.144.0', '@@llvm+' -> '@llvm'.
pub fn short_repo(repo: &str) -> String {
    let r = repo.trim_start_matches('@').trim_end_matches('+');
    let r = if let Some(i) = r.rfind("__") {
        &r[i + 2..]
    } else if let Some(i) = r.rfind('+') {
        if i + 1 < r.len() { &r[i + 1..] } else { r }
    } else {
        r
    };
    format!("@{}", r)
}

pub fn short_label(label: &str, width: usize) -> String {
    let label = if label.starts_with('@') {
        match label.find("//") {
            Some(i) => format!("{}//{}", short_repo(&label[..i]), &label[i + 2..]),
            None => label.to_string(),
        }
    } else {
        label.to_string()
    };
    if label.chars().count() <= width {
        return label;
    }
    if let Some(i) = label.rfind(':') {
        let (head, tail) = (&label[..i], &label[i + 1..]);
        if !tail.is_empty() && tail.chars().count() + 4 < width {
            let keep = width - tail.chars().count() - 2;
            let h: String = head.chars().rev().take(keep).collect::<Vec<_>>().into_iter().rev().collect();
            return format!("…{}:{}", h, tail);
        }
    }
    let t: String = label.chars().rev().take(width - 1).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{}", t)
}
