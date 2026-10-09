//! Optional event trace (`BAZEL_HOOK_TRACE=/path`): one JSON line per decoded event, appended.
//! This is a debugging and test aid, not the interface; the compatibility matrix reads it.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};

use crate::events::Stamped;

pub struct Trace {
    out: BufWriter<File>,
}

impl Trace {
    pub fn open(path: &str) -> Option<Trace> {
        let f = OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Trace { out: BufWriter::new(f) })
    }

    pub fn write(&mut self, ev: &Stamped) {
        if let Ok(line) = serde_json::to_string(ev) {
            let _ = self.out.write_all(line.as_bytes());
            let _ = self.out.write_all(b"\n");
        }
    }

    pub fn flush(&mut self) {
        let _ = self.out.flush();
    }
}
