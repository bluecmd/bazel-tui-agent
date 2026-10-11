//! Wire format between the Java probe and this library (see agent/src/bazelhook/Packet.java).
//!
//! One packet per event, little-endian, no framing (JNI hands over whole byte arrays):
//!
//! ```text
//! u8  tag          which Event variant (TAG_* below, mirrored in Packet.java)
//! i64 t_ms         server wall clock
//! ... fields in the order of the Event variant's fields:
//!     str  = u32 byte length + UTF-8 bytes
//!     list = u32 count + that many str
//!     i64 / u64 = 8 bytes, f64 = 8 bytes IEEE, bool = u8
//!     ActionRef = 6 str (id, mnemonic, label, target_kind, config, description)
//! ```
//!
//! Strings arrive as the UTF-8 bytes Bazel keeps internally; invalid sequences are replaced.

use crate::events::{ActionRef, Event, Message, Phase, SpawnBrief, Stamped};

pub const TAG_COMMAND_STARTED: u8 = 1;
pub const TAG_BUILD_STARTED: u8 = 2;
pub const TAG_PHASE: u8 = 3;
pub const TAG_PROGRESS: u8 = 4;
pub const TAG_LOADING_COMPLETE: u8 = 5;
pub const TAG_ANALYSIS_COMPLETE: u8 = 6;
pub const TAG_ACTION_STARTED: u8 = 7;
pub const TAG_ACTION_CACHE_CHECK: u8 = 8;
pub const TAG_ACTION_SCHEDULING: u8 = 9;
pub const TAG_ACTION_RUNNING: u8 = 10;
pub const TAG_ACTION_PROGRESS: u8 = 11;
pub const TAG_ACTION_EXECUTED: u8 = 12;
pub const TAG_ACTION_COMPLETED: u8 = 13;
pub const TAG_SPAWN: u8 = 14;
pub const TAG_TARGET_ANALYZED: u8 = 15;
pub const TAG_TARGET_COMPLETE: u8 = 16;
pub const TAG_TEST_ANALYZED: u8 = 17;
pub const TAG_TEST_FILTERING_COMPLETE: u8 = 18;
pub const TAG_TEST_SUMMARY: u8 = 19;
pub const TAG_BUILD_COMPLETE: u8 = 20;
pub const TAG_NO_BUILD: u8 = 21;
pub const TAG_FETCH: u8 = 22;
pub const TAG_MESSAGE: u8 = 23;
pub const TAG_SYSTEM: u8 = 24;
pub const TAG_COMMAND_FINISHED: u8 = 25;
pub const TAG_HOOK_ERROR: u8 = 26;
pub const TAG_OTHER: u8 = 27;
pub const TAG_TERMINAL_SIZE: u8 = 28;
pub const TAG_HELP_COMMAND: u8 = 29;
pub const TAG_HELP_OPTION: u8 = 30;
pub const TAG_ACTION_CACHED: u8 = 31;
pub const TAG_ACTION_RESULT: u8 = 32;
pub const TAG_CANCELLED: u8 = 33;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(self.i64()?.max(0) as u64)
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn list(&mut self) -> Option<Vec<String>> {
        let n = self.u32()? as usize;
        let mut v = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            v.push(self.str()?);
        }
        Some(v)
    }

    fn action(&mut self) -> Option<ActionRef> {
        Some(ActionRef {
            id: self.str()?,
            mnemonic: self.str()?,
            label: self.str()?,
            target_kind: self.str()?,
            config: self.str()?,
            description: self.str()?,
        })
    }
}

