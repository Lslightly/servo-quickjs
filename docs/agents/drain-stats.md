# Drain statistics (`mozjs/src/engine/drain_stats.rs`)

`drain_stats.rs` times `Cx::drain_pool`, the call that releases the autorelease pool, and records
why it ran (`DrainContext`). It is the counterpart of `pool_stats.rs`:

- `pool_stats.rs`: **what enters and leaves the pool**, by API function.
- `drain_stats.rs`: **how long each drain takes, and which reason is expensive**.

The Rust side only produces raw records: every drain appends one JSON line. Counting, summing,
maxima and histograms are done afterwards, by the analysis script
(`servo-ohos/etc/ci/performance/merge_drain_stats.py`). The Rust side does not record the program's
total running time either; measure the whole command externally, for example with
`/usr/bin/time -f '%e' -o wall.txt ...`, and use that wall clock in the analysis tool.

## Quick start

```sh
# Build the tests with the switch, in its own target dir.
DRAIN_STATS=1 QJS_TARGET_DIR=$PWD/target/drain-stats just test

# Build an instrumented servoshell, run it, and collect the raw lines.
DRAIN_STATS=1 SERVO_TARGET_DIR=$SERVO_DIR/target/drain-stats just servo "build --features js-quickjs"
QJS_DRAIN_STATS_OUT=/tmp/drain.jsonl \
  $SERVO_DIR/target/drain-stats/debug/servoshell --headless --exit http://example.com
```

## Variables

| Variable | Read at | Meaning |
|---|---|---|
| `DRAIN_STATS` | build | `1` (or any value except empty / `0`) turns on `cfg(drain_stats)` in `mozjs/build.rs`. Without it the module and every call site compile out. |
| `QJS_DRAIN_STATS_OUT` | run | Output path. Default: `qjs-drain-stats-<pid>.jsonl` in the temp dir. Lines are appended with `O_APPEND`, so several processes may share one file. |

`DRAIN_STATS` is **compile-time only**. Setting it when running a binary does nothing. Toggling it
rebuilds `mozjs` (build.rs emits `rerun-if-env-changed`), and therefore Servo too; use a separate
target dir so switching it on and off does not rebuild everything twice.

## Output

Each drain appends one line to the output file as it happens:

```json
{"thread":"Script(1,1)","pid":1234,"runtime":1,"ctx":"native_call","elapsed_ns":1234,"values":5}
```

Fields:

- `thread`: the OS thread name (empty if unnamed).
- `pid`: the process id.
- `runtime`: a per-process counter starting at 1. It is assigned on the thread's first drain after
  the previous runtime was destroyed, so `pid` + `runtime` identify one runtime in a shared file.
  A thread may host several runtimes over time.
- `ctx`: `scope_drop`, `native_call`, `runtime_destroy`, `proxy_trap`, `job_callback`, `job_run`,
  `task`. These strings are stable; tools may match them.
- `elapsed_ns`: duration of this drain, in nanoseconds, or `null` for an empty drain (nothing to
  release; empty drains are not timed).
- `values`: pool entries freed by this drain. Atoms are not counted, so `values == 0` does not
  imply that the drain was empty.

The file is truncated by a process's first write, so data from an earlier run never mixes in. Each
line is flushed when written, so a process killed with `SIGKILL` keeps everything written so far.

## Reading the output

Use `jq` for ad-hoc queries, or `servo-ohos/etc/ci/performance/merge_drain_stats.py` to merge
several testcases into CSV.

1. Time per context:

   ```sh
   jq -s '
     group_by(.ctx)
     | map({ctx: .[0].ctx,
            calls: length,
            empty: (map(select(.elapsed_ns == null)) | length),
            total_ns: (map(.elapsed_ns // 0) | add),
            max_ns: (map(.elapsed_ns // 0) | max),
            values: (map(.values) | add)})
     | sort_by(-.total_ns)
   ' /tmp/drain.jsonl
   ```

2. One context, per runtime:

   ```sh
   jq -s '
     map(select(.ctx == "task"))
     | group_by([.pid, .runtime])
     | map({pid: .[0].pid, runtime: .[0].runtime,
            calls: length, total_ns: (map(.elapsed_ns // 0) | add)})
   ' /tmp/drain.jsonl
   ```

## Running it

- **Tests**: `DRAIN_STATS=1 QJS_TARGET_DIR=$PWD/target/drain-stats just test`. `just test` sets
  `CARGO_TARGET_DIR` itself, so use `QJS_TARGET_DIR` to move it.
- **servoshell**: build once with `DRAIN_STATS=1` (above), then set only the `QJS_*` variables when
  running. `just run <url>` accepts them too, but uses `SERVO_TARGET_DIR`, so pass the same value.
  `--exit` makes a single page exit on its own.
- **WPT**: build the instrumented servoshell, then
  `QJS_DRAIN_STATS_OUT=/tmp/wpt-drain.jsonl SERVO_TARGET_DIR=… just wpt /tmp/wpt.log dom/nodes`.
  `mach` hands the environment to the browser process (the same mechanism as `QJS_VERIFY_TRACING`,
  which `just wpt` already relies on). One `QJS_DRAIN_STATS_OUT` file collects every WPT browser
  process; separate them by `pid` + `runtime`. Browsers that `mach` kills at the end keep the lines
  written before the kill.

## Caveats

1. A killed process keeps its lines, but the records stop at the kill: the last drains never ran and
   are absent, not counted as empty.
2. Every drain writes one line; the file grows with the number of drains, not with time. A long
   benchmark that drains millions of times produces a large file. Aggregate before sharing it.
3. Each thread keeps one `LineWriter<File>` open. A write error stops further output on that thread
   (reported once on stderr); it never aborts the program.
4. Every non-empty drain pays one clock read for the timing itself. Empty drains are not timed.
5. A new runtime on the same thread gets a new `runtime` id; `reset_runtime()` runs when the runtime
   is destroyed.
6. Only `drain_pool` is measured. Time between drains is not; the frees inside the loop are.

## See also

- `mozjs/src/engine/pool_stats.rs` — pool contents and origins (`QJS_POOL_STATS`, `tools/pool_stats.py`).
- `docs/design.md`, "Ownership: counted storage plus a per-task pool" — why drains exist.
- `mozjs/src/engine/mod.rs`, `DrainContext` — the seven reasons a drain runs.
- `servo-ohos/etc/ci/performance/README-drain-stats.md` — build → collect → merge pipeline.
