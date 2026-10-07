servo := "../servo"
target := "target"
servo_target := "../servo/target"
patch := 'patch.crates-io.mozjs.path="' + justfile_directory() + '/mozjs"'

# Regenerate the stubbed SpiderMonkey bindings, forwarding everything implemented in mozjs/src/engine.
regen:
    cd mozjs && python3 ../tools/stub_bindings.py upstream-bindgen/jsapi.rs src/sys/generated/jsapi.rs src/engine
    cd mozjs && python3 ../tools/stub_bindings.py upstream-bindgen/gluebindings.rs src/sys/generated/gluebindings.rs src/engine

# Regenerate quickjs-sys bindings from the vendored QuickJS header.
gen-bindings:
    LIBCLANG_PATH=/usr/lib/llvm-18/lib CARGO_TARGET_DIR={{target}} cargo run -p gen-bindings

test *args: regen
    CARGO_TARGET_DIR={{target}} cargo test -p mozjs {{args}}

# Build Servo's servoshell (in the quickjs-spike worktree) against the shim.
servo cmd="build": regen
    cd {{servo}} && PATH=/usr/bin:$PATH LIBCLANG_PATH=/usr/lib/llvm-18/lib CARGO_TARGET_DIR={{servo_target}} cargo {{cmd}} -p servoshell --config '{{patch}}'