/// Decodes one packet; `None` for a malformed or unknown one (the caller just drops it).
pub fn decode(buf: &[u8]) -> Option<Stamped> {
    let mut r = Reader { buf, pos: 0 };
    let tag = r.u8()?;
    let t_ms = r.i64()?;
    let event = match tag {
        TAG_COMMAND_STARTED => Event::CommandStarted {
            command: r.str()?,
            command_id: r.str()?,
            workspace: r.str()?,
            cwd: r.str()?,
            args: r.list()?,
            start_ms: r.i64()?,
            bazel_version: r.str()?,
            agent_jar: r.str()?,
            agent_jar_mtime_ms: r.i64()?,
            exec_root: r.str()?,
            show_result: r.i64()?,
            test_summary: r.str()?,
        },
        TAG_BUILD_STARTED => Event::BuildStarted { targets: r.list()?, build_id: r.str()? },
        TAG_PHASE => Event::Phase {
            phase: match r.u8()? {
                0 => Phase::Loading,
                1 => Phase::Analysis,
                _ => Phase::Execution,
            },
        },
        TAG_PROGRESS => Event::Progress { loading: r.str()?, loading_detail: r.str()?, analysis: r.str()?, execution: r.str()? },
        TAG_LOADING_COMPLETE => Event::LoadingComplete { targets: r.u64()? },
        TAG_ANALYSIS_COMPLETE => Event::AnalysisComplete { targets: r.u64()?, targets_configured: r.u64()?, actions: r.u64()?, time_ms: r.u64()? },
        TAG_ACTION_STARTED => Event::ActionStarted { action: r.action()?, start_ns: r.i64()? },
        TAG_ACTION_CACHE_CHECK => Event::ActionCacheCheck { action: r.action()?, strategy: r.str()? },
        TAG_ACTION_SCHEDULING => Event::ActionScheduling { action: r.action()?, strategy: r.str()? },
        TAG_ACTION_RUNNING => Event::ActionRunning { action: r.action()?, strategy: r.str()? },
        TAG_ACTION_PROGRESS => Event::ActionProgress { action: r.action()?, progress_id: r.str()?, progress: r.str()?, finished: r.bool()? },
        TAG_ACTION_EXECUTED => Event::ActionExecuted { action: r.action()?, success: r.bool()?, error: r.str()? },
        TAG_ACTION_COMPLETED => Event::ActionCompleted { action: r.action()?, start_ns: r.i64()?, finish_ns: r.i64()? },
        TAG_SPAWN => Event::Spawn {
            id: r.str()?,
            mnemonic: r.str()?,
            runner: r.str()?,
            runner_subtype: r.str()?,
            host: r.str()?,
            status: r.str()?,
            exit_code: r.i64()?,
            cache_hit: r.bool()?,
            remote: r.bool()?,
            wall_ms: r.i64()?,
        },
        TAG_TARGET_ANALYZED => Event::TargetAnalyzed { label: r.str()? },
        TAG_TARGET_COMPLETE => Event::TargetComplete { label: r.str()?, failed: r.bool()?, outputs: r.list()? },
        TAG_TEST_ANALYZED => Event::TestAnalyzed { label: r.str()? },
        TAG_TEST_FILTERING_COMPLETE => Event::TestFilteringComplete { targets: r.u64()?, tests: r.u64()?, skipped: r.u64()? },
        TAG_TEST_SUMMARY => Event::TestSummary {
            label: r.str()?,
            status: r.str()?,
            runs: r.u64()?,
            cached: r.bool()?,
            skipped: r.bool()?,
            failed_cases: r.u64()?,
            total_cases: r.u64()?,
            time_ms: r.u64()?,
            failed_logs: r.list()?,
            passed_logs: r.list()?,
            failed_case_names: r.list()?,
            attempts: r.u64()?,
            wrong_size: r.bool()?,
        },
        TAG_BUILD_COMPLETE => Event::BuildComplete { success: r.bool()?, catastrophe: r.bool()?, exit_code: r.i64()?, stop_ms: r.i64()? },
        TAG_NO_BUILD => Event::NoBuild,
        TAG_FETCH => Event::Fetch { resource: r.str()?, progress: r.str()?, finished: r.bool()?, bytes: r.i64()?.max(0) as u64, total: r.i64()?.max(0) as u64 },
        TAG_MESSAGE => Event::Message(Message { kind: r.str()?, text: r.str()?, location: r.str()?, tag: r.str()?, stdout: r.str()?, stderr: r.str()? }),
        TAG_SYSTEM => Event::System { heap_used_mb: r.u64()?, heap_max_mb: r.u64()?, load: r.f64()?, cpus: r.u64()? },
        TAG_COMMAND_FINISHED => Event::CommandFinished,
        TAG_HOOK_ERROR => Event::HookError { event_class: r.str()?, error: r.str()?, disabled: r.str()? },
        TAG_OTHER => Event::Other { class: r.str()? },
        TAG_TERMINAL_SIZE => Event::TerminalSize { columns: r.u64()?, rows: r.u64()? },
        TAG_ACTION_RESULT => {
            let action = r.action()?;
            let n = r.u64()?.min(10_000) as usize;
            let mut spawns = Vec::with_capacity(n);
            for _ in 0..n {
                spawns.push(SpawnBrief { runner: r.str()?, cache_hit: r.bool()?, remote: r.bool()?, wall_ms: r.i64()?, status: r.str()? });
            }
            Event::ActionResult { action, spawns }
        }
        TAG_CANCELLED => Event::Cancelled,
        TAG_HELP_COMMAND => Event::HelpCommand { name: r.str()?, summary: r.str()?, help: r.str()? },
        TAG_HELP_OPTION => Event::HelpOption {
            name: r.str()?,
            abbrev: r.str()?,
            value_type: r.str()?,
            default: r.str()?,
            category: r.str()?,
            help: r.str()?,
            commands: r.list()?,
            effect_tags: r.list()?,
            metadata_tags: r.list()?,
            expansion: r.list()?,
            deprecation: r.str()?,
            old_name: r.str()?,
            multiple: r.bool()?,
            boolean_syntax: r.bool()?,
        },
        TAG_ACTION_CACHED => Event::ActionCached { action: r.action()? },
        _ => return None,
    };
    Some(Stamped { t_ms, event })
}
