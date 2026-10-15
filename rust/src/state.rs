//! Build state reconstructed from the protocol events (PROTOCOL.md).

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::events::{ActionRef, Event, Phase, Stamped};
use crate::text::short_repo;

/// A repository fetch or HTTP download in flight (`Event::Fetch`).
#[derive(Clone, Default, Debug)]
pub struct Fetch {
    /// Bazel's own text ("37 MiB (30.4%)", "Fetching repository @foo", "starting").
    pub progress: String,
    pub bytes: u64,
    /// Content-Length, 0 when unknown.
    pub total: u64,
    /// (time, bytes) over the last few seconds, for the transfer rate.
    pub samples: VecDeque<(i64, u64)>,
}

impl Fetch {
    const RATE_WINDOW_MS: i64 = 5000;

    /// True for an HTTP download, which has byte counts; repository-rule fetches only have text.
    pub fn is_download(&self) -> bool {
        self.bytes > 0 || self.total > 0
    }

    /// Fraction done, from the byte counts or, failing that, the "(30.4%)" in Bazel's text.
    pub fn frac(&self) -> Option<f64> {
        if self.total > 0 {
            return Some((self.bytes as f64 / self.total as f64).clamp(0.0, 1.0));
        }
        let open = self.progress.rfind('(')?;
        let pct = self.progress[open + 1..].strip_suffix("%)")?.trim();
        pct.parse::<f64>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
    }

    /// Bytes per second over the sample window, once there is half a second of data.
    pub fn rate(&self) -> Option<f64> {
        let (t0, b0) = *self.samples.front()?;
        let (t1, b1) = *self.samples.back()?;
        if t1 - t0 < 500 || b1 < b0 {
            return None;
        }
        Some((b1 - b0) as f64 * 1000.0 / (t1 - t0) as f64)
    }

