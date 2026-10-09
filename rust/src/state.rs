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
    pub status: String,
}

#[derive(Default)]
pub struct State {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub t0_ms: i64,
    pub phase: String,
    pub phase_t: HashMap<String, i64>,
    pub loading: Option<String>,
    pub loading_detail: Option<String>,
    pub analysis: Option<String>,
    pub analysis_done: bool,
    pub execution: Option<String>,
    pub build_started: bool,
    pub fetches: BTreeMap<String, String>,
    pub actions: HashMap<String, Action>,
    pub completed: u64,
    pub failed_actions: u64,
    pub peak_running: u64,
    pub tests: HashMap<String, TestResult>,
    pub warnings: u64,
    pub errors: u64,
    pub build_success: Option<bool>,
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

    pub fn apply(&mut self, ev: &Stamped) {
        let now = ev.t_ms;
        match &ev.event {
            Event::CommandStarted { command, args, .. } => {
                self.command = Some(command.clone());
                self.args = args.clone();
                self.t0_ms = now;
                self.tp_last = (now, 0);
                self.last_sample_ms = now;
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
            Event::Spawn { cache_hit, remote, runner, host, .. } => {
                self.spawns += 1;
                if *cache_hit {
                    self.cache_hits += 1;
                }
                if *remote {
                    self.remote += 1;
                }
                *self.runners.entry(if runner.is_empty() { "?".into() } else { runner.clone() }).or_insert(0) += 1;
                if !host.is_empty() {
                    *self.hosts.entry(host.clone()).or_insert(0) += 1;
                }
            }
            Event::TestSummary { label, status, .. } => {
                self.tests.insert(label.clone(), TestResult { status: status.clone() });
            }
            Event::Message(m) => match m.kind.as_str() {
                "WARNING" => self.warnings += 1,
                "ERROR" | "FATAL" => self.errors += 1,
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
    }

    pub fn running_sorted(&self) -> Vec<&Action> {
        let mut v: Vec<&Action> = self.actions.values().collect();
        v.sort_by(|a, b| a.state.cmp(&b.state).then(a.t_ms.cmp(&b.t_ms)));
        v
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
