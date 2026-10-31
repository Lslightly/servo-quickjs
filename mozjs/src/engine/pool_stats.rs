/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Autorelease pool statistics, built with `QJS_POOL_STATS=1`: which API functions put values in
//! the pool, and what became of each value by the time the pool drained.
//!
//! `Cx::drain_pool` is also timed, per `DrainContext` (why it ran: a scope dropped, a native call
//! returned, a task ended, …). Each kind gets a count, a total, a maximum and a histogram of the
//! durations in four buckets per power of two (`drained`). Empty drains are counted separately
//! and not timed: they are the common case, and the clock reads would be most of their cost.
//!
//! - **rooted**: a `Rooted` or handle write took the value (`retain_rooted`);
//! - **kept**: not rooted, but something else held it when the pool drained (a `Heap`, a
//!   `Dom<T>`, a JS object);
//! - **dropped**: not rooted, and the pool held its last count.
//!
//! For values returned as raw GC pointers, every `QJS_POOL_STATS_SAMPLE`-th (default 100, 0 for
//! none) records a backtrace, so that the Rust code using them unrooted can be found. Each runtime
//! appends a JSON line to `QJS_POOL_STATS_OUT` (default `qjs-pool-stats-<pid>.jsonl` in the
//! temporary directory) when it is destroyed, and a snapshot every 10 s before that, for processes
//! that are killed (WPT); `tools/pool_stats.py` keeps the last line of each runtime.
//!
//! With `QJS_POOL_STATS_FLAME` set (a path, empty for the temporary directory), every
//! `QJS_POOL_STATS_FLAME_SAMPLE`-th (default 1000, 0 for none) push also records a backtrace, and
//! a folded stack file (`qjs-pool-flame-<pid>.folded`) is rewritten at each snapshot:
//!
//! ```text
//! main;servo::script::run_script;mozjs::…;result 17
//! ```
//!
//! The frames are the Rust code that pushed the value — for `adopt`/`retain`/`retain_rooted`
//! alike, since they all push through `Cx::push_pool` — and the last one is the kind. That is the
//! format `tracing-flame` writes and `inferno-flamegraph` reads:
//!
//! ```sh
//! inferno-flamegraph qjs-pool-flame-<pid>.folded > pool.svg
//! ```

use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::panic::Location;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use servo_quickjs as q;

use super::{DrainContext, Pooled as Kind};

const KINDS: [&str; 4] = ["result", "borrowed", "root", "deferred"];

const CONTEXTS: [DrainContext; DrainContext::COUNT] = [
    DrainContext::ScopeDrop,
    DrainContext::NativeCall,
    DrainContext::RuntimeDestroy,
    DrainContext::ProxyTrap,
    DrainContext::JobCallback,
    DrainContext::JobRun,
    DrainContext::Task,
];

struct Entry {
    api: &'static str,
    raw: bool,
    /// Where the value was pooled, for values pooled outside any API function.
    site: Option<&'static Location<'static>>,
    kind: Kind,
    fresh: bool,
    rooted: bool,
    backtrace: Option<Backtrace>,
}

#[derive(Default)]
struct Counts {
    total: u64,
    fresh: u64,
    rooted: u64,
    kept: u64,
    dropped: u64,
}