    pub(crate) fn update(&mut self, now: i64, progress: &str, bytes: u64, total: u64) {
        self.progress = progress.to_string();
        self.bytes = bytes;
        if total > 0 {
            self.total = total;
        }
        if bytes > 0 {
            self.samples.push_back((now, bytes));
            while self.samples.len() > 2 && now - self.samples[0].0 > Self::RATE_WINDOW_MS {
                self.samples.pop_front();
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActState {
    Run,
    Cache,
    Sched,
    Queued,
}

pub struct Action {
    pub label: String,
    pub mnemonic: String,
    pub desc: String,
    pub state: ActState,
    pub strategy: String,
    pub t_ms: i64,
    pub progress: Option<String>,
}

/// How an action left the running list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    Done,
    Cached,
    Failed,
}

/// An action that finished a moment ago. The running panels keep showing these, fading, for
/// `FADE_MS`: on a fast build most actions are over before a frame is drawn, and this is the
/// only way to see them go by.
pub struct Finished {
    pub label: String,
    pub mnemonic: String,
    pub desc: String,
    pub strategy: String,
    pub how: Finish,
    pub done_ms: i64,
    /// Wall time the action ran, when it was seen running.
    pub took_ms: Option<u64>,
}

/// How long a finished action stays visible, fading out.
pub const FADE_MS: i64 = 4000;
const RECENT_MAX: usize = 200;

#[derive(Default)]
pub struct Node {
    pub children: BTreeMap<String, Node>,
    pub running: i64,
    pub done: u64,
    pub failed: u64,
    pub last_ms: i64,
    /// Ids of actions in flight that belong to exactly this node.
    pub action_ids: std::collections::BTreeSet<String>,
}

pub struct TestResult {
    pub label: String,
    pub status: String,
    pub runs: u64,
    pub cached: bool,
    pub skipped: bool,
    pub failed_cases: u64,
    pub total_cases: u64,
    pub time_ms: u64,
    pub failed_logs: Vec<String>,
    pub passed_logs: Vec<String>,
    pub failed_case_names: Vec<String>,
    pub attempts: u64,
    pub wrong_size: bool,
}

/// A top-level target as reported complete, with the files Bazel would list for it.
pub struct TargetDone {
    pub label: String,
    pub failed: bool,
    pub outputs: Vec<String>,
}

/// Where an action's work came from, for the cache panel. Local action-cache hits are
/// `ActionCached` events (Bazel skipped the action); the rest are classified from the spawn's
/// runner name ("remote cache hit", "disk cache hit", "remote", "linux-sandbox", "worker", ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    ActionCache,
    RemoteCache,
    DiskCache,
    RemoteExec,
    LocalExec,
}

impl Source {
    pub const ALL: [Source; 5] = [Source::ActionCache, Source::RemoteCache, Source::DiskCache, Source::RemoteExec, Source::LocalExec];

    pub fn name(self) -> &'static str {
        match self {
            Source::ActionCache => "action cache",
            Source::RemoteCache => "remote cache",
            Source::DiskCache => "disk cache",
            Source::RemoteExec => "remote exec",
            Source::LocalExec => "local exec",
        }
    }

    pub fn is_hit(self) -> bool {
        matches!(self, Source::ActionCache | Source::RemoteCache | Source::DiskCache)
    }

    pub fn of_spawn(runner: &str, cache_hit: bool, remote: bool) -> Source {
        let r = runner.to_ascii_lowercase();
        if r.contains("cache") || cache_hit {
            if r.contains("disk") || (!r.contains("remote") && !remote) { Source::DiskCache } else { Source::RemoteCache }
        } else if remote || r == "remote" {
            Source::RemoteExec
        } else {
            Source::LocalExec
        }
    }
}

#[derive(Default)]
pub struct State {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub workspace: String,
    pub agent_jar: String,
    pub agent_jar_mtime_ms: i64,
    pub exec_root: String,
    pub show_result: i64,
    pub test_summary: String,
    pub agent_version: String,
    /// The invocation's page on the Build Event Service, empty when there is none.
    pub bes_url: String,
    pub t0_ms: i64,
    pub phase: String,
    pub phase_t: HashMap<String, i64>,
    pub loading: Option<String>,
    pub loading_detail: Option<String>,
    pub analysis: Option<String>,
    pub analysis_done: bool,
    pub execution: Option<String>,
    pub build_started: bool,
    /// Latest PROGRESS-kind message ("Computing main repo mapping", "Loading package: ...").
    pub activity: String,
    pub fetches: BTreeMap<String, Fetch>,
    pub actions: HashMap<String, Action>,
    /// Actions that finished within the last `FADE_MS`, oldest first.
    pub recent: VecDeque<Finished>,
    pub completed: u64,
    pub failed_actions: u64,
    pub peak_running: u64,
    pub tests: HashMap<String, TestResult>,
    /// Labels in completion order (tests and targets).
    pub targets: Vec<TargetDone>,
    pub help: Vec<crate::help::Item>,
    /// Action ids whose spawns were already counted from `Spawn` events (so their `ActionResult`
    /// is not counted again).
    pub spawn_counted: std::collections::HashSet<String>,
    pub warnings: u64,
    pub errors: u64,
    pub build_success: Option<bool>,
    /// Ctrl-C was pressed; set until the command ends.
    pub cancelled_ms: Option<i64>,
    pub finished: bool,
    pub exec_t0_ms: Option<i64>,
    pub exec_done0: u64,
    pub history_running: Vec<u32>,
    pub history_tp: Vec<f32>,
    pub tp_last: (i64, u64),
    pub last_sample_ms: i64,
    pub spawns: u64,
    pub cache_hits: u64,
    pub remote: u64,
    pub runners: HashMap<String, u64>,
    pub hosts: HashMap<String, u64>,
    pub mnemonic_stats: HashMap<String, (u64, u64)>,
    pub heap_used_mb: u64,
    pub heap_max_mb: u64,
    pub load: f64,
    pub cpus: u64,
    pub tree: Node,
    /// Cache panel: actions per `Source` (indexed by position in `Source::ALL`), local
    /// action-cache hits counted from `ActionCached`, the rest from `Spawn`.
    pub sources: [u64; 5],
    /// Hit rate per one-second window (0..1), -1 for windows without any action; sampled in `sample`.
    pub history_hit: Vec<f32>,
    /// (time, hits, total) at the previous hit-rate sample.
    pub hit_last: (i64, u64, u64),
}

/// '@@repo//a/b:c' -> ['@repo', 'a', 'b', ':c']; '//x:y' -> ['//', 'x', ':y'].
pub fn label_path(label: &str) -> Vec<String> {
    let (repo, rest) = match label.find("//") {
        Some(i) => (&label[..i], &label[i + 2..]),
        None => ("", label),
    };
    let mut parts = vec![if repo.is_empty() { "//".to_string() } else { short_repo(repo) }];
    let (pkg, name) = match rest.find(':') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    for p in pkg.split('/').filter(|p| !p.is_empty()) {
        parts.push(p.to_string());
    }
    let last_pkg = pkg.rsplit('/').next().unwrap_or("");
    if !name.is_empty() && name != last_pkg {
        parts.push(format!(":{}", name));
    }
    parts
}

impl State {
    fn touch(&mut self, label: &str, now: i64, running_delta: i64, done: u64, failed: u64) {
        let mut n = &mut self.tree;
        for part in label_path(label) {
            n = n.children.entry(part).or_default();
            n.running = (n.running + running_delta).max(0);
            n.done += done;
            n.failed += failed;
            n.last_ms = now;
        }
    }

