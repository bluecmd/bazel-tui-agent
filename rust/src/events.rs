//! The event interface: what the Java probe reports about a Bazel command, as plain Rust data.
//!
//! Everything downstream (state, rendering, the trace file) consumes these types. The Java side
//! only knows the wire format in `wire.rs`; it never sees these structs.

use serde::Serialize;

/// One action as Bazel identifies it across its lifecycle events. `id` is the exec path of the
/// primary output and is stable from `ActionStarted` to `ActionCompleted`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ActionRef {
    pub id: String,
    pub mnemonic: String,
    pub label: String,
    pub target_kind: String,
    pub config: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Loading,
    Analysis,
    Execution,
}

/// A message Bazel's console UI would print: INFO, WARNING, ERROR, DEBUG, STDOUT, STDERR, ...
#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub kind: String,
    pub text: String,
    pub location: String,
    pub tag: String,
    /// Captured stdout/stderr of a failed action attached to the message, lossily decoded.
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    CommandStarted {
        command: String,
        command_id: String,
        workspace: String,
        cwd: String,
        args: Vec<String>,
        start_ms: i64,
        bazel_version: String,
        agent_jar: String,
        agent_jar_mtime_ms: i64,
    },
    BuildStarted { targets: Vec<String>, build_id: String },
    Phase { phase: Phase },
    /// Bazel's own progress strings; a field is empty when that phase is not live.
    Progress { loading: String, loading_detail: String, analysis: String, execution: String },
    LoadingComplete { targets: u64 },
    AnalysisComplete { targets: u64, targets_configured: u64, actions: u64, time_ms: u64 },
    ActionStarted { action: ActionRef, start_ns: i64 },
    ActionCacheCheck { action: ActionRef, strategy: String },
    ActionScheduling { action: ActionRef, strategy: String },
    ActionRunning { action: ActionRef, strategy: String },
    ActionProgress { action: ActionRef, progress_id: String, progress: String, finished: bool },
    ActionExecuted { action: ActionRef, success: bool, error: String },
    ActionCompleted { action: ActionRef, start_ns: i64, finish_ns: i64 },
    Spawn {
        id: String,
        mnemonic: String,
        runner: String,
        runner_subtype: String,
        host: String,
        status: String,
        exit_code: i64,
        cache_hit: bool,
        remote: bool,
        wall_ms: i64,
    },
    TargetAnalyzed { label: String },
    TargetComplete { label: String, failed: bool },
    TestAnalyzed { label: String },
    TestFilteringComplete { targets: u64, tests: u64, skipped: u64 },
    TestSummary {
        label: String,
        status: String,
        runs: u64,
        cached: bool,
        skipped: bool,
        failed_cases: u64,
        total_cases: u64,
        time_ms: u64,
    },
    BuildComplete { success: bool, catastrophe: bool, exit_code: i64, stop_ms: i64 },
    NoBuild,
    Fetch { resource: String, progress: String, finished: bool },
    Message(Message),
    System { heap_used_mb: u64, heap_max_mb: u64, load: f64, cpus: u64 },
    CommandFinished,
    /// An encoder on the Java side failed (renamed accessor, missing class); it disabled itself.
    HookError { event_class: String, error: String, disabled: String },
    /// Any other event-bus posting, by class name (only when BAZEL_HOOK_ALL_EVENTS=1).
    Other { class: String },
}

/// An event with the server's wall-clock timestamp.
#[derive(Clone, Debug, Serialize)]
pub struct Stamped {
    pub t_ms: i64,
    #[serde(flatten)]
    pub event: Event,
}