type Key = (&'static str, bool, Option<&'static Location<'static>>, Kind);

/// Four buckets per power of two, exact below 4 ns; the last covers `2^63` ns and above.
const HIST_BUCKETS: usize = 252;

struct DrainStats {
    calls: u64,
    /// Calls that found nothing to release (not timed).
    empty: u64,
    total_ns: u64,
    max_ns: u64,
    /// Values released, summed over the timed calls.
    values: u64,
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
fn bucket(ns: u64) -> usize {
    if ns < 4 {
        return ns as usize;
    }
    let e = 63 - ns.leading_zeros() as usize;
    e * 4 + ((ns >> (e - 2)) & 0b11) as usize - 4
}

/// The smallest duration `bucket(i)` holds, in nanoseconds.
fn bucket_lo(i: usize) -> u64 {
    if i < 4 {
        return i as u64;
    }
    let bits = i + 4;
    let e = bits / 4;
    let sub = bits % 4;
    ((4 + sub) as u64) << (e - 2)
}

/// Records one `drain_pool` call. `elapsed` is `None` when it found nothing to release; `values`
/// is how many pool entries it freed (atoms are not counted).
pub(crate) fn drained(context: DrainContext, elapsed: Option<Duration>, values: u64) {
    let _ = STATE.try_with(|s| {
        let stats = &mut s.borrow_mut().drains[context as usize];
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

#[derive(Default)]
struct State {
    /// The API function running (its name, and whether it returns a raw GC pointer).
    api: Option<(&'static str, bool)>,
    /// One entry per value in `Cx::pool`, in the same order.
    entries: Vec<Entry>,
    /// The newest entry of each GC thing not rooted yet.
    unrooted: HashMap<usize, usize>,
    /// The number of entries of each GC thing.
    pooled: HashMap<usize, i32>,
    counts: HashMap<Key, Counts>,
    /// Sampled backtraces of unrooted raw results, not symbolized yet.
    samples: Vec<(&'static str, &'static str, Backtrace)>,
    /// The symbolized samples: how many per function, fate and calling frames.
    callers: HashMap<(&'static str, &'static str, Vec<String>), u64>,
    /// Sampled backtraces of pushes, not symbolized yet, with the kind they pushed.
    flame_samples: Vec<(Kind, Backtrace)>,
    /// The symbolized flame samples: how many per kind and calling frames.
    flame: HashMap<(Kind, Vec<String>), u64>,
    flame_every: Option<u64>,
    since_flame: u64,
    pushes: [u64; 4],
    /// Time of `drain_pool`, indexed by `DrainContext`.
    drains: [DrainStats; DrainContext::COUNT],
    tasks: u64,
    max_len: usize,
    sum_len: u64,
    sample_every: Option<u64>,
    since_sample: u64,
    /// Identifies the runtime's lines in the output.
    runtime: u64,
    last_snapshot: Option<Instant>,
}

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);

thread_local!(static STATE: RefCell<State> = RefCell::default());

/// Restores the API function that was running when it was created.
pub(crate) struct ApiScope(Option<(&'static str, bool)>);

/// Records that the API function `name` runs until the returned scope is dropped.
#[inline]
pub(crate) fn enter(name: &'static str, returns_raw_pointer: bool) -> ApiScope {
    ApiScope(STATE.with(|s| s.borrow_mut().api.replace((name, returns_raw_pointer))))
}

impl Drop for ApiScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        let _ = STATE.try_with(|s| s.borrow_mut().api = previous);
    }
}

fn key(v: q::JSValue) -> usize {
    v.ptr() as usize
}

impl State {
    fn sample(&mut self) -> bool {
        let every = *self.sample_every.get_or_insert_with(|| {
            std::env::var("QJS_POOL_STATS_SAMPLE")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(100)
        });
        if every == 0 {
            return false;
        }
        self.since_sample += 1;
        if self.since_sample < every {
            return false;
        }
        self.since_sample = 0;
        true
    }

    /// Whether this push should record a backtrace for the flame graph. Off unless
    /// `QJS_POOL_STATS_FLAME` names an output.
    fn flame_sample(&mut self) -> bool {
        let every = *self.flame_every.get_or_insert_with(|| {
            if std::env::var_os("QJS_POOL_STATS_FLAME").is_none() {
                return 0;
            }
            std::env::var("QJS_POOL_STATS_FLAME_SAMPLE")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1000)
        });
        if every == 0 {
            return false;
        }
        self.since_flame += 1;
        if self.since_flame < every {
            return false;
        }
        self.since_flame = 0;
        true
    }
}

/// Records a value pushed onto the pool; `v` has a reference count.
pub(crate) fn pushed(v: q::JSValue, kind: Kind, site: &'static Location<'static>) {
    let _ = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        s.pushes[kind as usize] += 1;
        let (api, raw) = s.api.unwrap_or(("", false));
        let index = s.entries.len();
        let k = key(v);
        *s.pooled.entry(k).or_default() += 1;
        match kind {
            Kind::Root => {
                if let Some(i) = s.unrooted.remove(&k) {
                    s.entries[i].rooted = true;
                }
            }
            Kind::Result | Kind::Borrowed => {
                s.unrooted.insert(k, index);
            }
            Kind::Deferred => {}
        }
        let fresh = kind == Kind::Result && unsafe { q::JS_GetRefCount(v) } == 1;
        let backtrace = (raw && kind == Kind::Result && s.sample()).then(Backtrace::force_capture);
        // `adopt_as`, `retain_as` and `release_object` all push through here, so one capture
        // covers all three, with the kind telling them apart.
        if s.flame_sample() {
            let backtrace = Backtrace::force_capture();
            s.flame_samples.push((kind, backtrace));
        }
        s.entries.push(Entry {
            api,
            raw,
            site: api.is_empty().then_some(site),
            kind,
            fresh,
            rooted: false,
            backtrace,
        });
    });
}

/// Records the value popped off the pool, before the pool releases it.
pub(crate) fn popped(v: q::JSValue) {
    let _ = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        let Some(entry) = s.entries.pop() else {
            return;
        };
        let k = key(v);
        let in_pool = s.pooled.get(&k).copied().unwrap_or(1);
        if in_pool <= 1 {
            s.pooled.remove(&k);
        } else {
            s.pooled.insert(k, in_pool - 1);
        }
        if s.unrooted.get(&k) == Some(&s.entries.len()) {
            s.unrooted.remove(&k);
        }
        if matches!(entry.kind, Kind::Root | Kind::Deferred) {
            return;
        }
        let fate = if entry.rooted {
            "rooted"
        } else if unsafe { q::JS_GetRefCount(v) } > in_pool {
            "kept"
        } else {
            "dropped"
        };
        let counts = s
            .counts
            .entry((entry.api, entry.raw, entry.site, entry.kind))
            .or_default();
        counts.total += 1;
        counts.fresh += entry.fresh as u64;
        match fate {
            "rooted" => counts.rooted += 1,
            "kept" => counts.kept += 1,
            _ => counts.dropped += 1,
        }
        if let Some(backtrace) = entry.backtrace {
            if fate != "rooted" {
                s.samples.push((entry.api, fate, backtrace));
            }
        }
    });
}

