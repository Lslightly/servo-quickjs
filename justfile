servo := "../servo"
target := env_var_or_default("CARGO_TARGET_DIR", "target")
servo_target := "../servo/target"
patch := 'patch.crates-io.mozjs.path="' + justfile_directory() + '/mozjs"'

# Regenerate quickjs-sys bindings from the vendored QuickJS header.
gen-bindings:
    LIBCLANG_PATH=/usr/lib/llvm-18/lib CARGO_TARGET_DIR={{target}} cargo run -p gen-bindings

test *args:
    CARGO_TARGET_DIR={{target}} cargo test -p mozjs {{args}}

# Build Servo's servoshell (in the quickjs-spike worktree) against the shim.
servo cmd="build":
    cd {{servo}} && PATH=/usr/bin:$PATH LIBCLANG_PATH=/usr/lib/llvm-18/lib CARGO_TARGET_DIR={{servo_target}} cargo {{cmd}} -p servoshell --config '{{patch}}'

# Run the shim-backed servoshell headless on a page (`just servo "build --features js-quickjs"` first).
run page *args:
    {{servo_target}}/debug/servoshell --headless --exit {{args}} {{page}}

# Run WPT tests (e.g. `dom/nodes`) with the shim-backed servoshell and the tracing verifier.
wpt log *tests:
    cd {{servo}} && QJS_VERIFY_TRACING=1 PATH=/usr/bin:$PATH ./mach test-wpt --bin {{servo_target}}/debug/servoshell --processes 2 --log-raw {{log}} {{tests}}