    fn leaf(&mut self, label: &str) -> &mut Node {
        let mut n = &mut self.tree;
        for part in label_path(label) {
            n = n.children.entry(part).or_default();
        }
        n
    }

    fn ensure_action(&mut self, a: &ActionRef, now: i64) {
        if !self.actions.contains_key(&a.id) {
            self.leaf(&a.label).action_ids.insert(a.id.clone());
        }
        self.actions.entry(a.id.clone()).or_insert_with(|| Action {
            label: a.label.clone(),
            mnemonic: a.mnemonic.clone(),
            desc: a.description.clone(),
            state: ActState::Queued,
            strategy: String::new(),
            t_ms: now,
            progress: None,
        });
    }

    fn transition(&mut self, a: &ActionRef, new_state: ActState, strategy: &str, now: i64) {
        self.ensure_action(a, now);
        let (label, old) = {
            let act = self.actions.get_mut(&a.id).unwrap();
            let old = act.state;
            if new_state == ActState::Run && old != ActState::Run {
                act.t_ms = now;
            }
            act.state = new_state;
            act.strategy = strategy.to_string();
            (act.label.clone(), old)
        };
        if new_state == ActState::Run && old != ActState::Run {
            self.touch(&label, now, 1, 0, 0);
            let running = self.running_count();
            self.peak_running = self.peak_running.max(running);
        } else if old == ActState::Run && new_state != ActState::Run {
            self.touch(&label, now, -1, 0, 0);
        }
    }

    fn finished(&mut self, a: &ActionRef, prior: Option<&Action>, how: Finish, now: i64) {
        let (label, mnemonic, desc, strategy, took_ms) = match prior {
            Some(p) => (p.label.clone(), p.mnemonic.clone(), p.desc.clone(), p.strategy.clone(), if p.state == ActState::Run { Some((now - p.t_ms).max(0) as u64) } else { None }),
            None => (a.label.clone(), a.mnemonic.clone(), a.description.clone(), String::new(), None),
        };
        self.recent.push_back(Finished { label, mnemonic, desc, strategy, how, done_ms: now, took_ms });
        while self.recent.len() > RECENT_MAX {
            self.recent.pop_front();
        }
    }

    /// Recently finished actions, newest first, still within their fade time.
    pub fn recent_finished(&self, now: i64) -> Vec<&Finished> {
        self.recent.iter().rev().take_while(|f| now - f.done_ms < FADE_MS).collect()
    }

    /// One spawn (executed or served from a cache) for the counters and the source breakdown.
    fn count_spawn(&mut self, runner: &str, cache_hit: bool, remote: bool, host: &str) {
        self.spawns += 1;
        if cache_hit {
            self.cache_hits += 1;
        }
        if remote {
            self.remote += 1;
        }
        *self.runners.entry(if runner.is_empty() { "?".into() } else { runner.to_string() }).or_insert(0) += 1;
        // Bazel fills the executor host name for local and sandboxed spawns too (the machine's
        // own name), so only remote spawns count toward the hosts.
        if remote && !host.is_empty() {
            *self.hosts.entry(host.to_string()).or_insert(0) += 1;
        }
        self.count_source(Source::of_spawn(runner, cache_hit, remote));
    }