/// Records the length of the pool when a task drains it completely.
pub(crate) fn task_drained(len: usize) {
    let snapshot = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        s.tasks += 1;
        s.max_len = s.max_len.max(len);
        s.sum_len += len as u64;
        let now = Instant::now();
        let due = s
            .last_snapshot
            .is_none_or(|last| now - last >= SNAPSHOT_INTERVAL);
        if due {
            s.last_snapshot = Some(now);
        }
        due
    });
    if snapshot == Ok(true) {
        write(false);
    }
}

/// The statistics of this thread's runtime as a JSON object; `final_` once the runtime is gone.
pub(crate) fn json(final_: bool) -> String {
    let json = STATE.try_with(|s| {
        let s = &mut *s.borrow_mut();
        if s.runtime == 0 {
            s.runtime = NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed);
        }
        for (api, fate, backtrace) in std::mem::take(&mut s.samples) {
            *s.callers.entry((api, fate, callers(&backtrace))).or_default() += 1;
        }
        for (kind, backtrace) in std::mem::take(&mut s.flame_samples) {
            *s.flame.entry((kind, frames(&backtrace))).or_default() += 1;
        }
        let s = &*s;
        let mut out = String::new();
        let thread = std::thread::current();
        let _ = write!(
            out,
            "{{\"thread\":{},\"pid\":{},\"runtime\":{},\"final\":{final_},\"sample_every\":{},\"tasks\":{},\"max_len\":{},\"sum_len\":{},\"pushes\":{{",
            string(thread.name().unwrap_or("")),
            std::process::id(),
            s.runtime,
            s.sample_every.unwrap_or(0),
            s.tasks,
            s.max_len,
            s.sum_len,
        );
        for (i, kind) in KINDS.iter().enumerate() {
            let _ = write!(
                out,
                "{}\"{kind}\":{}",
                if i == 0 { "" } else { "," },
                s.pushes[i]
            );
        }
        out.push_str("},\"apis\":[");
        for (i, ((api, raw, site, kind), c)) in s.counts.iter().enumerate() {
            let site = site.map(|l| format!("{}:{}", l.file(), l.line()));
            let _ = write!(
                out,
                "{}{{\"api\":{},\"raw\":{raw},\"site\":{},\"kind\":\"{}\",\"total\":{},\"fresh\":{},\"rooted\":{},\"kept\":{},\"dropped\":{}}}",
                if i == 0 { "" } else { "," },
                string(api),
                site.as_deref().map_or("null".to_owned(), string),
                KINDS[*kind as usize],
                c.total,
                c.fresh,
                c.rooted,
                c.kept,
                c.dropped,
            );
        }
        out.push_str("],\"samples\":[");
        for (i, ((api, fate, frames), n)) in s.callers.iter().enumerate() {
            let frames: Vec<String> = frames.iter().map(|f| string(f)).collect();
            let _ = write!(
                out,
                "{}{{\"api\":{},\"fate\":\"{fate}\",\"frames\":[{}],\"count\":{n}}}",
                if i == 0 { "" } else { "," },
                string(api),
                frames.join(","),
            );
        }
        out.push_str("],\"drains\":[");
        for (i, context) in CONTEXTS.iter().enumerate() {
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
            out.push('}');
        }
        out.push_str("]}");
        out
    });
    json.unwrap_or_default()
}

