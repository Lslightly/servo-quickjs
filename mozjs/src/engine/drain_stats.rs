/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Timing of `Cx::drain_pool`, built with `DRAIN_STATS=1`.
//!
//! Every drain is timed per `DrainContext` (why it ran: a scope dropped, a native call returned, a
//! task ended, …). Each kind gets a count, a total, a maximum and a histogram of the durations in
//! four buckets per power of two. Empty drains are counted separately and not timed: they are the
//! common case, and the clock reads would be most of their cost.
//!
//! This is separate from `pool_stats.rs`, which describes what enters and leaves the pool: the two
//! answer different questions and are written to different files.
//!
//! Each runtime writes one JSON line to `QJS_DRAIN_STATS_OUT` (default
//! `qjs-drain-stats-<pid>.jsonl` in the temporary directory) when it is destroyed. The file is
//! truncated by the first write of a process, so it never mixes in lines from an earlier run;
//! later runtimes of the same process append their own line, and `pid + runtime` tell them apart.
//! A process that is killed before any runtime is destroyed writes nothing. Each line has one
//! object per kind in `drains`, and its `hist` lists only the nonzero buckets, as
//! `{"lo_ns": lower bound, "count": durations in the bucket}`.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::DrainContext;

/// Four buckets per power of two, exact below 4 ns; the last covers `2^63` ns and above.
const HIST_BUCKETS: usize = 252;

/// The timing of one `DrainContext`, in nanoseconds (`u64` holds ~584 years).
struct DrainStats {
    /// Every call, including the empty ones.
    calls: u64,
    /// Calls that found nothing to release (not timed).
    empty: u64,
    /// Sum and maximum over the timed calls only.
    total_ns: u64,
    max_ns: u64,
    /// Values released, summed over the timed calls.
    values: u64,
    /// One count per duration bucket (`bucket`). Boxed to keep `State` small: it is allocated
    /// once, in `Default`, and never resized.
    hist: Box<[u64]>,
}

impl Default for DrainStats {
    fn default() -> Self {
        Self {
            calls: 0,
            empty: 0,
            total_ns: 0,
            max_ns: 0,
            values: 0,
            hist: vec![0; HIST_BUCKETS].into_boxed_slice(),
        }
    }
}

/// The bucket `ns` falls in: exact below 4, then four per power of two.
///
/// Above 4, `e` is the floor of `log2(ns)`, and the two bits below the leading one pick the
/// quarter of `[2^e, 2^(e+1))`; `- 4` leaves 0..3 for the exact values below. A bucket's width is
/// therefore 1/4 to 1/7 of its lower bound.
fn bucket(ns: u64) -> usize {
    if ns < 4 {
        return ns as usize;
    }
    let e = 63 - ns.leading_zeros() as usize;
    e * 4 + ((ns >> (e - 2)) & 0b11) as usize - 4
}

/// The smallest duration `bucket(i)` holds, in nanoseconds; the next bucket's `lo` is its end.
///
/// This must stay the inverse of `bucket`: the JSON reports these bounds, and a mismatch would
/// place a bucket's counts at the wrong time.
fn bucket_lo(i: usize) -> u64 {
    if i < 4 {
        return i as u64;
    }
    let bits = i + 4;
    let e = bits / 4;
    let sub = bits % 4;
    ((4 + sub) as u64) << (e - 2)
}

#[derive(Default)]
struct State {
    /// Time of `drain_pool`, indexed by the `DrainContext` discriminant.
    drains: [DrainStats; DrainContext::COUNT],
    /// Identifies the runtime's line in the output.
    runtime: u64,
}

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);

thread_local!(static STATE: RefCell<State> = RefCell::default());