    pub fn apply(&mut self, ev: &Stamped) {
        let now = ev.t_ms;
        match &ev.event {
            Event::CommandStarted { command, args, workspace, exec_root, show_result, test_summary, agent_jar, agent_jar_mtime_ms, agent_version, bes_url, .. } => {
                self.command = Some(command.clone());
                self.args = args.clone();
                self.workspace = workspace.clone();
                self.agent_jar = agent_jar.clone();
                self.agent_jar_mtime_ms = *agent_jar_mtime_ms;
                self.exec_root = exec_root.clone();
                self.show_result = *show_result;
                self.test_summary = test_summary.clone();
                self.agent_version = agent_version.clone();
                self.bes_url = bes_url.clone();
                self.t0_ms = now;
                self.tp_last = (now, 0);
                self.last_sample_ms = now;
                self.hit_last = (now, 0, 0);
            }
            Event::BuildStarted { .. } => {
                self.build_started = true;
                self.phase = "build".into();
            }
            Event::Phase { phase } => {
                let p = match phase {
                    Phase::Loading => "loading",
                    Phase::Analysis => "analysis",
                    Phase::Execution => "execution",
                };
                self.phase_t.entry(p.to_string()).or_insert(now);
                if *phase == Phase::Execution && self.exec_t0_ms.is_none() {
                    self.exec_t0_ms = Some(now);
                    self.exec_done0 = self.completed;
                }
                self.phase = p.to_string();
            }
            Event::Progress { loading, loading_detail, analysis, execution } => {
                if !loading.is_empty() {
                    self.loading = Some(loading.clone());
                    self.loading_detail = if loading_detail.is_empty() { None } else { Some(loading_detail.clone()) };
                }
                if !analysis.is_empty() {
                    self.analysis = Some(analysis.clone());
                }
                if !execution.is_empty() {
                    self.execution = Some(execution.clone());
                }
            }
            Event::LoadingComplete { .. } => self.loading_detail = None,
            Event::AnalysisComplete { .. } => {
                self.analysis_done = true;
                self.analysis = None;
            }
            Event::Fetch { resource, progress, finished, bytes, total } => {
                if *finished {
                    self.fetches.remove(resource);
                    // Bazel's last "Downloading <url>: N bytes" progress message would otherwise
                    // stay as the activity after the download it describes is over.
                    if self.activity.starts_with("Downloading ") && !self.fetches.values().any(|f| f.is_download()) {
                        self.activity.clear();
                    }
                } else {
                    let f = self.fetches.entry(resource.clone()).or_insert_with(Fetch::default);
                    f.update(now, progress, *bytes, *total);
                }
            }
            Event::ActionStarted { action, .. } => self.ensure_action(action, now),
            Event::ActionScheduling { action, strategy } => self.transition(action, ActState::Sched, strategy, now),
            Event::ActionRunning { action, strategy } => self.transition(action, ActState::Run, strategy, now),
            Event::ActionCacheCheck { action, strategy } => self.transition(action, ActState::Cache, strategy, now),
            Event::ActionProgress { action, progress, finished, .. } => {
                if let Some(a) = self.actions.get_mut(&action.id) {
                    a.progress = if *finished { None } else { Some(progress.clone()) };
                }
            }
            Event::ActionExecuted { action, success, .. } => {
                if !success {
                    self.failed_actions += 1;
                    self.touch(&action.label, now, 0, 0, 1);
                    let prior = self.actions.remove(&action.id);
                    if prior.as_ref().map_or(false, |p| p.state == ActState::Run) {
                        self.touch(&action.label, now, -1, 0, 0);
                    }
                    self.leaf(&action.label).action_ids.remove(&action.id);
                    self.finished(action, prior.as_ref(), Finish::Failed, now);
                }
            }
            Event::ActionCompleted { action, .. } => {
                let a = self.actions.remove(&action.id);
                self.finished(action, a.as_ref(), Finish::Done, now);
                self.completed += 1;
                let (label, was_running, dur) = match &a {
                    Some(a) => (a.label.clone(), a.state == ActState::Run, (now - a.t_ms).max(0) as u64),
                    None => (action.label.clone(), false, 0),
                };
                self.touch(&label, now, if was_running { -1 } else { 0 }, 1, 0);
                self.leaf(&label).action_ids.remove(&action.id);
                let st = self.mnemonic_stats.entry(action.mnemonic.clone()).or_insert((0, 0));
                st.0 += 1;
                if was_running {
                    st.1 += dur;
                }
            }
            Event::ActionResult { action, spawns } => {
                if !self.spawn_counted.contains(&action.id) {
                    for sp in spawns {
                        self.count_spawn(&sp.runner, sp.cache_hit, sp.remote, "");
                    }
                }
            }
            Event::Spawn { id, cache_hit, remote, runner, host, .. } => {
                self.spawn_counted.insert(id.clone());
                self.count_spawn(runner, *cache_hit, *remote, host);
            }
            Event::TargetComplete { label, failed, outputs } => {
                self.targets.push(TargetDone { label: label.clone(), failed: *failed, outputs: outputs.clone() });
            }
            Event::TestSummary { label, status, runs, cached, skipped, failed_cases, total_cases, time_ms, failed_logs, passed_logs, failed_case_names, attempts, wrong_size } => {
                self.tests.insert(
                    label.clone(),
                    TestResult {
                        label: label.clone(),
                        status: status.clone(),
                        runs: *runs,
                        cached: *cached,
                        skipped: *skipped,
                        failed_cases: *failed_cases,
                        total_cases: *total_cases,
                        time_ms: *time_ms,
                        failed_logs: failed_logs.clone(),
                        passed_logs: passed_logs.clone(),
                        failed_case_names: failed_case_names.clone(),
                        attempts: *attempts,
                        wrong_size: *wrong_size,
                    },
                );
            }
            Event::HelpCommand { name, summary, help } => self.help.push(crate::help::Item::command(name, summary, help)),
            Event::HelpOption { name, abbrev, value_type, default, category, help, commands, effect_tags, metadata_tags, expansion, deprecation, old_name, multiple, boolean_syntax } => {
                self.help.push(crate::help::Item::option(crate::help::OptionDoc {
                    name: name.clone(),
                    abbrev: abbrev.clone(),
                    value_type: value_type.clone(),
                    default: default.clone(),
                    category: category.clone(),
                    help: help.clone(),
                    commands: commands.clone(),
                    effect_tags: effect_tags.clone(),
                    metadata_tags: metadata_tags.clone(),
                    expansion: expansion.clone(),
                    deprecation: deprecation.clone(),
                    old_name: old_name.clone(),
                    multiple: *multiple,
                    boolean_syntax: *boolean_syntax,
                }))
            }
            Event::ActionCached { action } => {
                self.count_source(Source::ActionCache);
                self.finished(action, None, Finish::Cached, now);
            }
            Event::Cancelled => {
                if self.cancelled_ms.is_none() {
                    self.cancelled_ms = Some(now);
                }
            }
            Event::Message(m) => match m.kind.as_str() {
                "WARNING" => self.warnings += 1,
                "ERROR" | "FATAL" => {
                    self.errors += 1;
                    if m.text.trim() == "build interrupted" && self.cancelled_ms.is_none() {
                        self.cancelled_ms = Some(now);
                    }
                }
                "PROGRESS" => self.activity = m.text.trim().to_string(),
                _ => {}
            },
            Event::System { heap_used_mb, heap_max_mb, load, cpus } => {
                self.heap_used_mb = *heap_used_mb;
                self.heap_max_mb = *heap_max_mb;
                self.load = *load;
                self.cpus = *cpus;
            }
            Event::BuildComplete { success, .. } => {
                self.build_success = Some(*success);
                self.phase = "finished".into();
                self.phase_t.entry("finished".into()).or_insert(now);
            }
            Event::NoBuild => self.phase = "finished".into(),
            Event::CommandFinished => {
                self.finished = true;
                self.phase_t.entry("finished".into()).or_insert(now);
            }
            _ => {}
        }
    }