/// Appends this thread's statistics to the output file, and resets them: the runtime is gone.
pub(crate) fn report() {
    write(true);
    let _ = STATE.try_with(|s| *s.borrow_mut() = State::default());
}

/// Appends this thread's statistics to the output file.
fn write(final_: bool) {
    let line = json(final_);
    if line.is_empty() {
        return;
    }
    let path = std::env::var_os("QJS_POOL_STATS_OUT")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("qjs-pool-stats-{}.jsonl", std::process::id()))
        });
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut file) => {
            // One write per line: processes share the file (O_APPEND).
            let _ = file.write_all(format!("{line}\n").as_bytes());
            if final_ {
                eprintln!("qjs-pool-stats: appended to {}", path.display());
            }
        }
        Err(e) => eprintln!("qjs-pool-stats: can't write {}: {e}", path.display()),
    }
    write_flame();
}

/// Rewrites the folded stacks that `inferno-flamegraph` reads, truncating: they hold every push
/// so far. Only when `QJS_POOL_STATS_FLAME` is set.
fn write_flame() {
    let Some(path) = std::env::var_os("QJS_POOL_STATS_FLAME").map(|p| {
        if p.is_empty() {
            std::env::temp_dir().join(format!("qjs-pool-flame-{}.folded", std::process::id()))
        } else {
            Path::new(&p).to_owned()
        }
    }) else {
        return;
    };
    let flame = flame();
    match std::fs::write(&path, &flame) {
        Ok(()) if !flame.is_empty() => {
            eprintln!("qjs-pool-stats: flame graph data in {}", path.display());
        }
        Ok(()) => {}
        Err(e) => eprintln!("qjs-pool-stats: can't write {}: {e}", path.display()),
    }
}

/// The sampled pushes as folded stacks, one per line: the Rust frames that pushed the value,
/// oldest first, then the kind.
fn flame() -> String {
    let flame = STATE.try_with(|s| {
        let s = &*s.borrow();
        let mut lines: Vec<(String, &str, u64)> = s
            .flame
            .iter()
            .map(|((kind, frames), n)| (frames.join(";"), KINDS[*kind as usize], *n))
            .collect();
        lines.sort_unstable();
        let mut out = String::new();
        for (frames, kind, n) in lines {
            let _ = writeln!(out, "{frames};{kind} {n}");
        }
        out
    });
    flame.unwrap_or_default()
}

/// The first frames of `backtrace` outside the shim and the standard library: the Rust code that
/// called the API function. Frames are told apart by their symbol, and by their source location
/// (the shim's repository, its generated bindings, the standard library) for generic code and
/// closures whose symbols name another crate.
fn callers(backtrace: &Backtrace) -> Vec<String> {
    let mut frames = symbols(backtrace);
    frames.truncate(3);
    frames
}

/// Every frame of `backtrace` outside the shim and the standard library, innermost first: a
/// folded stack from the Rust code that pushed the value out to its entry point.
fn frames(backtrace: &Backtrace) -> Vec<String> {
    let mut frames = symbols(backtrace);
    frames.reverse();
    frames
}

fn symbols(backtrace: &Backtrace) -> Vec<String> {
    const SKIP_SYMBOLS: &[&str] = &[
        "std::",
        "core::",
        "alloc::",
        "mozjs::",
        "<std::",
        "<core::",
        "<alloc::",
        "<mozjs::",
        "__rust",
        "rust_begin",
    ];
    let shim = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| format!("{}/", root.display()))
        .unwrap_or_default();
    let skip_location = |l: &str| {
        (!shim.is_empty() && l.starts_with(&shim))
            || l.starts_with(env!("OUT_DIR"))
            || l.starts_with("/rustc/")
            || l.contains("/rustlib/src/rust/library/")
    };
    let text = backtrace.to_string();
    let mut frames = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((_, symbol)) = line.trim_start().split_once(": ") else {
            continue;
        };
        let location = lines
            .peek()
            .and_then(|next| next.trim_start().strip_prefix("at "))
            .map(str::to_owned);
        if location.is_some() {
            lines.next();
        }
        let symbol = strip_hash(symbol);
        let skip = SKIP_SYMBOLS.iter().any(|prefix| symbol.starts_with(prefix))
            || location.as_deref().is_some_and(skip_location);
        if skip {
            continue;
        }
        frames.push(match location {
            Some(location) => format!("{symbol} ({location})"),
            None => symbol.to_owned(),
        });
    }
    frames
}

/// `name::h0123456789abcdef` without its hash.
fn strip_hash(symbol: &str) -> &str {
    match symbol.rsplit_once("::h") {
        Some((name, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name
        }
        _ => symbol,
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
