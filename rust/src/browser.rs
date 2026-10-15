//! Opening a web page for the user from inside the server.
//!
//! The server was started by some earlier client, possibly from another session, so its own
//! environment is the wrong one for finding the user's display and browser. The client that
//! asked for this command has the right one; on Linux it is readable from `/proc/<pid>/environ`
//! and the opener is run with exactly that. `$BROWSER` wins when set (`%s` stands for the URL,
//! the usual convention), otherwise the desktop's opener (`xdg-open`, `open` on macOS,
//! `rundll32 url.dll` on Windows). The opener is never waited for on the caller's thread.

use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Starts the user's browser on `url`. The error names what could not be started; the page
/// may still fail to open later, which is invisible here.
pub fn open(url: &str, client_pid: i64) -> Result<(), String> {
    if !is_web_url(url) {
        return Err("not a web address".into());
    }
    let env = client_env(client_pid);
    let mut cmd = command(url, env.as_ref().and_then(|e| e.get("BROWSER")).map(|s| s.as_str()));
    if let Some(env) = env {
        cmd.env_clear().envs(env);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            // Reap it when it is done, so the server does not collect a zombie per key press.
            std::thread::Builder::new().name("bazel-hook-open".into()).spawn(move || {
                let _ = child.wait();
            }).ok();
            Ok(())
        }
        Err(e) => Err(format!("{}: {}", cmd.get_program().to_string_lossy(), e)),
    }
}

fn is_web_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The opener for this platform, or the user's `$BROWSER`.
fn command(url: &str, browser: Option<&str>) -> Command {
    if let Some(b) = browser.filter(|b| !b.trim().is_empty()) {
        // "$BROWSER" may be a command line with `%s` for the URL ("firefox --new-tab %s"); a
        // list of alternatives separated by colons is reduced to its first entry.
        let first = b.split(':').next().unwrap_or(b).trim();
        let line = if first.contains("%s") { first.replace("%s", &shell_quote(url)) } else { format!("{} {}", first, shell_quote(url)) };
        let mut c = Command::new("sh");
        c.arg("-c").arg(line);
        return c;
    }
    if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The client process's environment (Linux only; `None` elsewhere or when unreadable, in which
/// case the opener inherits the server's).
fn client_env(client_pid: i64) -> Option<HashMap<String, String>> {
    if client_pid <= 0 {
        return None;
    }
    let raw = std::fs::read(format!("/proc/{}/environ", client_pid)).ok()?;
    let mut env = HashMap::new();
    for entry in raw.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let text = String::from_utf8_lossy(entry);
        if let Some((k, v)) = text.split_once('=') {
            if !k.is_empty() {
                env.insert(k.to_string(), v.to_string());
            }
        }
    }
    if env.is_empty() { None } else { Some(env) }
}

/// The host of a web address, for showing where a key would take the user: `app.buildbuddy.io`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let rest = rest.rsplit('@').next().unwrap_or(rest);
    rest.split(|c| c == '/' || c == '?' || c == '#').next().unwrap_or(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_what_the_user_recognises() {
        assert_eq!(host("https://app.buildbuddy.io/invocation/4b49-01a4"), "app.buildbuddy.io");
        assert_eq!(host("http://bes.corp:8080/?x=1"), "bes.corp:8080");
        assert_eq!(host("https://user:pw@results.example.com"), "results.example.com");
        assert_eq!(host("app.buildbuddy.io"), "app.buildbuddy.io");
    }

    #[test]
    fn only_web_addresses_are_opened() {
        assert!(is_web_url("https://app.buildbuddy.io/invocation/1"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("https://x.example/\nrm -rf"));
        assert!(!is_web_url(""));
    }

    #[test]
    fn browser_variable_takes_the_url() {
        let c = command("https://x.example/a'b", Some("firefox --new-tab %s"));
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(c.get_program(), "sh");
        assert_eq!(args, vec!["-c", "firefox --new-tab 'https://x.example/a'\\''b'"]);
        let c = command("https://x.example/", Some("chromium:firefox"));
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, vec!["-c", "chromium 'https://x.example/'"]);
        let c = command("https://x.example/", None);
        assert!(c.get_program() == "xdg-open" || c.get_program() == "open" || c.get_program() == "rundll32");
    }
}
