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
        /// Where exec paths resolve (`<output_base>/execroot/<workspace>`).
        exec_root: String,
        /// Bazel's `--show_result` threshold (-1 for commands without it).
        show_result: i64,
        /// Bazel's `--test_summary` format in lower case ("" for commands without it).
        test_summary: String,
        /// The release tag the agent jar was built from ("dev" when unstamped).
        agent_version: String,
        /// This invocation's page on the Build Event Service (`--bes_results_url` + command id,
        /// as Bazel's "Streaming build results to:" line has it); empty without a BES backend.
        bes_url: String,
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
    /// `outputs`: exec paths of the files Bazel's result printer would list for the target.
    TargetComplete { label: String, failed: bool, outputs: Vec<String> },
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
        failed_logs: Vec<String>,
        passed_logs: Vec<String>,
        failed_case_names: Vec<String>,
        attempts: u64,
        wrong_size: bool,
    },
    BuildComplete { success: bool, catastrophe: bool, exit_code: i64, stop_ms: i64 },
    NoBuild,
    /// `bytes`/`total` are set for HTTP downloads (total 0 when the size is unknown), both 0
    /// for repository-rule fetches.
    Fetch { resource: String, progress: String, finished: bool, bytes: u64, total: u64 },
    Message(Message),
    System { heap_used_mb: u64, heap_max_mb: u64, load: f64, cpus: u64 },
    CommandFinished,
    /// An encoder on the Java side failed (renamed accessor, missing class); it disabled itself.
    HookError { event_class: String, error: String, disabled: String },
    /// Any other event-bus posting, by class name (only when BAZEL_HOOK_ALL_EVENTS=1).
    Other { class: String },
    /// Bazel 9.3+: the client's terminal size as reported over the command server (columns
    /// only in practice; rows is 0). Sent on SIGWINCH, never at command start.
    TerminalSize { columns: u64, rows: u64 },
    /// Bazel's result for one action (every executed or cache-served action), with its spawn
    /// results; this is where remote and disk cache hits are visible on releases that post no
    /// `Spawn` for them.
    ActionResult { action: ActionRef, spawns: Vec<SpawnBrief> },
    /// The client asked to cancel (Ctrl-C): Bazel is interrupting and will finish with an error.
    Cancelled,
    /// One Bazel command (or help topic) for the interactive `help` finder.
    HelpCommand { name: String, summary: String, help: String },
    /// One documented option for the interactive `help` finder.
    HelpOption {
        name: String,
        abbrev: String,
        value_type: String,
        default: String,
        category: String,
        help: String,
        commands: Vec<String>,
        effect_tags: Vec<String>,
        metadata_tags: Vec<String>,
        expansion: Vec<String>,
        deprecation: String,
        old_name: String,
        multiple: bool,
        boolean_syntax: bool,
    },
    /// Local action-cache hit: Bazel found the action up to date and skipped it (no spawn
    /// follows; this is the "N action cache hit" of Bazel's own summary).
    ActionCached { action: ActionRef },
}

/// One spawn result inside an `ActionResult`.
#[derive(Clone, Debug, Serialize)]
pub struct SpawnBrief {
    pub runner: String,
    pub cache_hit: bool,
    pub remote: bool,
    pub wall_ms: i64,
    pub status: String,
}

/// An event with the server's wall-clock timestamp.
#[derive(Clone, Debug, Serialize)]
pub struct Stamped {
    pub t_ms: i64,
    #[serde(flatten)]
    pub event: Event,
}
