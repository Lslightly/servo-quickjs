/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Raw timing of `Cx::drain_pool`, built with `DRAIN_STATS=1`.
//!
//! Every drain appends one JSON line to `QJS_DRAIN_STATS_OUT` (default
//! `qjs-drain-stats-<pid>.jsonl` in the temporary directory):
//!
//! ```json
//! {"thread":"Script(1,1)","pid":123,"runtime":1,"ctx":"native_call","elapsed_ns":1234,"values":5}
//! ```
//!
//! `ctx` says why the drain ran (a scope dropped, a native call returned, …). `elapsed_ns` is
//! `null` for an empty drain (nothing to release; those are not timed). `values` is how many pool
//! entries the drain freed; atoms are not counted.
//!
//! This module does no aggregation: `merge_drain_stats.py` computes counts, totals, maxima and
//! histograms from the raw lines. Each line is flushed and fsynced before `drained` returns, so a
//! killed process — or a machine crash — keeps everything written so far. The fsync costs one
//! `sync_all` per drain.
//!
//! The file is truncated by the first write of a process, so it never mixes in lines from an
//! earlier run. Later runtimes of the same process append their own lines, and `pid + runtime`
//! tell them apart.
//!
//! This is separate from `pool_stats.rs`, which describes what enters and leaves the pool: the two
//! answer different questions and are written to different files.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;
use std::time::Duration;

use super::DrainContext;

/// Assigned on a thread's first drain; reset when its runtime is destroyed.
static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);

/// Truncates the output file once per process: a file left behind by an earlier run must not mix
/// its lines into this run's data.
static FIRST_WRITE: Once = Once::new();

#[derive(Default)]
struct State {
    /// Created on this thread's first drain.
    out: Option<LineWriter<File>>,
    /// 0 until the first drain of the current runtime.
    runtime: u64,
    /// Opening or writing failed; stop retrying and stop reporting.
    failed: bool,
}

thread_local!(static STATE: RefCell<State> = RefCell::new(State::default()));

/// Records one `drain_pool` call by appending a line to the output file.
///
/// `elapsed` is `None` when the drain found nothing to release. This runs on every drain, so it
/// keeps the file open per thread and writes one line per call.
pub(crate) fn drained(context: DrainContext, elapsed: Option<Duration>, values: u64) {
    let _ = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        if s.failed {
            return;
        }

        // Open lazily: threads that never drain do not create the file.
        if s.out.is_none() {
            let path = std::env::var_os("QJS_DRAIN_STATS_OUT")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join(format!("qjs-drain-stats-{}.jsonl", std::process::id()))
                });
            log::info!("qjs-drain-stats: writing to {}", path.display());
            FIRST_WRITE.call_once(|| {
                let _ = File::create(&path);
            });
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => s.out = Some(LineWriter::new(file)),
                Err(e) => {
                    log::error!("qjs-drain-stats: can't write {}: {e}", path.display());
                    s.failed = true;
                    return;
                }
            }
        }

        if s.runtime == 0 {
            s.runtime = NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed);
        }

        let thread = std::thread::current();
        let thread = string(thread.name().unwrap_or(""));
        let ctx = string(context.name());
        let pid = std::process::id();
        let runtime = s.runtime;
        let out = s.out.as_mut().unwrap();

        let result = match elapsed {
            Some(elapsed) => {
                let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
                writeln!(
                    out,
                    "{{\"thread\":{thread},\"pid\":{pid},\"runtime\":{runtime},\"ctx\":{ctx},\
                     \"elapsed_ns\":{ns},\"values\":{values}}}"
                )
            }
            None => writeln!(
                out,
                "{{\"thread\":{thread},\"pid\":{pid},\"runtime\":{runtime},\"ctx\":{ctx},\
                 \"elapsed_ns\":null,\"values\":{values}}}"
            ),
        };
        // Flush the line out of the `LineWriter` and fsync it: the newline alone only gets the
        // line into the OS page cache, so a machine crash could still lose it. `sync_all` is an
        // fsync per drain.
        if result
            .and_then(|()| out.flush())
            .is_err()
        {
            s.failed = true;
        }
    });
}

/// Ends the current runtime on this thread: the next drain gets a new `runtime` id.
pub(crate) fn reset_runtime() {
    let _ = STATE.try_with(|s| s.borrow_mut().runtime = 0);
}

fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
