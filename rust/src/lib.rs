//! bazel-hook: the in-process side of the Bazel hook.
//!
//! A tiny Java probe (agent/) injects a module into the Bazel server and reports what Bazel's
//! console UI would see as binary packets (`wire.rs`) over JNI. This library turns them into the
//! typed `events::Event` model, keeps the build state, renders the terminal status block and
//! produces the exact bytes the Java side writes to the client's stderr/stdout. Optionally it
//! appends every event to a JSON trace file.
//!
//! The modules are public so that the replay tool (`src/bin/replay.rs`) can drive the same
//! `engine::Engine` the JNI entry points below drive.

pub mod browser;
pub mod dashboard;
pub mod engine;
pub mod events;
pub mod help;
pub mod logview;
pub mod pty;
pub mod render;
pub mod scenario;
pub mod state;
pub mod summary;
pub mod terminal;
pub mod text;
pub mod trace;
pub mod wire;

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jboolean, jbyteArray, jint, jlong};
use jni::JNIEnv;
use std::sync::Mutex;

use engine::Engine;
use terminal::{Output, Terminal};
use trace::Trace;

fn engine<'a>(handle: jlong) -> Option<&'a Mutex<Engine>> {
    if handle == 0 {
        return None;
    }
    // SAFETY: handles come from `create` and live until `destroy`.
    Some(unsafe { &*(handle as *const Mutex<Engine>) })
}

fn bytes(env: &JNIEnv, b: &[u8]) -> jbyteArray {
    match env.byte_array_from_slice(b) {
        Ok(a) => a.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// `ui`: draw on the terminal (width/color/rows); `client_pid`: the Bazel client, whose terminal
/// is used for size and keys when it can be found; `trace`: path of a JSON trace file or empty.
#[no_mangle]
pub extern "system" fn Java_bazelhook_Native_create(mut env: JNIEnv, _class: JClass, ui: jboolean, width: jint, color: jboolean, truecolor: jboolean, rows: jint, client_pid: jlong, trace: JString) -> jlong {
    text::TRUECOLOR.store(truecolor != 0, std::sync::atomic::Ordering::Relaxed);
    let trace_path: String = env.get_string(&trace).map(|s| s.into()).unwrap_or_default();
    let terminal = if ui != 0 { Some(Terminal::new(width.max(0) as usize, color != 0, rows.max(0) as usize, pty::Pty::open(client_pid), client_pid)) } else { None };
    let trace = if trace_path.is_empty() { None } else { Trace::open(&trace_path) };
    Box::into_raw(Box::new(Mutex::new(Engine::new(terminal, trace)))) as jlong
}

/// One wire packet. Returns true when the packet was understood.
#[no_mangle]
pub extern "system" fn Java_bazelhook_Native_event(env: JNIEnv, _class: JClass, handle: jlong, packet: JByteArray) -> jboolean {
    let Some(m) = engine(handle) else { return 0 };
    let Ok(raw) = env.convert_byte_array(&packet) else { return 0 };
    let Some(ev) = wire::decode(&raw) else { return 0 };
    let Ok(mut e) = m.lock() else { return 0 };
    e.event(&ev);
    1
}

/// Bytes to write now (framed stderr + stdout, see terminal.rs); empty when nothing changed.
#[no_mangle]
pub extern "system" fn Java_bazelhook_Native_poll(env: JNIEnv, _class: JClass, handle: jlong, now_ms: jlong) -> jbyteArray {
    let Some(m) = engine(handle) else { return bytes(&env, &[]) };
    let out = match m.lock() {
        Ok(mut e) => e.poll(now_ms),
        Err(_) => Output::default(),
    };
    if out.stderr.is_empty() && out.stdout.is_empty() {
        return bytes(&env, &[]);
    }
    bytes(&env, &out.framed())
}

/// Final bytes at the end of the command (framed), then the engine may be destroyed.
#[no_mangle]
pub extern "system" fn Java_bazelhook_Native_finish(env: JNIEnv, _class: JClass, handle: jlong, now_ms: jlong) -> jbyteArray {
    let Some(m) = engine(handle) else { return bytes(&env, &[]) };
    let out = match m.lock() {
        Ok(mut e) => e.finish(now_ms),
        Err(_) => Output::default(),
    };
    bytes(&env, &out.framed())
}

/// True while an interactive screen (the help finder) is up and needs polling.
#[no_mangle]
pub extern "system" fn Java_bazelhook_Native_busy(_env: JNIEnv, _class: JClass, handle: jlong) -> jboolean {
    let Some(m) = engine(handle) else { return 0 };
    match m.lock() {
        Ok(e) => e.busy() as jboolean,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "system" fn Java_bazelhook_Native_destroy(_env: JNIEnv, _class: JClass, handle: jlong) {
    if handle != 0 {
        // SAFETY: created by `create`; Java calls destroy exactly once.
        unsafe { drop(Box::from_raw(handle as *mut Mutex<Engine>)) };
    }
}
