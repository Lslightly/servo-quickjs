# Implementing SpiderMonkey API functions on QuickJS

Design: `docs/design.md`. Read it first.

## How a function gets implemented

- `mozjs/upstream-bindgen/*.rs` is SpiderMonkey's bindgen output for mozjs_sys 140.14.0-1. Don't edit it.
- `mozjs/build.rs` runs `mozjs/stub_bindings.py`. It turns every SpiderMonkey function into a Rust function:
  - If `mozjs/src/engine/**/*.rs` contains `pub(crate) unsafe fn NAME(`, the function forwards to it.
  - Otherwise its body panics with "`NAME` is not implemented".
- To implement `NAME`:
  1. Run `python3 tools/sig.py NAME`. It prints the exact signature.
  2. Paste it into the engine module for its area. Engine modules alias the jsapi module as `root`; types that exist only in the glue bindings (e.g. `GetSize`, `ProxyTraps`) are in `crate::sys::glue`.
  3. Write the body. Rebuilding picks it up.
- Helpers that are not SpiderMonkey functions should be `pub(crate) fn` or `pub(crate) unsafe fn` with a name that is not a SpiderMonkey function name.
- The Rust layers copied from mozjs (`mozjs/src/*.rs`, `mozjs/src/sys/*.rs`) may be changed where they depend on SpiderMonkey internals. Mark each such change with a `QuickJS:` comment.

## Rules

- **Context and realm.** `Cx::from_raw(cx)` gives the runtime state; `cx.qctx()` is the QuickJS context of the current realm. Functions without a `cx` parameter use `super::current()`.
- **Values** (`JSVal`) are QuickJS `JSValue`s (`.to_qjs()`, `JSVal::from_qjs()`). `*mut JSObject`, `*mut JSString` and `*mut JS::Symbol` are the QuickJS pointers. Use `obj_val(obj)` / `val_obj(v)`.
- **Ownership.**
  - Anything returned to Rust or written to an out-parameter must stay alive for the rest of the task:
    - `cx.adopt(v)` takes over a reference you own (e.g. a QuickJS API result);
    - `cx.retain(v)` adds one.
  - Never return a borrowed QuickJS value to Rust without one of these.
  - Values passed *into* QuickJS functions that consume a reference (`JS_SetProperty`, `JS_DefinePropertyValue`, `JS_Throw`, …) need a `JS_DupValueRT` first.
  - Free every temporary you own.
  - Atoms work the same way: `cx.adopt_atom`, `JS_FreeAtomRT`.
- **Counted storage** (`Heap<T>`, reserved slots, anything kept beyond the task) holds exactly one reference. Only such storage may be marked during `gc_mark`; marking anything else causes use-after-free.
- **Strings.**
  - Rust never sees a rope: `cx.adopt`/`cx.retain` flatten them.
  - Create strings with `JS_NewStringLatin1` / `JS_NewStringUTF16` (patch Q3) or `JS_NewStringLen` (UTF-8).
  - Read them with `JS_GetStringData`.
- **Exceptions.** On failure, return `false` / null with a QuickJS exception pending (`JS_Throw*`). A `false` without an exception means uncatchable termination.
- **Property keys** (`jsid`) map to QuickJS atoms: `id.to_atom()` (borrowed), `crate::sys::jsid::from_atom`.
- **QuickJS patches** go in `quickjs-sys/quickjs`:
  - Mark them with a `/* Servo: ... */` comment.
  - Add the declaration to `quickjs.h` and run `just gen-bindings`.
  - Commit them separately with a `quickjs:` prefix.
  - List them in the design doc's patch table.

## Testing

- `CARGO_TARGET_DIR=<your target dir> just test`.
- The mozjs 0.23.1 tests are in `mozjs/tests/` (see `mozjs/upstream-tests/README.md` for the local changes).
- Add focused tests for your area as `mozjs/tests/qjs_<area>.rs`.

## Commits

- Author `Jonathan Schwender <schwenderjonathan@gmail.com>`, with `-s`.
- Short subject (`mozjs: …` or `quickjs: …`) and a short body.
- No Co-Authored-By or AI trailers.
