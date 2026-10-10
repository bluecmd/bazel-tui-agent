//! Build state reconstructed from the protocol events (PROTOCOL.md).

use std::collections::{BTreeMap, HashMap};

use crate::events::{ActionRef, Event, Phase, Stamped};
use crate::text::short_repo;

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
    pub fetches: BTreeMap<String, String>,
    pub actions: HashMap<String, Action>,
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
        if !host.is_empty() {
            *self.hosts.entry(host.to_string()).or_insert(0) += 1;
        }
        self.count_source(Source::of_spawn(runner, cache_hit, remote));
    }

    pub fn apply(&mut self, ev: &Stamped) {
        let now = ev.t_ms;
        match &ev.event {
            Event::CommandStarted { command, args, workspace, exec_root, show_result, test_summary, agent_jar, agent_jar_mtime_ms, .. } => {
                self.command = Some(command.clone());
                self.args = args.clone();
                self.workspace = workspace.clone();
                self.agent_jar = agent_jar.clone();
                self.agent_jar_mtime_ms = *agent_jar_mtime_ms;
                self.exec_root = exec_root.clone();
                self.show_result = *show_result;
                self.test_summary = test_summary.clone();
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
            Event::Fetch { resource, progress, finished } => {
                if *finished {
                    self.fetches.remove(resource);
                } else {
                    self.fetches.insert(resource.clone(), progress.clone());
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
                }
            }
            Event::ActionCompleted { action, .. } => {
                let a = self.actions.remove(&action.id);
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
            Event::ActionCached { .. } => self.count_source(Source::ActionCache),
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

    /// The action everything is waiting on: when at most two actions run and more are queued
    /// behind them, the longest-running one (with its age in ms).
    pub fn bottleneck(&self, now: i64) -> Option<(&Action, i64)> {
        let running: Vec<&Action> = self.actions.values().filter(|a| a.state == ActState::Run).collect();
        if running.is_empty() || running.len() > 2 {
            return None;
        }
        let left = self.exec_counts().map(|(d, t)| t.saturating_sub(d)).unwrap_or(0);
        if left <= running.len() as u64 {
            return None;
        }
        let a = running.iter().min_by_key(|a| a.t_ms)?;
        Some((a, now - a.t_ms))
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