/// Records one `drain_pool` call. `elapsed` is `None` when it found nothing to release; `values`
/// is how many pool entries it freed (atoms are not counted).
///
/// This runs on every drain, so it does no allocation and takes no locks: the histogram is
/// preallocated, and the counts live in the thread-local `State`.
pub(crate) fn drained(context: DrainContext, elapsed: Option<Duration>, values: u64) {
    println!("drained: context={:?}, elapsed={:?}, values={}", context, elapsed, values);
    let _ = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        let stats = &mut s.drains[context as usize];
        stats.calls += 1;
        let Some(elapsed) = elapsed else {
            stats.empty += 1;
            return;
        };
        let ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        stats.total_ns += ns;
        stats.max_ns = stats.max_ns.max(ns);
        stats.values += values;
        stats.hist[bucket(ns)] += 1;
    });
}

/// This thread's runtime timing as a JSON object.
pub(crate) fn json() -> String {
    let json = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        if s.runtime == 0 {
            s.runtime = NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed);
        }
        let s = &*s;
        let mut out = String::new();
        let thread = std::thread::current();
        let _ = write!(
            out,
            "{{\"thread\":{},\"pid\":{},\"runtime\":{},\"drains\":[",
            string(thread.name().unwrap_or("")),
            std::process::id(),
            s.runtime,
        );
        // Every context is listed, even an idle one: a fixed shape is easier to consume.
        for (i, context) in DRAIN_CONTEXTS.iter().enumerate() {
            let d = &s.drains[*context as usize];
            let _ = write!(
                out,
                "{}{{\"ctx\":{},\"calls\":{},\"empty\":{},\"total_ns\":{},\"max_ns\":{},\"values\":{},\"hist\":[",
                if i == 0 { "" } else { "," },
                string(context.name()),
                d.calls,
                d.empty,
                d.total_ns,
                d.max_ns,
                d.values,
            );
            // Only nonempty buckets: an idle or short-lived context stays small. `lo_ns` is the
            // bucket's lower bound; it ends where the next written bucket begins.
            let mut first = true;
            for (bucket, count) in d.hist.iter().enumerate() {
                if *count == 0 {
                    continue;
                }
                let _ = write!(
                    out,
                    "{}{{\"lo_ns\":{},\"count\":{count}}}",
                    if first { "" } else { "," },
                    bucket_lo(bucket),
                );
                first = false;
            }
            // Close the histogram array and this context's object.
            out.push_str("]}");
        }
        // Close the `drains` array and the root object.
        out.push_str("]}");
        out
    });
    json.unwrap_or_default()
}

/// Every `DrainContext`, in the order the JSON lists them. `State::drains` is indexed by the
/// discriminant instead, so only the array length (not this order) ties the two together: after
/// bumping `DrainContext::COUNT`, a missing entry here fails to compile.
const DRAIN_CONTEXTS: [DrainContext; DrainContext::COUNT] = [
    DrainContext::ScopeDrop,
    DrainContext::NativeCall,
    DrainContext::RuntimeDestroy,
    DrainContext::ProxyTrap,
    DrainContext::JobCallback,
    DrainContext::JobRun,
    DrainContext::Task,
];

/// Appends this thread's timing to the output file, and resets it: the runtime is gone.
pub(crate) fn report() {
    write();
    let _ = STATE.try_with(|s| *s.borrow_mut() = State::default());
}

/// Truncates the output file once per process: a file left behind by an earlier run must not mix
/// its lines into this run's data.
static FIRST_WRITE: Once = Once::new();

/// Writes this thread's timing to the output file.
fn write() {
    let line = json();
    if line.is_empty() {
        return;
    }
    let path = std::env::var_os("QJS_DRAIN_STATS_OUT")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("qjs-drain-stats-{}.jsonl", std::process::id()))
        });
    FIRST_WRITE.call_once(|| {
        let _ = std::fs::File::create(&path);
    });
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut file) => {
            // One write per line: the runtimes of this process share the file (O_APPEND).
            let _ = file.write_all(format!("{line}\n").as_bytes());
            eprintln!("qjs-drain-stats: wrote to {}", path.display());
        }
        Err(e) => eprintln!("qjs-drain-stats: can't write {}: {e}", path.display()),
    }
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