    /// `help` with no topic: the interactive finder replaces Bazel's usage text.
    pub fn interactive_help(&self) -> bool {
        self.command.as_deref() == Some("help") && self.args.is_empty()
    }

    /// Commands that run a build and therefore get the live UI from their first tick. Everything
    /// else (`query`, `info`, `help`, ...) writes to stdout and is left alone.
    pub fn command_builds(&self) -> bool {
        matches!(
            self.command.as_deref(),
            Some("build" | "test" | "run" | "coverage" | "fetch" | "vendor" | "mobile-install" | "cquery" | "aquery" | "sync" | "print_action")
        )
    }

    /// Feeds the graphs; called from render (every ~100 ms).
    pub fn sample(&mut self, now: i64) {
        if now - self.last_sample_ms < 100 {
            return;
        }
        self.last_sample_ms = now;
        while self.recent.front().map_or(false, |f| now - f.done_ms >= FADE_MS) {
            self.recent.pop_front();
        }
        let running = self.actions.values().filter(|a| a.state == ActState::Run).count() as u32;
        self.history_running.push(running);
        if now - self.tp_last.0 >= 1000 {
            let dt = (now - self.tp_last.0) as f32 / 1000.0;
            self.history_tp.push((self.completed - self.tp_last.1) as f32 / dt);
            self.tp_last = (now, self.completed);
        }
        if self.history_running.len() > 600 {
            self.history_running.drain(..self.history_running.len() - 600);
        }
        if self.history_tp.len() > 600 {
            self.history_tp.drain(..self.history_tp.len() - 600);
        }
        if now - self.hit_last.0 >= 1000 {
            let (hits, total) = (self.cache_hit_count(), self.cache_total());
            let (dh, dt) = (hits - self.hit_last.1, total - self.hit_last.2);
            self.history_hit.push(if dt > 0 { dh as f32 / dt as f32 } else { -1.0 });
            self.hit_last = (now, hits, total);
        }
        if self.history_hit.len() > 600 {
            self.history_hit.drain(..self.history_hit.len() - 600);
        }
    }

