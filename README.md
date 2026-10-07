# servo-quickjs

QuickJS as an alternative JavaScript engine for Servo. Spike.

- `quickjs-sys`: Bellard's QuickJS, vendored in `quickjs-sys/quickjs` (patched locally) with
  pregenerated bindings. Regenerate with `LIBCLANG_PATH=/usr/lib/llvm-18/lib cargo run -p gen-bindings`.
- `mozjs`: the `mozjs` 0.23.1 API implemented on QuickJS. SpiderMonkey's bindgen types are kept;
  its functions are Rust stubs (`tools/stub_bindings.py` over `mozjs/upstream-bindgen/*.rs`) that are
  replaced by QuickJS implementations.

Build Servo against it without touching Servo's `Cargo.toml`:

```sh
cargo build -p servoshell --config 'patch.crates-io.mozjs.path="/path/to/servo-quickjs/mozjs"'
```

The version in `mozjs/Cargo.toml` must equal the `mozjs` version Servo pins.
