//! The engine: one Bazel command's state, its terminal output and the optional trace, behind the
//! same three calls the Java side makes (`event`, `poll`, `finish`). The JNI layer, the replay
//! tool and the tests all go through this, so what they exercise is exactly what Bazel drives.

use crate::events::{Event, Stamped};
use crate::state::State;
use crate::terminal::{Output, Terminal};
use crate::trace::Trace;

pub struct Engine {
    pub state: State,
    pub terminal: Option<Terminal>,
    trace: Option<Trace>,
}

impl Engine {
    /// `terminal`: where to draw, or `None` for a trace-only engine (`BAZEL_HOOK_UI=0`).
    pub fn new(terminal: Option<Terminal>, trace: Option<Trace>) -> Engine {
        Engine { state: State::default(), terminal, trace }
    }

    /// One decoded event: traced, applied to the state, and handed to the terminal where it
    /// changes what is printed (messages, the command's start, the client's reported size).
    pub fn event(&mut self, ev: &Stamped) {
        if let Some(t) = self.trace.as_mut() {
            t.write(ev);
        }
        self.state.apply(ev);
        let Some(term) = self.terminal.as_mut() else { return };
        match &ev.event {
            Event::CommandStarted { .. } => term.command_started(&self.state),
            Event::Message(msg) => term.message(msg, ev.t_ms),
            Event::TerminalSize { columns, rows } => term.reported_size(*columns as u16, *rows as u16),
            _ => {}
        }
    }

    /// Bytes to write now; empty when nothing changed.
    pub fn poll(&mut self, now_ms: i64) -> Output {
        self.state.sample(now_ms);
        match self.terminal.as_mut() {
            Some(t) => t.poll(&self.state, now_ms),
            None => Output::default(),
        }
    }

    /// Final bytes at the end of the command.
    pub fn finish(&mut self, now_ms: i64) -> Output {
        if let Some(t) = self.trace.as_mut() {
            t.flush();
        }
        match self.terminal.as_mut() {
            Some(t) => t.finish(&self.state, now_ms),
            None => Output::default(),
        }
    }

    /// True while an interactive screen (the help finder) is up and needs polling.
    pub fn busy(&self) -> bool {
        self.terminal.as_ref().map_or(false, |t| t.busy(&self.state))
    }
}