    fn count_source(&mut self, src: Source) {
        self.sources[Source::ALL.iter().position(|s| *s == src).unwrap_or(0)] += 1;
    }

    pub fn source_count(&self, src: Source) -> u64 {
        self.sources[Source::ALL.iter().position(|s| *s == src).unwrap_or(0)]
    }

    /// Actions accounted for by the cache panel: action-cache hits plus every spawn.
    pub fn cache_total(&self) -> u64 {
        self.sources.iter().sum()
    }

    /// Actions that were not executed: action-cache, remote-cache and disk-cache hits.
    pub fn cache_hit_count(&self) -> u64 {
        Source::ALL.iter().filter(|s| s.is_hit()).map(|s| self.source_count(*s)).sum()
    }

    /// Overall hit rate 0..1, when anything has been counted.
    pub fn hit_rate(&self) -> Option<f64> {
        let t = self.cache_total();
        if t > 0 { Some(self.cache_hit_count() as f64 / t as f64) } else { None }
    }

    pub fn running_sorted(&self) -> Vec<&Action> {
        let mut v: Vec<&Action> = self.actions.values().collect();
        v.sort_by(|a, b| a.state.cmp(&b.state).then(a.t_ms.cmp(&b.t_ms)));
        v
    }

    pub fn cancelling(&self) -> bool {
        self.cancelled_ms.is_some() && !self.finished && self.build_success.is_none()
    }

    /// Analysis is still configuring targets. With Skymeld, Bazel starts the execution phase and
    /// its "[1 / 1]" counter while this is going on; that counter says nothing useful yet.
    pub fn analysis_live(&self) -> bool {
        self.analysis.is_some() && !self.analysis_done
    }

    pub fn running_count(&self) -> u64 {
        self.actions.values().filter(|a| a.state == ActState::Run).count() as u64
    }

    /// (done, total) from Bazel's "[1,234 / 5,678]".
    pub fn exec_counts(&self) -> Option<(u64, u64)> {
        let s = self.execution.as_ref()?;
        let inner = s.trim().trim_start_matches('[').trim_end_matches(']');
        let (a, b) = inner.split_once('/')?;
        let n = |x: &str| x.trim().replace(',', "").parse::<u64>().ok();
        Some((n(a)?, n(b)?))
    }

