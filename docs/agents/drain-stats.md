# Drain statistics (`mozjs/src/engine/drain_stats.rs`)

`drain_stats.rs` times `Cx::drain_pool`, the call that releases the autorelease pool, and records
why it ran (`DrainContext`). It is the counterpart of `pool_stats.rs`:

- `pool_stats.rs`: **what enters and leaves the pool**, by API function.
- `drain_stats.rs`: **how long each drain takes, and which reason is expensive**.

The Rust side does not record the program's total running time. To compute a drain's share of
execution time, measure the whole command externally, for example with
`/usr/bin/time -f '%e' -o wall.txt ...`, and use that wall clock in the analysis tool.

## Quick start

```sh
# Build the tests with the switch, in its own target dir.
DRAIN_STATS=1 QJS_TARGET_DIR=$PWD/target/drain-stats just test

# Build an instrumented servoshell, run it, and collect one JSON line per thread.
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

Each thread's runtime appends **one JSON object** to the output file when the runtime is destroyed,
after `JS_FreeRuntime` and its finalizers. The thread then prints
`qjs-drain-stats: appended to <path>` on stderr.

A killed process (including `SIGKILL` from `timeout`) writes nothing, because it never reaches
runtime destruction. Use this mode only for processes that exit on their own.

```json
{
  "thread": "Script(1,1)",
  "pid": 1234,
  "runtime": 1,
  "drains": [
    {
      "ctx": "task",
      "calls": 10,
      "empty": 9,
      "total_ns": 300,
      "max_ns": 300,
      "values": 1,
      "hist": [{"lo_ns": 256, "count": 1}]
    }
  ]
}
```

Root fields:

- `thread`: the OS thread name (empty if unnamed).
- `pid`, `runtime`: `runtime` is a per-process counter starting at 1, assigned on the runtime's
  first write. A thread may host several runtimes over time; each destroyed runtime gets its own
  line. Use `pid` + `runtime` to identify one runtime in a shared file.
- `drains`: always all seven contexts, in a fixed order.

Per-context fields:

- `ctx`: `scope_drop`, `native_call`, `runtime_destroy`, `proxy_trap`, `job_callback`, `job_run`,
  `task`. These strings are stable; tools may match them.
- `calls`: every drain of this kind.
- `empty`: drains that found nothing to release. Not timed.
- `total_ns`, `max_ns`, `values`, `hist`: over the **timed (non-empty) drains only**.
  - `calls - empty == sum(hist[].count)`.
  - `values` counts pool entries freed. An atom-only drain is timed but adds `0`, so `values == 0`
    does not imply `empty`.
- `hist`: only non-zero buckets. `lo_ns` is the bucket's lower bound; the bucket ends where the next
  written bucket begins. Buckets are exact below 4 ns, then four per power of two, so a bucket is
  1/4 to 1/7 of its `lo_ns` wide.

## Reading the output

Use `jq` for ad-hoc queries, or `servo-ohos/etc/ci/performance/merge_drain_stats.py` to merge
several testcases into CSV.

1. Time per context, summed over every runtime in the file:

   ```sh
   jq -s '
     map(.drains[])
     | group_by(.ctx)
     | map({ctx: .[0].ctx,
            calls: (map(.calls) | add),
            empty: (map(.empty) | add),
            total_ns: (map(.total_ns) | add),
            max_ns: (map(.max_ns) | max),
            values: (map(.values) | add)})
     | sort_by(-.total_ns)
   ' /tmp/drain.jsonl
   ```

2. Merge the histograms of one context:

   ```sh
   jq -s '
     map(.drains[] | select(.ctx == "task")) | map(.hist) | add
     | group_by(.lo_ns) | map({lo_ns: .[0].lo_ns, count: (map(.count) | add)})
   ' /tmp/drain.jsonl
   ```

## Running it

- **Tests**: `DRAIN_STATS=1 QJS_TARGET_DIR=$PWD/target/drain-stats just test`. `just test` sets
  `CARGO_TARGET_DIR` itself, so use `QJS_TARGET_DIR` to move it.
- **servoshell**: build once with `DRAIN_STATS=1` (above), then set only the `QJS_*` variables when
  running. `just run <url>` accepts them too, but uses `SERVO_TARGET_DIR`, so pass the same value.
  Let the process exit normally; `--exit` makes a single page do that.
- **WPT**: build the instrumented servoshell, then
  `QJS_DRAIN_STATS_OUT=/tmp/wpt-drain.jsonl SERVO_TARGET_DIR=… just wpt /tmp/wpt.log dom/nodes`.
  `mach` hands the environment to the browser process (the same mechanism as `QJS_VERIFY_TRACING`,
  which `just wpt` already relies on). One `QJS_DRAIN_STATS_OUT` file collects every WPT browser
  process; separate them by `pid` + `runtime`. Browsers that `mach` kills at the end write nothing.
- **In process**: `mozjs::drain_stats_json()` returns the current thread's runtime as JSON.
  Compiled only with `DRAIN_STATS`.

## Caveats

1. A killed process has no final line. Long benchmarks that are stopped with `timeout -s KILL`
   produce no data; they must be allowed to exit.
2. The line is written from the thread that destroyed the runtime, and opens/appends/closes the
   file once. There is no periodic snapshot and no per-drain clock read for snapshot scheduling.
3. Every non-empty drain pays one clock read for the timing itself. Empty drains are not timed.
4. `report()` resets the thread-local state after writing, so a later runtime on the same thread
   gets a new `runtime` id and starts from zero.
5. Only `drain_pool` is measured. Time between drains is not; the frees inside the loop are.

## See also

- `mozjs/src/engine/pool_stats.rs` — pool contents and origins (`QJS_POOL_STATS`, `tools/pool_stats.py`).
- `docs/design.md`, "Ownership: counted storage plus a per-task pool" — why drains exist.
- `mozjs/src/engine/mod.rs`, `DrainContext` — the seven reasons a drain runs.