    pub fn eta_secs(&self, now: i64) -> Option<f64> {
        let (done, total) = self.exec_counts()?;
        let t0 = self.exec_t0_ms?;
        let elapsed = (now - t0) as f64 / 1000.0;
        if elapsed < 15.0 || done <= self.exec_done0 + 10 || total <= done {
            return None;
        }
        let rate = (done - self.exec_done0) as f64 / elapsed;
        if rate > 0.0 { Some((total - done) as f64 / rate) } else { None }
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::Fetch;

    #[test]
    fn fraction_from_bytes_or_bazels_percentage() {
        let mut f = Fetch::default();
        f.update(0, "37 MiB (30.4%)", 0, 0);
        assert_eq!(f.frac().map(|x| (x * 1000.0).round()), Some(304.0));
        assert!(!f.is_download());
        f.update(100, "40 MiB (33.3%)", 40 << 20, 120 << 20);
        assert!(f.is_download());
        assert_eq!(f.frac().map(|x| (x * 3.0).round()), Some(1.0));
        let mut r = Fetch::default();
        r.update(0, "Fetching repository @@foo", 0, 0);
        assert_eq!(r.frac(), None);
        let mut u = Fetch::default();
        u.update(0, "12 MiB", 12 << 20, 0);
        assert!(u.is_download());
        assert_eq!(u.frac(), None);
    }

    #[test]
    fn finished_download_clears_its_stale_progress_message() {
        use crate::events::{Event, Stamped};
        let mut s = super::State::default();
        let fetch = |finished: bool| Stamped { t_ms: 1000, event: Event::Fetch { resource: "https://x/y.tar".into(), progress: "1 MiB".into(), finished, bytes: 1 << 20, total: 0 } };
        s.apply(&fetch(false));
        s.activity = "Downloading https://x/y.tar: 1,048,576 bytes".into();
        s.apply(&fetch(true));
        assert_eq!(s.activity, "");
        s.activity = "Loading package: foo".into();
        s.apply(&fetch(false));
        s.apply(&fetch(true));
        assert_eq!(s.activity, "Loading package: foo");
    }

    #[test]
    fn rate_needs_half_a_second_and_uses_a_sliding_window() {
        let mut f = Fetch::default();
        f.update(0, "", 1 << 20, 0);
        assert_eq!(f.rate(), None);
        f.update(1000, "", 5 << 20, 0);
        assert_eq!(f.rate().map(|r| (r / (1 << 20) as f64).round()), Some(4.0));
        // Ten seconds later the old samples are gone and the rate reflects the recent stretch.
        f.update(10_000, "", 6 << 20, 0);
        f.update(11_000, "", 7 << 20, 0);
        assert_eq!(f.rate().map(|r| (r / (1 << 20) as f64).round()), Some(1.0));
        assert!(f.samples.len() <= 3);
    }
}

#[cfg(test)]
mod host_tests {
    use super::State;
    use crate::events::{Event, Stamped};

    fn spawn(runner: &str, remote: bool, host: &str) -> Stamped {
        Stamped {
            t_ms: 1_000,
            event: Event::Spawn {
                id: "x".into(), mnemonic: "Genrule".into(), runner: runner.into(), runner_subtype: String::new(), host: host.into(),
                status: "SUCCESS".into(), exit_code: 0, cache_hit: false, remote, wall_ms: 10,
            },
        }
    }

    #[test]
    fn only_remote_spawns_count_as_hosts() {
        let mut s = State::default();
        // Bazel names the local machine as the executor host of local and sandboxed spawns.
        s.apply(&spawn("linux-sandbox", false, "bastion"));
        s.apply(&spawn("local", false, "bastion"));
        assert!(s.hosts.is_empty());
        s.apply(&spawn("remote", true, "worker-7"));
        s.apply(&spawn("remote", true, "worker-7"));
        s.apply(&spawn("remote", true, "worker-9"));
        assert_eq!(s.hosts.len(), 2);
        assert_eq!(s.hosts["worker-7"], 2);
    }
}
